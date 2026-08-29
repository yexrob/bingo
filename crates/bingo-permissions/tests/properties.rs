//! The invariants the gate keeps for every input, not only for the ones a test
//! remembered to write down. Each one is a way of failing closed.

use std::path::{Path, PathBuf};

use bingo_permissions::decide::{Request, decide};
use bingo_permissions::rule::{Call, Rule, Rules};
use bingo_permissions::{Mode, path, split};
use bingo_sdk::{Decision, Reason, Subject, ToolTraits};
use proptest::prelude::*;

const CWD: &str = "/work/project";
const HOME: &str = "/home/user";

struct Case {
    name: &'static str,
    subjects: Vec<Subject>,
    traits: ToolTraits,
    confirm: Option<String>,
    mode: Mode,
    rules: Rules,
}

impl Case {
    fn new(name: &'static str, subjects: Vec<Subject>) -> Self {
        Self {
            name,
            subjects,
            traits: acting_traits(),
            confirm: None,
            mode: Mode::Default,
            rules: Rules::default(),
        }
    }

    fn decide(&self) -> Decision {
        decide(
            &Request {
                call: Call {
                    name: self.name,
                    subjects: &self.subjects,
                },
                traits: &self.traits,
                confirm: self.confirm.as_deref(),
                mode: self.mode,
                cwd: Path::new(CWD),
                home: Some(Path::new(HOME)),
                extra_dirs: &[],
            },
            &self.rules,
        )
    }

    fn allowed(&self) -> bool {
        matches!(self.decide(), Decision::Allow { .. })
    }

    fn asks(&self) -> bool {
        matches!(self.decide(), Decision::Ask { .. })
    }

    fn denied(&self) -> bool {
        matches!(self.decide(), Decision::Deny { .. })
    }

    /// Allowed because a rule named this call, not because the mode did.
    fn allowed_by_rule(&self) -> bool {
        matches!(
            self.decide(),
            Decision::Allow {
                reason: Reason::Rule { .. }
            }
        )
    }
}

/// A tool that acts: trusted traits, but nothing the gate lets through unasked.
fn acting_traits() -> ToolTraits {
    ToolTraits {
        trusted: true,
        ..ToolTraits::default()
    }
}

fn table(lines: &[String]) -> Vec<Rule> {
    lines.iter().filter_map(|line| Rule::parse(line)).collect()
}

fn word() -> impl Strategy<Value = &'static str> {
    prop::sample::select(vec![
        "ls", "rm", "git", "cargo", "echo", "cat", "-la", "-rf", "/tmp", "x.txt", "--locked", "~",
    ])
}

fn simple_command() -> impl Strategy<Value = String> {
    prop::collection::vec(word(), 1..5).prop_map(|words| words.join(" "))
}

fn compound_command() -> impl Strategy<Value = String> {
    (
        simple_command(),
        prop::sample::select(vec!["&&", "||", ";", "|", "&", "\n"]),
        simple_command(),
    )
        .prop_map(|(head, operator, tail)| format!("{head} {operator} {tail}"))
}

/// Commands the grammar reports an `ERROR` or `MISSING` node for.
fn unreadable_command() -> impl Strategy<Value = String> {
    (
        simple_command(),
        prop::sample::select(vec!["\"", "'", "$(", "`", "((", "&&", "|", "<("]),
    )
        .prop_map(|(head, tail)| format!("{head} {tail}"))
        .prop_filter("the parse must really carry an error", |command| {
            !split::split(command).is_parsed()
        })
}

fn any_command() -> impl Strategy<Value = String> {
    prop_oneof![simple_command(), compound_command(), unreadable_command()]
}

fn any_subject() -> impl Strategy<Value = Subject> {
    prop_oneof![
        any_command().prop_map(|command| Subject::Command { command }),
        path_text().prop_map(|text| Subject::Path {
            path: PathBuf::from(text)
        }),
        Just(Subject::Url {
            url: "https://example.com/x".to_string()
        }),
        Just(Subject::Name {
            name: "review-pr".to_string()
        }),
    ]
}

fn any_mode() -> impl Strategy<Value = Mode> {
    prop::sample::select(vec![
        Mode::Default,
        Mode::AcceptEdits,
        Mode::Plan,
        Mode::BypassPermissions,
        Mode::DontAsk,
    ])
}

fn rule_line() -> impl Strategy<Value = String> {
    prop::sample::select(vec![
        "Bash",
        "Bash(*)",
        "Bash(ls)",
        "Bash(ls:*)",
        "Bash(rm:*)",
        "Bash(git:*)",
        "Bash(cargo test:*)",
        "Bash(echo:*)",
        "Bash(cat:*)",
        "Write",
        "Write(/work/project/)",
        "Read(/tmp/)",
        "Edit(/work/**)",
    ])
    .prop_map(String::from)
}

fn rule_table() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec(rule_line(), 0..4)
}

fn any_traits() -> impl Strategy<Value = ToolTraits> {
    (any::<bool>(), any::<bool>(), any::<bool>(), any::<bool>()).prop_map(
        |(read_only, edit, destructive, trusted)| ToolTraits {
            read_only,
            edit,
            destructive,
            trusted,
            ..ToolTraits::default()
        },
    )
}

fn path_text() -> impl Strategy<Value = String> {
    prop_oneof![
        prop::collection::vec(
            prop::sample::select(vec!["a", "b", "..", ".", "~", "x.rs", ""]),
            0..6
        )
        .prop_map(|parts| parts.join("/")),
        "[a-zA-Z0-9~./ ]{0,24}",
    ]
}

proptest! {
    /// A parse that carried an error hides what the shell would run, so no rule
    /// covers it. `bypassPermissions` is excluded: it allows by definition, and
    /// the traits are those of a tool that acts, since a read-only tool runs on
    /// its traits rather than on any reading of its argument.
    #[test]
    fn an_unreadable_command_is_never_allowed(
        command in unreadable_command(),
        allow in rule_table(),
        mode in any_mode().prop_filter("bypass allows by definition", |mode| {
            *mode != Mode::BypassPermissions
        }),
    ) {
        let mut case = Case::new("Bash", vec![Subject::Command { command }]);
        case.mode = mode;
        case.rules.allow = table(&allow);
        prop_assert!(!case.allowed());
    }

    /// An allow rule reads more narrowly than the same line in the deny table,
    /// so whatever an allow rule covers, the same line denies — in every mode,
    /// because the deny table is read before the mode is.
    #[test]
    fn a_deny_rule_beats_the_same_line_in_the_allow_table(
        command in any_command(),
        line in rule_line(),
        mode in any_mode(),
    ) {
        let subject = || vec![Subject::Command { command: command.clone() }];
        let mut allowing = Case::new("Bash", subject());
        allowing.mode = mode;
        allowing.rules.allow = table(std::slice::from_ref(&line));

        let mut denying = Case::new("Bash", subject());
        denying.mode = mode;
        denying.rules.allow = table(std::slice::from_ref(&line));
        denying.rules.deny = table(std::slice::from_ref(&line));

        prop_assert!(!allowing.allowed_by_rule() || denying.denied());
    }

    /// `plan` is a ceiling: a rule written for another mode does not turn
    /// planning into doing.
    #[test]
    fn plan_never_allows_a_tool_that_is_not_read_only(
        subject in any_subject(),
        traits in any_traits(),
        allow in rule_table(),
    ) {
        let mut case = Case::new("Bash", vec![subject]);
        case.traits = ToolTraits { read_only: false, ..traits };
        case.mode = Mode::Plan;
        case.rules.allow = table(&allow);
        prop_assert!(!case.allowed());
    }

    /// The directories that decide what the tools may do next are never written
    /// to unasked, whatever the mode and whatever the allow table says.
    #[test]
    fn a_write_into_a_sensitive_path_asks_under_bypass(
        dir in prop::sample::select(vec![".git", ".claude", ".vscode", ".idea"]),
        leaf in prop::sample::select(vec!["config", "a/b", "x.json"]),
        allow in rule_table(),
    ) {
        let mut case = Case::new("Write", vec![Subject::Path {
            path: PathBuf::from(format!("{CWD}/{dir}/{leaf}")),
        }]);
        case.traits = ToolTraits::edit();
        case.mode = Mode::BypassPermissions;
        case.rules.allow = table(&allow);
        prop_assert!(case.asks());
    }

    /// `dontAsk` says nobody is there to answer; a prompt it cannot show is a
    /// denial, never a silent yes.
    #[test]
    fn dont_ask_never_asks(
        subject in any_subject(),
        traits in any_traits(),
        confirm in prop::option::of("[a-z ]{1,20}"),
        allow in rule_table(),
        deny in rule_table(),
        ask in rule_table(),
    ) {
        let mut case = Case::new("Bash", vec![subject]);
        case.traits = traits;
        case.confirm = confirm;
        case.mode = Mode::DontAsk;
        case.rules = Rules { allow: table(&allow), deny: table(&deny), ask: table(&ask) };
        prop_assert!(!case.asks());
    }

    /// A rule and a call are compared as normalised text, so normalising has to
    /// be a fixed point or the comparison depends on how often it ran.
    #[test]
    fn normalising_a_path_twice_changes_nothing(raw in path_text()) {
        let cwd = Path::new(CWD);
        let home = Path::new(HOME);
        let once = path::normalize(&raw, cwd, Some(home));
        prop_assert_eq!(path::normalize(&once, cwd, Some(home)), once);
    }
}

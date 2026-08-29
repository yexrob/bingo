//! One decision, seven steps, one function each.
//!
//! 1. the tool's own confirmation — this call is a person's to take
//! 2. a deny rule
//! 3. a write into a sensitive directory
//! 4. `bypassPermissions`
//! 5. an allow rule
//! 6. an ask rule
//! 7. what the mode does when no rule decided
//!
//! Steps 1 and 3 stand before the mode, so no mode and no allow rule can
//! silence them. Two modes then bound whatever the ladder said: `plan` may not
//! act at all and `dontAsk` has nobody to ask, so each turns what it cannot
//! honour into a denial. A mode that cannot answer never answers yes.

use std::path::{Path, PathBuf};

use bingo_sdk::{Decision, Reason, Subject, ToolTraits};

use crate::mode::Mode;
use crate::rule::{Call, MatchContext, MatchMode, Rule, Rules, first_match};
use crate::{path, scope};

/// Everything the ladder reads about one call.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    pub call: Call<'a>,
    pub traits: &'a ToolTraits,
    /// The tool's own reason that only a person may decide.
    pub confirm: Option<&'a str>,
    pub mode: Mode,
    pub cwd: &'a Path,
    pub home: Option<&'a Path>,
    /// Directories `acceptEdits` treats as part of the working tree.
    pub extra_dirs: &'a [PathBuf],
}

pub fn decide(req: &Request<'_>, rules: &Rules) -> Decision {
    let decision = with_scope(req, rules, steps(req, rules));
    limit_by_mode(req, decision)
}

fn steps(req: &Request<'_>, rules: &Rules) -> Decision {
    // An ask rule is a person saying "always check with me": it outranks the
    // allow rules and survives bypassPermissions, like confirm and sensitive
    // paths do. Only a deny rule sits above it.
    step_confirm(req)
        .or_else(|| step_deny_rules(req, rules))
        .or_else(|| step_sensitive_path(req))
        .or_else(|| step_ask_rules(req, rules))
        .or_else(|| step_bypass(req))
        .or_else(|| step_allow_rules(req, rules))
        .unwrap_or_else(|| step_mode_default(req))
}

/// 1. The tool says this call is the user's to accept.
fn step_confirm(req: &Request<'_>) -> Option<Decision> {
    req.confirm.map(|detail| Decision::Ask {
        reason: Reason::Confirm {
            detail: detail.to_string(),
        },
        scope: None,
    })
}

/// 2. A deny rule, read the broad way: one sub-command is enough.
fn step_deny_rules(req: &Request<'_>, rules: &Rules) -> Option<Decision> {
    first_match(&rules.deny, req.call, MatchMode::Any, &req.context()).map(|rule| Decision::Deny {
        reason: Reason::Rule {
            rule: rule.raw().to_string(),
        },
    })
}

/// 3. A write into `.git` and its siblings: the directories that decide what
///    the tools themselves may do next.
fn step_sensitive_path(req: &Request<'_>) -> Option<Decision> {
    if !(req.traits.edit || req.traits.destructive) {
        return None;
    }
    let target = req.paths().find(|target| path::is_sensitive(target))?;
    Some(Decision::Ask {
        reason: Reason::Safety {
            detail: format!("writing into a sensitive path: {target}"),
        },
        scope: None,
    })
}

/// 5. The user asked for no gate.
fn step_bypass(req: &Request<'_>) -> Option<Decision> {
    (req.mode == Mode::BypassPermissions).then(|| Decision::Allow {
        reason: Reason::Mode {
            mode: req.mode.to_string(),
        },
    })
}

/// 6. An allow rule, read the narrow way: every subject and every sub-command.
fn step_allow_rules(req: &Request<'_>, rules: &Rules) -> Option<Decision> {
    first_match(&rules.allow, req.call, MatchMode::All, &req.context()).map(|rule| {
        Decision::Allow {
            reason: Reason::Rule {
                rule: rule.raw().to_string(),
            },
        }
    })
}

/// 4. An ask rule, read the broad way like a deny rule.
fn step_ask_rules(req: &Request<'_>, rules: &Rules) -> Option<Decision> {
    first_match(&rules.ask, req.call, MatchMode::Any, &req.context()).map(|rule| Decision::Ask {
        reason: Reason::Rule {
            rule: rule.raw().to_string(),
        },
        scope: None,
    })
}

/// 7. Nothing named this call; the mode decides.
fn step_mode_default(req: &Request<'_>) -> Decision {
    match req.mode {
        Mode::AcceptEdits => accept_edits_default(req),
        // `bypassPermissions` never reaches here (step 4). `plan` and `dontAsk`
        // read like `default` and differ only in what `limit_by_mode` then does
        // with the answer.
        Mode::Default | Mode::Plan | Mode::DontAsk | Mode::BypassPermissions => trait_default(req),
    }
}

fn trait_default(req: &Request<'_>) -> Decision {
    if trusted_read_only(req.traits) {
        Decision::Allow {
            reason: Reason::ReadOnly,
        }
    } else {
        Decision::Ask {
            reason: Reason::Default,
            scope: None,
        }
    }
}

fn accept_edits_default(req: &Request<'_>) -> Decision {
    if edits_within_reach(req) {
        Decision::Allow {
            reason: Reason::Mode {
                mode: Mode::AcceptEdits.to_string(),
            },
        }
    } else {
        trait_default(req)
    }
}

/// Read-only runs unasked only when the kernel vouches for the traits: an MCP
/// server's `readOnlyHint` is a claim by the thing being gated.
fn trusted_read_only(traits: &ToolTraits) -> bool {
    traits.trusted && traits.read_only
}

/// An edit is accepted unasked only where the user said the work happens. A
/// tool that says nothing about the paths it touches names nowhere, and
/// nowhere is not inside anything.
fn edits_within_reach(req: &Request<'_>) -> bool {
    if !(req.traits.trusted && req.traits.edit) {
        return false;
    }
    let roots = req.roots();
    let mut paths = req.paths().peekable();
    paths.peek().is_some()
        && paths.all(|target| roots.iter().any(|root| path::is_within(&target, root)))
}

/// A session scope is offered only when installing it would really silence the
/// prompt: the ladder runs again with the candidate in the allow table, and
/// only an allow earns the offer. A prompt that outranks allow rules — a
/// confirmation, a sensitive path, a deny — therefore offers nothing.
fn with_scope(req: &Request<'_>, rules: &Rules, decision: Decision) -> Decision {
    let Decision::Ask { reason, .. } = decision else {
        return decision;
    };
    Decision::Ask {
        reason,
        scope: silencing_rule(req, rules),
    }
}

fn silencing_rule(req: &Request<'_>, rules: &Rules) -> Option<String> {
    let candidate = scope::narrowest(req.call, req.cwd, req.home)?;
    let mut widened = rules.clone();
    widened.allow.push(Rule::parse(&candidate)?);
    matches!(steps(req, &widened), Decision::Allow { .. }).then_some(candidate)
}

/// What the mode cannot honour, it denies. `plan` is a ceiling, not a default:
/// a rule the user wrote for another mode must not turn planning into doing.
/// `dontAsk` has nobody to answer a prompt, and the fail-closed reading of a
/// prompt nobody will see is a denial — including the prompts no mode can
/// silence, which are denied rather than granted.
fn limit_by_mode(req: &Request<'_>, decision: Decision) -> Decision {
    match req.mode {
        Mode::Plan if !trusted_read_only(req.traits) => refuse(Mode::Plan, decision),
        Mode::DontAsk if matches!(decision, Decision::Ask { .. }) => {
            refuse(Mode::DontAsk, decision)
        }
        _ => decision,
    }
}

/// The mode's denial, unless the ladder already had a better reason for one.
fn refuse(mode: Mode, decision: Decision) -> Decision {
    match decision {
        Decision::Deny { reason } => Decision::Deny { reason },
        _ => Decision::Deny {
            reason: Reason::Mode {
                mode: mode.to_string(),
            },
        },
    }
}

impl<'a> Request<'a> {
    fn context(&self) -> MatchContext<'a> {
        MatchContext {
            cwd: self.cwd,
            home: self.home,
        }
    }

    /// Every path this call names, normalised.
    fn paths(&self) -> impl Iterator<Item = String> + '_ {
        self.call
            .subjects
            .iter()
            .filter_map(|subject| match subject {
                Subject::Path { path } => Some(path::normalize_path(path, self.cwd, self.home)),
                _ => None,
            })
    }

    /// Where `acceptEdits` lets edits happen.
    fn roots(&self) -> Vec<String> {
        let mut roots = vec![path::normalize_path(self.cwd, self.cwd, self.home)];
        roots.extend(
            self.extra_dirs
                .iter()
                .map(|dir| path::normalize_path(dir, self.cwd, self.home)),
        );
        roots
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// What a decision is, without the reason, for a terse table.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum Kind {
        Allow,
        Deny,
        Ask,
    }

    pub(crate) fn kind(decision: &Decision) -> Kind {
        match decision {
            Decision::Allow { .. } => Kind::Allow,
            Decision::Deny { .. } => Kind::Deny,
            Decision::Ask { .. } => Kind::Ask,
        }
    }

    /// One call, built the way a test reads.
    #[derive(Clone, Debug)]
    pub(crate) struct Probe {
        name: String,
        subjects: Vec<Subject>,
        traits: ToolTraits,
        confirm: Option<String>,
        mode: Mode,
        cwd: PathBuf,
        home: Option<PathBuf>,
        extra_dirs: Vec<PathBuf>,
        rules: Rules,
    }

    impl Probe {
        pub(crate) fn tool(name: &str, traits: ToolTraits) -> Self {
            Self {
                name: name.to_string(),
                subjects: Vec::new(),
                traits,
                confirm: None,
                mode: Mode::Default,
                cwd: PathBuf::from("/work/project"),
                home: Some(PathBuf::from("/home/user")),
                extra_dirs: Vec::new(),
                rules: Rules::default(),
            }
        }

        pub(crate) fn read(path: &str) -> Self {
            Self::tool("Read", ToolTraits::read_only()).path(path)
        }

        pub(crate) fn write(path: &str) -> Self {
            Self::tool("Write", ToolTraits::edit()).path(path)
        }

        pub(crate) fn bash(command: &str) -> Self {
            let traits = ToolTraits {
                trusted: true,
                ..ToolTraits::default()
            };
            Self::tool("Bash", traits).command(command)
        }

        pub(crate) fn command(mut self, command: &str) -> Self {
            self.subjects.push(Subject::Command {
                command: command.to_string(),
            });
            self
        }

        pub(crate) fn path(mut self, path: &str) -> Self {
            self.subjects.push(Subject::Path {
                path: PathBuf::from(path),
            });
            self
        }

        pub(crate) fn url(mut self, url: &str) -> Self {
            self.subjects.push(Subject::Url {
                url: url.to_string(),
            });
            self
        }

        pub(crate) fn named(mut self, name: &str) -> Self {
            self.subjects.push(Subject::Name {
                name: name.to_string(),
            });
            self
        }

        pub(crate) fn mode(mut self, mode: Mode) -> Self {
            self.mode = mode;
            self
        }

        pub(crate) fn cwd(mut self, cwd: &str) -> Self {
            self.cwd = PathBuf::from(cwd);
            self
        }

        pub(crate) fn confirm(mut self, detail: &str) -> Self {
            self.confirm = Some(detail.to_string());
            self
        }

        pub(crate) fn extra_dirs(mut self, dirs: &[&str]) -> Self {
            self.extra_dirs = dirs.iter().map(PathBuf::from).collect();
            self
        }

        pub(crate) fn allow(mut self, rules: &[&str]) -> Self {
            self.rules.allow = parse_all(rules);
            self
        }

        pub(crate) fn deny(mut self, rules: &[&str]) -> Self {
            self.rules.deny = parse_all(rules);
            self
        }

        pub(crate) fn ask(mut self, rules: &[&str]) -> Self {
            self.rules.ask = parse_all(rules);
            self
        }

        pub(crate) fn decide(&self) -> Decision {
            decide(
                &Request {
                    call: Call {
                        name: &self.name,
                        subjects: &self.subjects,
                    },
                    traits: &self.traits,
                    confirm: self.confirm.as_deref(),
                    mode: self.mode,
                    cwd: &self.cwd,
                    home: self.home.as_deref(),
                    extra_dirs: &self.extra_dirs,
                },
                &self.rules,
            )
        }

        pub(crate) fn kind(&self) -> Kind {
            kind(&self.decide())
        }

        pub(crate) fn scope(&self) -> Option<String> {
            match self.decide() {
                Decision::Ask { scope, .. } => scope,
                _ => None,
            }
        }
    }

    fn parse_all(rules: &[&str]) -> Vec<Rule> {
        rules
            .iter()
            .map(|raw| Rule::parse(raw).expect("a rule the grammar reads"))
            .collect()
    }

    // --- the mode table -------------------------------------------------

    #[test]
    fn a_plain_non_read_only_tool_across_every_mode() {
        let probe = Probe::tool(
            "Report",
            ToolTraits {
                trusted: true,
                ..ToolTraits::default()
            },
        );
        for (mode, want) in [
            (Mode::Default, Kind::Ask),
            (Mode::AcceptEdits, Kind::Ask),
            (Mode::BypassPermissions, Kind::Allow),
            (Mode::DontAsk, Kind::Deny),
            (Mode::Plan, Kind::Deny),
        ] {
            assert_eq!(probe.clone().mode(mode).kind(), want, "{mode}");
        }
    }

    #[test]
    fn a_trusted_read_only_tool_is_allowed_in_every_mode() {
        let probe = Probe::read("Cargo.toml");
        for mode in Mode::ALL {
            assert_eq!(probe.clone().mode(mode).kind(), Kind::Allow, "{mode}");
        }
        assert!(matches!(
            probe.decide(),
            Decision::Allow {
                reason: Reason::ReadOnly
            }
        ));
    }

    #[test]
    fn a_write_asks_by_default_and_is_accepted_by_accept_edits() {
        let probe = Probe::write("/work/project/x.txt");
        assert_eq!(probe.clone().kind(), Kind::Ask);
        assert_eq!(probe.clone().mode(Mode::AcceptEdits).kind(), Kind::Allow);
        assert_eq!(probe.mode(Mode::BypassPermissions).kind(), Kind::Allow);
    }

    #[test]
    fn accept_edits_stops_at_the_edge_of_the_working_directories() {
        let outside = Probe::write("/elsewhere/x.txt").mode(Mode::AcceptEdits);
        assert_eq!(outside.clone().kind(), Kind::Ask, "outside the cwd");
        assert_eq!(
            outside.extra_dirs(&["/elsewhere"]).kind(),
            Kind::Allow,
            "a directory the user named is inside"
        );
    }

    #[test]
    fn accept_edits_needs_a_path_to_judge() {
        let nothing = Probe::tool("Format", ToolTraits::edit()).mode(Mode::AcceptEdits);
        assert_eq!(nothing.kind(), Kind::Ask, "an edit that names nowhere");
    }

    #[test]
    fn plan_denies_everything_that_is_not_read_only() {
        assert_eq!(
            Probe::write("/work/project/x").mode(Mode::Plan).kind(),
            Kind::Deny
        );
        assert_eq!(
            Probe::bash("ls")
                .mode(Mode::Plan)
                .allow(&["Bash(ls)"])
                .kind(),
            Kind::Deny,
            "an allow rule written for another mode does not turn planning into doing"
        );
        assert_eq!(
            Probe::tool("Publish", ToolTraits::edit())
                .confirm("a person decides")
                .mode(Mode::Plan)
                .kind(),
            Kind::Deny,
            "plan refuses rather than prompts"
        );
    }

    // --- the rule table -------------------------------------------------

    #[test]
    fn a_deny_rule_beats_the_mode_that_would_have_allowed() {
        let probe = Probe::bash("rm -rf /tmp/x").deny(&["Bash(rm -rf)"]);
        for mode in Mode::ALL {
            assert_eq!(probe.clone().mode(mode).kind(), Kind::Deny, "{mode}");
        }
    }

    #[test]
    fn a_deny_rule_matches_any_sub_command() {
        for command in [
            "rm -rf /tmp/x",
            "cd /tmp && rm -rf /",
            "ls; rm -rf ~",
            "true || rm -rf /",
            "cat x | rm -rf /",
            "echo hi\nrm -rf /",
            "ls & rm -rf /",
            "(cd /tmp && rm -rf /)",
            "echo $(rm -rf /)",
        ] {
            assert_eq!(
                Probe::bash(command).deny(&["Bash(rm)"]).kind(),
                Kind::Deny,
                "{command}"
            );
        }
        assert_eq!(
            Probe::bash("echo 'a; b'").deny(&["Bash(b)"]).kind(),
            Kind::Ask,
            "quoted text is not a sub-command"
        );
    }

    #[test]
    fn an_allow_rule_needs_every_sub_command() {
        assert_eq!(
            Probe::bash("ls -la").allow(&["Bash(ls)"]).kind(),
            Kind::Allow
        );
        for command in [
            "ls; rm -rf ~",
            "ls && rm -rf ~",
            "ls | rm -rf ~",
            "ls & rm -rf ~",
            "ls\nrm -rf ~",
            "ls \"; rm -rf ~",
        ] {
            assert_eq!(
                Probe::bash(command).allow(&["Bash(ls)"]).kind(),
                Kind::Ask,
                "{command}"
            );
        }
        assert_eq!(
            Probe::bash("ls -la && ls /tmp").allow(&["Bash(ls)"]).kind(),
            Kind::Allow
        );
    }

    #[test]
    fn an_allow_rule_lets_bash_run_in_default_mode() {
        assert_eq!(
            Probe::bash("git status").allow(&["Bash(git)"]).kind(),
            Kind::Allow
        );
    }

    #[test]
    fn the_colon_star_suffix_is_a_prefix_as_one_piece() {
        assert_eq!(
            Probe::bash("git push origin main")
                .deny(&["Bash(git push:*)"])
                .kind(),
            Kind::Deny
        );
        assert_ne!(
            Probe::bash("git pull").deny(&["Bash(git push:*)"]).kind(),
            Kind::Deny,
            "the prefix stays tight"
        );
        assert_eq!(
            Probe::bash("git log --oneline")
                .allow(&["Bash(git log:*)"])
                .kind(),
            Kind::Allow
        );
    }

    #[test]
    fn a_path_rule_normalises_before_it_denies() {
        let denied = |path: &str| Probe::read(path).deny(&["Read(/etc/)"]).kind();
        assert_eq!(denied("/etc/passwd"), Kind::Deny);
        assert_eq!(denied("/etc/../etc/passwd"), Kind::Deny);
        assert_eq!(denied("/etc/./ssh/../passwd"), Kind::Deny);
        assert_eq!(denied("/var/log/x"), Kind::Allow, "read-only elsewhere");
        assert_eq!(
            Probe::read("./src/main.rs")
                .deny(&["Read(/work/project/src)"])
                .kind(),
            Kind::Deny,
            "a relative path expands against the cwd first"
        );
    }

    #[test]
    fn a_skill_rule_matches_exactly_or_by_prefix() {
        let skill = || {
            Probe::tool(
                "Skill",
                ToolTraits {
                    trusted: true,
                    ..ToolTraits::default()
                },
            )
            .named("review-pr")
        };
        assert_eq!(
            skill().kind(),
            Kind::Ask,
            "running a skill is not read-only"
        );
        assert_eq!(skill().allow(&["Skill(review-pr)"]).kind(), Kind::Allow);
        assert_eq!(skill().allow(&["Skill(review:*)"]).kind(), Kind::Allow);
        assert_eq!(skill().allow(&["Skill(*)"]).kind(), Kind::Allow);
        assert_eq!(skill().allow(&["Skill(commit)"]).kind(), Kind::Ask);
    }

    #[test]
    fn a_url_tool_asks_until_a_domain_rule_names_its_host() {
        let fetch = |url: &str| {
            Probe::tool(
                "WebFetch",
                ToolTraits {
                    trusted: true,
                    ..ToolTraits::default()
                },
            )
            .url(url)
        };
        assert_eq!(fetch("https://example.com/page").kind(), Kind::Ask);
        assert_eq!(
            fetch("https://internal.example.com/docs")
                .allow(&["WebFetch(domain:internal.example.com)"])
                .kind(),
            Kind::Allow
        );
    }

    #[test]
    fn an_mcp_tools_own_read_only_claim_never_reaches_the_gate() {
        let peek = || {
            Probe::tool(
                "mcp__srv__peek",
                ToolTraits {
                    read_only: true,
                    concurrency_safe: true,
                    ..ToolTraits::default()
                },
            )
        };
        assert_eq!(
            peek().kind(),
            Kind::Ask,
            "an untrusted readOnlyHint is a claim, not a fact"
        );
        assert_eq!(peek().allow(&["mcp__srv"]).kind(), Kind::Allow);
        assert_eq!(peek().mode(Mode::Plan).kind(), Kind::Deny);
    }

    // --- what no mode can silence ---------------------------------------

    #[test]
    fn a_tools_own_confirmation_asks_in_every_mode() {
        let probe = Probe::tool("Publish", ToolTraits::read_only())
            .confirm("only a person may publish")
            .allow(&["Publish"]);
        for mode in [Mode::Default, Mode::AcceptEdits, Mode::BypassPermissions] {
            assert_eq!(probe.clone().mode(mode).kind(), Kind::Ask, "{mode}");
        }
        assert!(matches!(
            probe.decide(),
            Decision::Ask {
                reason: Reason::Confirm { .. },
                ..
            }
        ));
    }

    #[test]
    fn a_write_into_a_sensitive_path_asks_in_every_mode() {
        let probe = Probe::write("/work/project/.git/config").allow(&["Write"]);
        for mode in [Mode::Default, Mode::AcceptEdits, Mode::BypassPermissions] {
            assert_eq!(probe.clone().mode(mode).kind(), Kind::Ask, "{mode}");
        }
        assert!(matches!(
            probe.decide(),
            Decision::Ask {
                reason: Reason::Safety { .. },
                ..
            }
        ));
    }

    #[test]
    fn a_sensitive_relative_path_resolves_from_the_session_cwd() {
        let probe = Probe::tool("Write", ToolTraits::edit())
            .path("config")
            .cwd("project/.git")
            .mode(Mode::BypassPermissions);
        assert_eq!(probe.kind(), Kind::Ask);
    }

    #[test]
    fn accept_edits_does_not_reach_a_sensitive_path() {
        let probe = Probe::tool("Edit", ToolTraits::edit())
            .path("/work/project/.git/config")
            .mode(Mode::AcceptEdits);
        assert_eq!(probe.kind(), Kind::Ask);
    }

    #[test]
    fn a_read_of_a_sensitive_path_is_still_a_read() {
        assert_eq!(
            Probe::read("/work/project/.git/config").kind(),
            Kind::Allow,
            "the rule is about writing, not looking"
        );
    }

    #[test]
    fn dont_ask_denies_even_the_prompts_no_mode_can_silence() {
        let confirm = Probe::tool("Publish", ToolTraits::read_only())
            .confirm("only a person may publish")
            .mode(Mode::DontAsk);
        assert_eq!(confirm.kind(), Kind::Deny);
        let sensitive = Probe::write("/work/project/.git/config").mode(Mode::DontAsk);
        assert_eq!(sensitive.kind(), Kind::Deny);
    }

    // --- the ladder's own order -----------------------------------------

    #[test]
    fn an_ask_rule_outranks_an_allow_rule() {
        let probe = Probe::bash("git push").ask(&["Bash(git push)"]);
        assert_eq!(probe.clone().kind(), Kind::Ask);
        assert_eq!(
            probe.allow(&["Bash(git push:*)"]).kind(),
            Kind::Ask,
            "a person asked to be checked with; a broader allow does not silence that"
        );
    }

    #[test]
    fn an_ask_rule_survives_bypass_and_a_deny_rule_still_wins() {
        let probe = Probe::bash("git push").mode(Mode::BypassPermissions);
        assert_eq!(probe.clone().ask(&["Bash(git push)"]).kind(), Kind::Ask);
        assert_eq!(probe.deny(&["Bash(git push)"]).kind(), Kind::Deny);
    }

    // --- the session scope ----------------------------------------------

    #[test]
    fn a_session_scope_stops_the_gate_asking_again() {
        let probe = Probe::bash("cargo test --locked");
        assert_eq!(probe.clone().kind(), Kind::Ask);
        let scope = probe.scope().expect("a plain cargo call has a scope");
        assert_eq!(scope, "Bash(cargo:*)");
        assert_eq!(
            Probe::bash("cargo test --locked").allow(&[&scope]).kind(),
            Kind::Allow,
            "the call that asked is covered"
        );
        assert_eq!(
            Probe::bash("cargo build").allow(&[&scope]).kind(),
            Kind::Allow,
            "and the next call of the session"
        );
    }

    #[test]
    fn a_compound_command_gets_no_session_scope() {
        assert_eq!(Probe::bash("cd /tmp && rm -rf /").scope(), None);
    }

    #[test]
    fn a_prompt_that_outranks_allow_rules_offers_no_scope() {
        assert_eq!(
            Probe::write("/work/project/.git/config").scope(),
            None,
            "a sensitive path is not scopeable away"
        );
        assert_eq!(
            Probe::tool("Publish", ToolTraits::read_only())
                .confirm("a person decides")
                .scope(),
            None
        );
        assert_eq!(
            Probe::write("/work/project/note.txt").scope(),
            Some("Write(/work/project/)".to_string()),
            "an ordinary write is scoped to its directory"
        );
    }

    #[test]
    fn an_ask_rule_prompt_offers_no_session_scope() {
        // An ask rule outranks every allow rule, so no session rule could
        // silence the next call; offering one would be dead text.
        assert_eq!(
            Probe::write("/work/project/note.txt")
                .ask(&["Write"])
                .scope(),
            None
        );
    }
}

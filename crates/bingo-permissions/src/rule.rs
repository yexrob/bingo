//! The rule table: one settings line, parsed once, matched against what a tool
//! says it will touch.
//!
//! Grammar, one form per line:
//!
//! - `Tool` — every call of that tool.
//! - `Tool(*)` — the same; a rule that names nothing narrows nothing.
//! - `Tool(text)` — a prefix of a command, a path or a URL; the exact name of a
//!   `Skill`-style `Name` subject.
//! - `Tool(text:*)` — `text` as a prefix in every subject kind.
//! - `Tool(prefix:text)` — the same; `prefix:` is stripped.
//! - `Tool(/src/**)` — a path glob, where `*` stops at a separator.
//! - `Tool(domain:host)` — the URL host, exactly.
//! - `mcp__server` — every tool of that server.
//! - `mcp__server__tool` — that one tool.
//!
//! Deny and ask read a rule the broad way, allow the narrow way: each mode
//! takes the reading that fails closed.

use std::path::Path;

use bingo_sdk::Subject;
use globset::{GlobBuilder, GlobMatcher};

use crate::split;
use crate::{path, url};

/// How a rule table reads a call with more than one thing to match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatchMode {
    /// deny and ask: one hit is enough.
    Any,
    /// allow: every subject, and every sub-command inside one, must be covered.
    All,
}

/// What a rule is matched against.
#[derive(Clone, Copy, Debug)]
pub struct Call<'a> {
    pub name: &'a str,
    pub subjects: &'a [Subject],
}

/// What a path rule resolves against. No filesystem lookup happens here.
#[derive(Clone, Copy, Debug)]
pub struct MatchContext<'a> {
    pub cwd: &'a Path,
    pub home: Option<&'a Path>,
}

/// The three tables. The kernel merges the settings layers before the plugin
/// sees them, so these are already the whole story.
#[derive(Clone, Debug, Default)]
pub struct Rules {
    pub allow: Vec<Rule>,
    pub deny: Vec<Rule>,
    pub ask: Vec<Rule>,
}

/// The first rule of a table that covers the call.
pub fn first_match<'a>(
    table: &'a [Rule],
    call: Call<'_>,
    mode: MatchMode,
    cx: &MatchContext<'_>,
) -> Option<&'a Rule> {
    table.iter().find(|rule| rule.matches(call, mode, cx))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    raw: String,
    tool: ToolPattern,
    /// `None` when the rule names a tool and nothing else.
    content: Option<Content>,
}

impl Rule {
    /// `None` for a line this grammar cannot read; the plugin refuses to start
    /// on one rather than quietly dropping a deny rule.
    pub fn parse(raw: &str) -> Option<Self> {
        let text = raw.trim();
        let (name, content) = match text.split_once('(') {
            Some((name, rest)) => (name.trim(), Content::parse(rest.strip_suffix(')')?)),
            None => (text, None),
        };
        Some(Self {
            raw: text.to_string(),
            tool: ToolPattern::parse(name)?,
            content,
        })
    }

    /// The rule as written, for the reason the gate shows.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn matches(&self, call: Call<'_>, mode: MatchMode, cx: &MatchContext<'_>) -> bool {
        if !self.tool.matches(call.name) {
            return false;
        }
        let Some(content) = &self.content else {
            return true;
        };
        match mode {
            MatchMode::Any => call
                .subjects
                .iter()
                .any(|subject| content.covers(subject, mode, cx)),
            // A rule that names something must name everything the call
            // touches, and a call that names nothing is covered by no rule.
            MatchMode::All => {
                !call.subjects.is_empty()
                    && call
                        .subjects
                        .iter()
                        .all(|subject| content.covers(subject, mode, cx))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ToolPattern {
    Exact(String),
    /// `mcp__server`: every tool that server exposes.
    Server(String),
}

impl ToolPattern {
    fn parse(name: &str) -> Option<Self> {
        if name.is_empty() {
            return None;
        }
        if name.starts_with("mcp__") && name.split("__").count() == 2 {
            return Some(Self::Server(name.to_string()));
        }
        Some(Self::Exact(name.to_string()))
    }

    fn matches(&self, name: &str) -> bool {
        match self {
            Self::Exact(want) => want == name,
            Self::Server(server) => {
                name == server
                    || name
                        .strip_prefix(server.as_str())
                        .is_some_and(|rest| rest.starts_with("__"))
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Content {
    /// `Tool(text)`: a prefix for a command, path or URL; an exact name.
    Literal(String),
    /// `Tool(text:*)` or `Tool(prefix:text)`: a prefix in every subject kind.
    Prefix(String),
    /// `Tool(domain:host)`: the URL host and nothing else.
    Domain(String),
}

impl Content {
    /// `None` means "every call of this tool": `Tool(*)` says no more than
    /// `Tool` does. An empty `domain:` stays a `Domain` that matches no host,
    /// because a rule naming a host must not widen into one naming none.
    fn parse(inner: &str) -> Option<Self> {
        let text = inner.trim();
        if let Some(host) = text.strip_prefix("domain:") {
            return Some(Self::Domain(host.trim_end_matches('*').to_string()));
        }
        let (text, explicit) = match text.strip_prefix("prefix:") {
            Some(text) => (text, true),
            None => (text, false),
        };
        let (text, explicit) = match text.strip_suffix(":*") {
            Some(text) => (text, true),
            None => (text, explicit),
        };
        if text.is_empty() || text == "*" {
            return None;
        }
        Some(match explicit {
            true => Self::Prefix(text.to_string()),
            false => Self::Literal(text.to_string()),
        })
    }

    fn text(&self) -> &str {
        match self {
            Self::Literal(text) | Self::Prefix(text) | Self::Domain(text) => text,
        }
    }

    fn covers(&self, subject: &Subject, mode: MatchMode, cx: &MatchContext<'_>) -> bool {
        match (self, subject) {
            (Self::Domain(host), Subject::Url { url }) => host_is(url, host),
            (Self::Domain(_), _) => false,
            (_, Subject::Command { command }) => command_covered(command, self.text(), mode),
            (_, Subject::Path { path }) => path_covered(path, self, cx),
            (_, Subject::Url { url }) => url.starts_with(self.text()),
            (Self::Literal(text), Subject::Name { name }) => name == text,
            (Self::Prefix(text), Subject::Name { name }) => name.starts_with(text),
        }
    }
}

fn host_is(url: &str, want: &str) -> bool {
    !want.is_empty() && url::host(url).is_some_and(|host| host.eq_ignore_ascii_case(want))
}

/// A rule covers a Bash string only when it covers what that string runs.
fn command_covered(command: &str, content: &str, mode: MatchMode) -> bool {
    let split = split::split(command);
    match mode {
        MatchMode::All => {
            split.is_parsed()
                && !split.parts().is_empty()
                && split
                    .parts()
                    .iter()
                    .all(|part| unit_covered(part, content, mode))
        }
        // Deny and ask also read the whole string: when the parse is not
        // trusted the parts are not the whole story, and a rule written against
        // the text as the model wrote it must still hold.
        MatchMode::Any => {
            split
                .parts()
                .iter()
                .any(|part| unit_covered(part, content, mode))
                || unit_covered(command.trim(), content, mode)
        }
    }
}

/// Two readings of "starts with": the literal text, and the words the shell
/// would see. Deny takes either, allow needs both.
fn unit_covered(unit: &str, content: &str, mode: MatchMode) -> bool {
    let text = unit.starts_with(content);
    let words = words_start_with(unit, content);
    match mode {
        MatchMode::Any => text || words,
        MatchMode::All => text && words,
    }
}

fn words_start_with(unit: &str, content: &str) -> bool {
    let (Some(unit), Some(content)) = (shlex::split(unit), shlex::split(content)) else {
        return false;
    };
    content.len() <= unit.len() && unit.iter().zip(&content).all(|(word, want)| word == want)
}

fn path_covered(target: &Path, content: &Content, cx: &MatchContext<'_>) -> bool {
    let target = path::normalize_path(target, cx.cwd, cx.home);
    let pattern = path::normalize(content.text(), cx.cwd, cx.home);
    if matches!(content, Content::Literal(_))
        && let Some(glob) = compile(&pattern)
    {
        return glob.is_match(&target);
    }
    target.starts_with(&pattern)
}

/// A path rule is a glob only when it is written as one, so `Read(/etc/)` keeps
/// covering everything under it.
fn compile(pattern: &str) -> Option<GlobMatcher> {
    if !pattern.contains(['*', '?', '[', '{']) {
        return None;
    }
    GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .ok()
        .map(|glob| glob.compile_matcher())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cwd() -> PathBuf {
        PathBuf::from("/work/project")
    }

    fn cx(cwd: &Path) -> MatchContext<'_> {
        MatchContext { cwd, home: None }
    }

    fn command(text: &str) -> Vec<Subject> {
        vec![Subject::Command {
            command: text.to_string(),
        }]
    }

    fn file(text: &str) -> Vec<Subject> {
        vec![Subject::Path {
            path: PathBuf::from(text),
        }]
    }

    fn named(text: &str) -> Vec<Subject> {
        vec![Subject::Name {
            name: text.to_string(),
        }]
    }

    fn link(text: &str) -> Vec<Subject> {
        vec![Subject::Url {
            url: text.to_string(),
        }]
    }

    fn hits(rule: &str, name: &str, subjects: &[Subject], mode: MatchMode) -> bool {
        let cwd = cwd();
        Rule::parse(rule)
            .expect("a rule the grammar reads")
            .matches(Call { name, subjects }, mode, &cx(&cwd))
    }

    fn allows(rule: &str, name: &str, subjects: &[Subject]) -> bool {
        hits(rule, name, subjects, MatchMode::All)
    }

    fn denies(rule: &str, name: &str, subjects: &[Subject]) -> bool {
        hits(rule, name, subjects, MatchMode::Any)
    }

    #[test]
    fn a_bare_tool_rule_covers_every_call_of_that_tool() {
        assert!(allows("Write", "Write", &file("/tmp/x")));
        assert!(allows("Write", "Write", &[]));
        assert!(!allows("Write", "Edit", &file("/tmp/x")));
    }

    #[test]
    fn a_star_says_no_more_than_the_bare_tool_name() {
        for rule in ["Skill", "Skill(*)", "Skill()"] {
            assert!(allows(rule, "Skill", &named("anything")), "{rule}");
            assert!(allows(rule, "Skill", &[]), "{rule}");
        }
    }

    #[test]
    fn an_unreadable_line_is_not_a_rule() {
        for raw in ["", "   ", "(x)", "Bash(git", "Bash(git))x"] {
            assert_eq!(Rule::parse(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn a_bash_rule_is_a_prefix_of_the_command() {
        assert!(allows("Bash(git)", "Bash", &command("git status")));
        assert!(allows(
            "Bash(git push:*)",
            "Bash",
            &command("git push origin")
        ));
        assert!(!allows("Bash(git push:*)", "Bash", &command("git pull")));
        assert!(allows(
            "Bash(prefix:git log)",
            "Bash",
            &command("git log -n1")
        ));
    }

    #[test]
    fn allow_needs_every_sub_command_and_deny_takes_any() {
        for command_text in [
            "ls; rm -rf ~",
            "ls && rm -rf ~",
            "ls | rm -rf ~",
            "ls & rm -rf ~",
            "ls\nrm -rf ~",
            "ls $(rm -rf ~)",
        ] {
            let subjects = command(command_text);
            assert!(!allows("Bash(ls)", "Bash", &subjects), "{command_text}");
            assert!(denies("Bash(rm)", "Bash", &subjects), "{command_text}");
        }
        assert!(allows("Bash(ls)", "Bash", &command("ls -la && ls /tmp")));
    }

    #[test]
    fn an_untrusted_parse_is_covered_by_no_allow_rule() {
        let subjects = command("ls \"; rm -rf ~");
        assert!(!allows("Bash(ls)", "Bash", &subjects));
        assert!(
            allows("Bash", "Bash", &subjects),
            "a bare rule is about the tool, not the text"
        );
        assert!(
            denies("Bash(ls)", "Bash", &subjects),
            "ask still reads what it can"
        );
    }

    #[test]
    fn a_quoted_separator_is_not_a_sub_command() {
        assert!(!denies("Bash(b)", "Bash", &command("echo 'a; b'")));
    }

    #[test]
    fn a_word_boundary_narrows_allow_without_narrowing_deny() {
        // `rmdir` starts with the text `rm`, so a deny rule still holds; an
        // allow rule must not, because `rmdir` is not the command named.
        let subjects = command("rmdir /tmp/x");
        assert!(denies("Bash(rm)", "Bash", &subjects));
        assert!(!allows("Bash(rm)", "Bash", &subjects));
        // And a deny written mid-word still holds on the text.
        assert!(denies("Bash(rm -r)", "Bash", &command("rm -rf /")));
    }

    #[test]
    fn a_path_rule_normalises_before_it_matches() {
        assert!(denies("Read(/etc/)", "Read", &file("/etc/passwd")));
        assert!(denies("Read(/etc/)", "Read", &file("/etc/../etc/passwd")));
        assert!(denies("Read(/etc/)", "Read", &file("/etc/./ssh/../passwd")));
        assert!(!denies("Read(/etc/)", "Read", &file("/var/log/x")));
        assert!(denies(
            "Read(/work/project/src)",
            "Read",
            &file("./src/main.rs")
        ));
    }

    #[test]
    fn a_path_rule_written_as_a_glob_matches_as_one() {
        assert!(allows("Edit(/src/**)", "Edit", &file("/src/a/b.rs")));
        assert!(allows("Edit(/src/*)", "Edit", &file("/src/a.rs")));
        assert!(!allows("Edit(/src/*)", "Edit", &file("/src/a/b.rs")));
        assert!(!allows("Edit(/src/**)", "Edit", &file("/lib/a.rs")));
        assert!(allows(
            "Edit(src/**)",
            "Edit",
            &file("/work/project/src/a.rs")
        ));
    }

    #[test]
    fn a_domain_rule_matches_the_host_and_only_the_host() {
        let rule = "WebFetch(domain:internal.example.com)";
        assert!(allows(
            rule,
            "WebFetch",
            &link("https://internal.example.com/docs")
        ));
        assert!(!allows(
            rule,
            "WebFetch",
            &link("https://evil.example.com/docs")
        ));
        assert!(!allows(rule, "WebFetch", &link("not a url")));
        assert!(!allows(
            "WebFetch(domain:)",
            "WebFetch",
            &link("https://x.example/")
        ));
    }

    #[test]
    fn a_url_rule_without_domain_is_a_prefix() {
        assert!(allows(
            "WebFetch(https://example.com/docs)",
            "WebFetch",
            &link("https://example.com/docs/index.html")
        ));
    }

    #[test]
    fn a_name_rule_is_exact_unless_it_says_prefix() {
        assert!(allows("Skill(review-pr)", "Skill", &named("review-pr")));
        assert!(!allows("Skill(review)", "Skill", &named("review-pr")));
        assert!(allows("Skill(review:*)", "Skill", &named("review-pr")));
        assert!(!allows("Skill(commit)", "Skill", &named("review-pr")));
    }

    #[test]
    fn an_mcp_server_rule_covers_its_tools_and_a_tool_rule_covers_one() {
        assert!(allows("mcp__srv", "mcp__srv__peek", &[]));
        assert!(allows("mcp__srv", "mcp__srv", &[]));
        assert!(!allows("mcp__srv", "mcp__other__peek", &[]));
        assert!(allows("mcp__srv__peek", "mcp__srv__peek", &[]));
        assert!(!allows("mcp__srv__peek", "mcp__srv__peekaboo", &[]));
    }

    #[test]
    fn a_rule_that_names_something_never_covers_a_call_that_names_nothing() {
        assert!(!allows("Bash(git)", "Bash", &[]));
        assert!(!denies("Bash(git)", "Bash", &[]));
    }

    #[test]
    fn the_first_matching_rule_of_a_table_is_the_one_reported() {
        let cwd = cwd();
        let table: Vec<Rule> = ["Bash(cargo)", "Bash(git)"]
            .iter()
            .filter_map(|r| Rule::parse(r))
            .collect();
        let subjects = command("git status");
        let hit = first_match(
            &table,
            Call {
                name: "Bash",
                subjects: &subjects,
            },
            MatchMode::All,
            &cx(&cwd),
        );
        assert_eq!(hit.map(Rule::raw), Some("Bash(git)"));
    }
}

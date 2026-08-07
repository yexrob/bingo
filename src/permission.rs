use crate::tool::Tool;

/// Permission modes: default/acceptEdits/auto/bypassPermissions/dontAsk/plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    BypassPermissions,
    DontAsk,
    Plan,
}

impl std::str::FromStr for PermissionMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "default" => Ok(Self::Default),
            "acceptEdits" => Ok(Self::AcceptEdits),
            "bypassPermissions" => Ok(Self::BypassPermissions),
            "dontAsk" => Ok(Self::DontAsk),
            "plan" => Ok(Self::Plan),
            other => Err(format!(
                "unknown permission mode: {other} (expected default|acceptEdits|bypassPermissions|dontAsk|plan)"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionBehavior {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionResult {
    pub behavior: PermissionBehavior,
    pub reason: String,
}

fn deny(reason: impl Into<String>) -> PermissionResult {
    PermissionResult {
        behavior: PermissionBehavior::Deny,
        reason: reason.into(),
    }
}

fn ask(reason: impl Into<String>) -> PermissionResult {
    PermissionResult {
        behavior: PermissionBehavior::Ask,
        reason: reason.into(),
    }
}

/// Rule table semantics: deny/ask hold if any sub-command matches; allow requires all
/// sub-commands to match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchMode {
    /// deny / ask: holds on any match (fail closed).
    Any,
    /// allow: holds only when all match (fail closed).
    All,
}

/// Split on shell sequencing operators: `&&` `||` `;` `|` `&` newline,
/// plus sub-shell / command-substitution delimiters `(` `)` `` ` `` `{` `}`.
/// Separators inside quotes aren't split. Returns (sub-commands, trusted) — an
/// unterminated quote is untrusted and the caller never lets allow rules through.
fn split_shell_commands(command: &str) -> (Vec<String>, bool) {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(q) = quote {
            current.push(c);
            if c == q {
                quote = None;
            } else if q == '"' && c == '\\' {
                // Escaped char inside double quotes: the next char doesn't close the quote.
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            continue;
        }
        match c {
            '\'' | '"' => {
                quote = Some(c);
                current.push(c);
            }
            '\\' => {
                current.push(c);
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ';' | '\n' | '(' | ')' | '`' | '{' | '}' => parts.push(std::mem::take(&mut current)),
            '&' | '|' => {
                if chars.peek() == Some(&c) {
                    chars.next();
                }
                parts.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    parts.push(current);
    let parts = parts
        .into_iter()
        // The `$` of `$(` ends up at the tail of the previous segment; strip it to get the real command.
        .map(|p| p.trim().trim_end_matches('$').trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    (parts, quote.is_none())
}

/// Bash rule matching: prefix-match each sub-command.
/// Whole-string prefix matching can be bypassed by `cd /tmp && rm -rf /`,
/// and would let `Bash(ls)` pass `ls; rm -rf ~`.
fn bash_content_matches(command: &str, content: &str, mode: MatchMode) -> bool {
    let (parts, trusted) = split_shell_commands(command);
    match mode {
        // deny/ask: any sub-command match holds; fall back to whole-string match when splitting fails.
        MatchMode::Any => {
            parts.iter().any(|p| p.starts_with(content)) || command.trim().starts_with(content)
        }
        // allow: all sub-commands must match; an untrusted split never allows.
        MatchMode::All => trusted && !parts.is_empty() && parts.iter().all(|p| p.starts_with(content)),
    }
}

/// Path normalization: `~` expansion, relative paths expanded against the process cwd,
/// `.` and `..` resolved.
/// No filesystem lookups (rules must hold for non-existent paths too).
fn normalize_path(path: &str) -> String {
    use std::path::{Component, PathBuf};
    let expanded = match path.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => path.to_string(),
        },
        None => path.to_string(),
    };
    let raw = std::path::Path::new(&expanded);
    let mut out = if raw.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir().unwrap_or_default()
    };
    for component in raw.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    let normalized = out.to_string_lossy().into_owned();
    // Normalization eats the trailing slash; directory rules (`Read(/etc/)`) need the
    // boundary semantics preserved.
    if path.ends_with('/') && !normalized.ends_with('/') {
        format!("{normalized}/")
    } else {
        normalized
    }
}

/// Rule content matching: whether `Tool(content)`'s content holds for the current call.
/// Bash matches sub-commands by command prefix; file tools match path prefixes after
/// normalization; WebFetch supports `domain:` and URL prefixes; Skill matches exact
/// or `name:*` prefix; `*` matches everything; `prefix:` is ignored.
fn content_matches(
    tool_name: &str,
    input: &serde_json::Value,
    content: &str,
    mode: MatchMode,
) -> bool {
    // Skill rules: `Skill(name)` exact; `Skill(name:*)` prefix; `*` matches everything.
    if tool_name == "Skill" {
        let name = input.get("skill").and_then(|v| v.as_str());
        return match content {
            "*" => true,
            c if c.ends_with(":*") => {
                let prefix = &c[..c.len() - 2];
                name.is_some_and(|n| n.starts_with(prefix))
            }
            c => name.is_some_and(|n| n == c),
        };
    }
    let content = content.strip_prefix("prefix:").unwrap_or(content);
    // CC rule syntax `Bash(git push:*)`: the trailing `:*` is a prefix
    // wildcard as a unit. Strip it whole first — the bare-`*` trim below
    // would leave a dangling colon (`git push:`) that never matches.
    let content = content
        .strip_suffix(":*")
        .unwrap_or(content)
        .trim_end_matches('*');
    if content.is_empty() {
        return true;
    }
    if tool_name == "Bash" {
        return input
            .get("command")
            .and_then(|v| v.as_str())
            .is_some_and(|command| bash_content_matches(command, content, mode));
    }
    if matches!(tool_name, "Read" | "Edit" | "Write" | "Grep" | "Glob") {
        return input
            .get("file_path")
            .or_else(|| input.get("path"))
            .and_then(|v| v.as_str())
            .is_some_and(|target| {
                normalize_path(target).starts_with(&normalize_path(content))
            });
    }
    let target = match tool_name {
        "WebFetch" => {
            let url = input.get("url").and_then(|v| v.as_str());
            if let Some(domain) = content.strip_prefix("domain:")
                && let Some(url) = url
                && let Ok(parsed) = url::Url::parse(url)
                && let Some(host) = parsed.host_str()
            {
                // domain: rules match on hostname.
                return host == domain.trim_end_matches('*');
            }
            url
        }
        _ => None,
    };
    target.is_some_and(|t| t.starts_with(content))
}

/// Whether rule `Tool(content)` matches the current tool call.
/// The `mcp__server` form matches all tools of that server.
fn rule_matches(
    rule: &str,
    tool_name: &str,
    input: &serde_json::Value,
    mode: MatchMode,
) -> bool {
    if let Some(open) = rule.find('(') {
        let rule_tool = rule[..open].trim();
        let rest = &rule[open + 1..];
        let Some(close) = rest.rfind(')') else {
            return false;
        };
        let content = &rest[..close];
        if rule_tool != tool_name {
            return false;
        }
        content_matches(tool_name, input, content, mode)
    } else if rule.contains("__") {
        // mcp__server rule: prefix-match the tool name.
        tool_name.starts_with(rule)
    } else {
        rule == tool_name
    }
}

fn rule_hits(
    rules: &[String],
    tool_name: &str,
    input: &serde_json::Value,
    mode: MatchMode,
) -> bool {
    rules.iter().any(|r| rule_matches(r, tool_name, input, mode))
}

/// safetyCheck sensitive dirs: a write tool targeting inside these dirs → must prompt
/// (immune to bypass).
const SENSITIVE_DIRS: &[&str] = &[".git", ".claude", ".vscode", ".idea"];

fn safety_check(tool: &dyn Tool, input: &serde_json::Value) -> Option<String> {
    if !tool.is_destructive(input) {
        return None;
    }
    let target = input.get("file_path").and_then(|v| v.as_str())?;
    let path = std::path::Path::new(target);
    let sensitive = path.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| SENSITIVE_DIRS.contains(&s))
    });
    sensitive.then(|| format!("writing into a sensitive path: {target}"))
}

/// Unified permission gate: mode × rule tables × tool properties → allow/deny/ask.
pub fn can_use_tool(
    tool: &dyn Tool,
    input: &serde_json::Value,
    mode: PermissionMode,
    rules: &[String],
    ask_rules: &[String],
    allow_rules: &[String],
) -> PermissionResult {
    let name = tool.name();
    // 1. deny rules (whole tool or content match): any sub-command hit denies.
    if rule_hits(rules, &name, input, MatchMode::Any) {
        return deny(format!("denied by permission rule: {name}"));
    }
    // 2. ask rules: respected even in bypass mode (content-ask exception)
    if rule_hits(ask_rules, &name, input, MatchMode::Any) {
        return ask(format!("permission rule requires confirmation: {name}"));
    }
    // 2b. WebFetch preapproved domains auto-allow.
    //     Note: malformed calls without a url field don't hit, continue with the
    //     remaining checks.
    if name == "WebFetch"
        && let Some(url) = input.get("url").and_then(|v| v.as_str())
        && crate::preapproved::is_preapproved_url(url)
    {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: "preapproved host".into(),
        };
    }
    // 3. Read-only tools pass directly. Two exceptions:
    //    WebFetch (non-preapproved domains still need user approval);
    //    MCP tools (readOnlyHint is server-reported, untrusted input; must not
    //    short-circuit the permission gate).
    if tool.is_read_only(input) && name != "WebFetch" && !name.starts_with("mcp__") {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: "read-only tool".into(),
        };
    }
    // 4. safetyCheck: sensitive paths, bypass-immune, must prompt
    if let Some(reason) = safety_check(tool, input) {
        return ask(reason);
    }
    // 5. bypass check
    if mode == PermissionMode::BypassPermissions {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: "bypassPermissions mode".into(),
        };
    }
    // 6. acceptEdits: edit tools auto-allowed
    if mode == PermissionMode::AcceptEdits && tool.is_edit_tool(input) {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: "acceptEdits mode".into(),
        };
    }
    // 7. allow rules: Bash needs every sub-command to match.
    if rule_hits(allow_rules, &name, input, MatchMode::All) {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: format!("allowed by permission rule: {name}"),
        };
    }
    match mode {
        PermissionMode::DontAsk => deny("dontAsk mode denies non-read-only tools"),
        // Task tool family exemption (plan mode prompts "create a task list to track
        // the work"): plan mode allows creating/editing task lists, other non-read-only
        // tools are denied as usual.
        PermissionMode::Plan if name.starts_with("Task") => PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: "plan mode allows task list management".into(),
        },
        PermissionMode::Plan => deny("plan mode denies tool execution"),
        _ => {
            let mut reason = format!("{name} needs permission");
            if tool.is_destructive(input) {
                reason.push_str(" (destructive)");
            }
            ask(reason)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::edit::EditTool;
    use crate::tool::read::ReadTool;
    use crate::tool::write::WriteTool;

    fn decide(
        tool: &dyn Tool,
        input: serde_json::Value,
        mode: PermissionMode,
        rules: &[&str],
    ) -> PermissionResult {
        let all: Vec<String> = rules.iter().map(|s| s.to_string()).collect();
        can_use_tool(tool, &input, mode, &all, &[], &[])
    }

    #[test]
    fn read_only_always_allowed() {
        let tool = ReadTool::new();
        let input = serde_json::json!({"file_path": "Cargo.toml"});
        let result = decide(&tool as &dyn Tool, input, PermissionMode::Default, &[]);
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[test]
    fn bypass_allows_everything() {
        let tool = WriteTool;
        let input = serde_json::json!({"file_path": "/tmp/x.txt", "content": "hi"});
        let result = decide(&tool as &dyn Tool, input, PermissionMode::BypassPermissions, &[]);
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[test]
    fn write_asks_in_default_but_auto_allows_in_accept_edits() {
        let tool = WriteTool;
        let input = || serde_json::json!({"file_path": "/tmp/x.txt", "content": "hi"});
        let result = decide(&tool as &dyn Tool, input(), PermissionMode::Default, &[]);
        assert_eq!(result.behavior, PermissionBehavior::Ask);
        let result = decide(&tool as &dyn Tool, input(), PermissionMode::AcceptEdits, &[]);
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[test]
    fn edit_is_not_auto_allowed_in_accept_edits_with_sensitive_path() {
        let tool = EditTool;
        let input = serde_json::json!({
            "file_path": "repo/.git/config",
            "old_string": "a",
            "new_string": "b"
        });
        // safetyCheck takes precedence over acceptEdits (bypass-immune)
        let result = decide(&tool as &dyn Tool, input, PermissionMode::AcceptEdits, &[]);
        assert_eq!(result.behavior, PermissionBehavior::Ask);
    }

    #[test]
    fn skill_rules_match_exact_and_prefix() {
        let tool = crate::tool::skill::SkillTool::new(vec![]);
        let input = || serde_json::json!({"skill": "review-pr"});
        let allow = |rules: &[&str]| {
            let all: Vec<String> = rules.iter().map(|s| s.to_string()).collect();
            can_use_tool(&tool as &dyn Tool, &input(), PermissionMode::Default, &[], &[], &all)
        };
        // No rules → ask (skill execution is non-read-only)
        let result = allow(&[]);
        assert_eq!(result.behavior, PermissionBehavior::Ask);
        // Exact match
        let result = allow(&["Skill(review-pr)"]);
        assert_eq!(result.behavior, PermissionBehavior::Allow);
        // Prefix match (CC `review:*` semantics)
        let result = allow(&["Skill(review:*)"]);
        assert_eq!(result.behavior, PermissionBehavior::Allow);
        // Wildcard
        let result = allow(&["Skill(*)"]);
        assert_eq!(result.behavior, PermissionBehavior::Allow);
        // A non-matching exact rule doesn't pass
        let result = allow(&["Skill(commit)"]);
        assert_eq!(result.behavior, PermissionBehavior::Ask);
    }

    #[test]
    fn deny_rule_beats_everything() {
        let tool = crate::tool::bash::BashTool::new();
        let input = serde_json::json!({"command": "rm -rf /tmp/x"});
        let result = decide(
            &tool as &dyn Tool,
            input,
            PermissionMode::BypassPermissions,
            &["Bash(rm -rf)"],
        );
        assert_eq!(result.behavior, PermissionBehavior::Deny);
    }

    #[test]
    fn ask_rule_respected_even_in_bypass() {
        let tool = crate::tool::bash::BashTool::new();
        let input = serde_json::json!({"command": "git push"});
        let all = vec!["Bash(git push)".to_string()];
        let result = can_use_tool(
            &tool as &dyn Tool,
            &input,
            PermissionMode::BypassPermissions,
            &[],
            &all,
            &[],
        );
        assert_eq!(result.behavior, PermissionBehavior::Ask);
    }

    #[test]
    fn allow_rule_permits_bash_in_default() {
        let tool = crate::tool::bash::BashTool::new();
        let input = serde_json::json!({"command": "git status"});
        let all = vec!["Bash(git)".to_string()];
        let result = can_use_tool(
            &tool as &dyn Tool,
            &input,
            PermissionMode::Default,
            &[],
            &[],
            &all,
        );
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[test]
    fn colon_star_wildcard_matches_as_prefix() {
        // `Bash(git push:*)` is the documented CC syntax; the `:*` suffix
        // must strip as a unit (a leftover colon would never match).
        let tool = crate::tool::bash::BashTool::new();
        let input = serde_json::json!({"command": "git push origin main"});
        let result = decide(
            &tool as &dyn Tool,
            input,
            PermissionMode::Default,
            &["Bash(git push:*)"],
        );
        assert_eq!(result.behavior, PermissionBehavior::Deny);

        // Prefix scope stays tight: `git pull` is not `git push:*`.
        let input = serde_json::json!({"command": "git pull"});
        let result = decide(
            &tool as &dyn Tool,
            input,
            PermissionMode::Default,
            &["Bash(git push:*)"],
        );
        assert_ne!(result.behavior, PermissionBehavior::Deny);

        // Allow side works too. (Pipelines still need one rule to cover
        // every sub-command — cross-rule union is intentionally not granted.)
        let tool = crate::tool::bash::BashTool::new();
        let input = serde_json::json!({"command": "git log --oneline"});
        let all = vec!["Bash(git log:*)".to_string()];
        let result = can_use_tool(
            &tool as &dyn Tool,
            &input,
            PermissionMode::Default,
            &[],
            &[],
            &all,
        );
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[test]
    fn webfetch_preapproved_host_auto_allows() {
        let tool = crate::tool::webfetch::WebFetchTool;
        let input = serde_json::json!({"url": "https://doc.rust-lang.org/book/"});
        let result = can_use_tool(
            &tool as &dyn Tool,
            &input,
            PermissionMode::Default,
            &[],
            &[],
            &[],
        );
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[test]
    fn webfetch_unknown_host_asks() {
        let tool = crate::tool::webfetch::WebFetchTool;
        let input = serde_json::json!({"url": "https://example.com/page"});
        let result = can_use_tool(
            &tool as &dyn Tool,
            &input,
            PermissionMode::Default,
            &[],
            &[],
            &[],
        );
        assert_eq!(result.behavior, PermissionBehavior::Ask);
    }

    fn bash_decision(command: &str, deny_rules: &[&str], allow: &[&str]) -> PermissionResult {
        let tool = crate::tool::bash::BashTool::new();
        let input = serde_json::json!({ "command": command });
        can_use_tool(
            &tool as &dyn Tool,
            &input,
            PermissionMode::Default,
            &deny_rules.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            &[],
            &allow.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
    }

    /// Security regression: deny rules must not be bypassable via sequencing operators.
    #[test]
    fn deny_rule_matches_any_sub_command() {
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
                bash_decision(command, &["Bash(rm)"], &[]).behavior,
                PermissionBehavior::Deny,
                "deny 应命中: {command}"
            );
        }
        // Separators inside quotes aren't operators and must not create false hits.
        assert_eq!(
            bash_decision("echo 'a; b'", &["Bash(b)"], &[]).behavior,
            PermissionBehavior::Ask,
            "引号内文本不切分为子命令"
        );
    }

    /// Security regression: allow rules only pass when every sub-command matches.
    #[test]
    fn allow_rule_requires_every_sub_command_to_match() {
        // Single command: passes as usual.
        assert_eq!(
            bash_decision("ls -la", &[], &["Bash(ls)"]).behavior,
            PermissionBehavior::Allow
        );
        // Second appended command isn't covered by the rule → must ask.
        for command in [
            "ls; rm -rf ~",
            "ls && rm -rf ~",
            "ls | rm -rf ~",
            "ls & rm -rf ~",
            "ls\nrm -rf ~",
        ] {
            assert_eq!(
                bash_decision(command, &[], &["Bash(ls)"]).behavior,
                PermissionBehavior::Ask,
                "不应免询问放行: {command}"
            );
        }
        // All sub-commands match → pass.
        assert_eq!(
            bash_decision("ls -la && ls /tmp", &[], &["Bash(ls)"]).behavior,
            PermissionBehavior::Allow
        );
        // Unterminated quote (untrusted split) → no pass.
        assert_eq!(
            bash_decision("ls \"; rm -rf ~", &[], &["Bash(ls)"]).behavior,
            PermissionBehavior::Ask,
            "切分不可信时不放行"
        );
    }

    #[test]
    fn shell_splitter_keeps_quoted_separators() {
        let (parts, trusted) = split_shell_commands("echo 'a; b' && ls");
        assert_eq!(parts, vec!["echo 'a; b'".to_string(), "ls".to_string()]);
        assert!(trusted);
        let (_, trusted) = split_shell_commands("echo \"unterminated");
        assert!(!trusted, "引号不闭合 → 不可信");
        let (parts, _) = split_shell_commands("cd /tmp && rm -rf / ; echo done");
        assert_eq!(parts.len(), 3, "{parts:?}");
    }

    /// Security regression: path rules normalize before matching; `..` can't cross directory boundaries.
    #[test]
    fn file_rules_normalize_paths() {
        // Absolute roots differ per platform: `/etc` on Unix, `C:\etc` on Windows (a
        // Unix-style `/etc/passwd` isn't absolute on Windows and would never match).
        #[cfg(windows)]
        let (etc, other) = ("C:\\etc", "D:\\var");
        #[cfg(not(windows))]
        let (etc, other) = ("/etc", "/var");
        let tool = ReadTool::new();
        let denied = |path: &str| {
            can_use_tool(
                &tool as &dyn Tool,
                &serde_json::json!({ "file_path": path }),
                PermissionMode::Default,
                &[format!("Read({etc}/)")],
                &[],
                &[],
            )
            .behavior
        };
        assert_eq!(denied(&format!("{etc}/passwd")), PermissionBehavior::Deny);
        // `../etc` (not `../{etc}`): repeating the Windows drive letter (`C:\etc\..\C:\etc`)
        // would normalize to a garbage `C:\C:\etc` path instead of the drive root.
        assert_eq!(denied(&format!("{etc}/../etc/passwd")), PermissionBehavior::Deny);
        assert_eq!(denied(&format!("{etc}/./ssh/../passwd")), PermissionBehavior::Deny);
        // Paths outside the directory are unaffected (read-only tools pass).
        assert_eq!(denied(&format!("{other}/log/x")), PermissionBehavior::Allow);
        // Relative paths expand against cwd, then match against absolute rules.
        let cwd = std::env::current_dir().unwrap_or_default();
        let rule = format!("Read({})", cwd.join("src").to_string_lossy());
        let hit = can_use_tool(
            &tool as &dyn Tool,
            &serde_json::json!({ "file_path": "./src/main.rs" }),
            PermissionMode::Default,
            &[rule],
            &[],
            &[],
        );
        assert_eq!(hit.behavior, PermissionBehavior::Deny);
    }

    /// MCP server-reported readOnlyHint must not short-circuit the permission gate (untrusted input).
    #[test]
    fn mcp_read_only_hint_does_not_bypass_permission_gate() {
        struct FakeMcpTool;
        #[async_trait::async_trait]
        impl Tool for FakeMcpTool {
            fn name(&self) -> String {
                "mcp__srv__peek".into()
            }
            fn description(&self) -> String {
                String::new()
            }
            fn input_schema(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            fn is_read_only(&self, _input: &serde_json::Value) -> bool {
                true
            }
            async fn call(
                &self,
                _input: serde_json::Value,
                _ctx: &crate::tool::ToolContext,
            ) -> Result<crate::tool::ToolResult, crate::tool::ToolError> {
                Ok(Default::default())
            }
        }
        let tool = FakeMcpTool;
        let input = serde_json::json!({});
        let result = can_use_tool(&tool as &dyn Tool, &input, PermissionMode::Default, &[], &[], &[]);
        assert_eq!(
            result.behavior,
            PermissionBehavior::Ask,
            "readOnlyHint 不再免询问"
        );
        // An explicit allow rule can still pass.
        let allow = vec!["mcp__srv".to_string()];
        let result = can_use_tool(
            &tool as &dyn Tool,
            &input,
            PermissionMode::Default,
            &[],
            &[],
            &allow,
        );
        assert_eq!(result.behavior, PermissionBehavior::Allow);
        // Built-in read-only tools are unaffected.
        let read = ReadTool::new();
        let result = can_use_tool(
            &read as &dyn Tool,
            &serde_json::json!({"file_path": "Cargo.toml"}),
            PermissionMode::Default,
            &[],
            &[],
            &[],
        );
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[test]
    fn webfetch_domain_rule_matches_hostname() {
        let tool = crate::tool::webfetch::WebFetchTool;
        let input = serde_json::json!({"url": "https://internal.example.com/docs"});
        let all = vec!["WebFetch(domain:internal.example.com)".to_string()];
        let result = can_use_tool(
            &tool as &dyn Tool,
            &input,
            PermissionMode::Default,
            &[],
            &[],
            &all,
        );
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }
}

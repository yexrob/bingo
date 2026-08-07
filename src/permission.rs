use crate::tool::Tool;

/// 权限模式：default/acceptEdits/auto/bypassPermissions/dontAsk/plan。
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

/// 规则表语义：deny/ask 只要任一子命令命中即成立；allow 要求全部子命令命中。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchMode {
    /// deny / ask：命中任一即成立（fail closed）。
    Any,
    /// allow：全部命中才成立（fail closed）。
    All,
}

/// shell 顺序操作符切分：`&&` `||` `;` `|` `&` 换行，
/// 外加子 shell / 命令替换定界符 `(` `)` `` ` `` `{` `}`。
/// 引号内的分隔符不切。返回 (子命令, 是否可信)——引号不闭合时不可信，
/// 调用方对 allow 规则一律不放行。
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
                // 双引号内的转义：下一个字符不结束引号。
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
        // `$(` 的 `$` 落在前一段尾部，去掉后才是真正的命令。
        .map(|p| p.trim().trim_end_matches('$').trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    (parts, quote.is_none())
}

/// Bash 规则匹配：对每个子命令做前缀匹配。
/// 整串前缀匹配会被 `cd /tmp && rm -rf /` 绕过 deny，
/// 也会让 `Bash(ls)` 放行 `ls; rm -rf ~`。
fn bash_content_matches(command: &str, content: &str, mode: MatchMode) -> bool {
    let (parts, trusted) = split_shell_commands(command);
    match mode {
        // deny/ask：任一子命令命中即成立；切不动时对整串兜底匹配。
        MatchMode::Any => {
            parts.iter().any(|p| p.starts_with(content)) || command.trim().starts_with(content)
        }
        // allow：全部子命令命中才放行；切分不可信一律不放行。
        MatchMode::All => trusted && !parts.is_empty() && parts.iter().all(|p| p.starts_with(content)),
    }
}

/// 路径归一化：`~` 展开、相对路径按进程 cwd 展开、消解 `.` 与 `..`。
/// 不查文件系统（规则对不存在的路径同样要成立）。
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
    // 归一化会吃掉结尾斜杠，目录规则（`Read(/etc/)`）要保留边界语义。
    if path.ends_with('/') && !normalized.ends_with('/') {
        format!("{normalized}/")
    } else {
        normalized
    }
}

/// 规则内容匹配：`Tool(content)` 的 content 对当前调用是否成立。
/// Bash 按子命令匹配命令前缀；文件类工具归一化路径后匹配路径前缀；
/// WebFetch 支持 `domain:` 与 URL 前缀；Skill 精确/`name:*` 前缀匹配；
/// `*` 匹配一切；`prefix:` 前缀忽略。
fn content_matches(
    tool_name: &str,
    input: &serde_json::Value,
    content: &str,
    mode: MatchMode,
) -> bool {
    // Skill 规则：`Skill(name)` 精确；`Skill(name:*)` 前缀；`*` 匹配一切。
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
                // domain: 规则按 hostname 匹配。
                return host == domain.trim_end_matches('*');
            }
            url
        }
        _ => None,
    };
    target.is_some_and(|t| t.starts_with(content))
}

/// 规则 `Tool(content)` 对当前工具调用是否匹配。
/// `mcp__server` 形式：匹配该 server 全部工具。
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
        // mcp__server 规则：工具名前缀匹配
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

/// safetyCheck 敏感目录：写工具目标落在这些目录内 → 必须提示（bypass 免疫）。
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

/// 统一权限门：模式 × 规则表 × 工具属性 → allow/deny/ask。
pub fn can_use_tool(
    tool: &dyn Tool,
    input: &serde_json::Value,
    mode: PermissionMode,
    rules: &[String],
    ask_rules: &[String],
    allow_rules: &[String],
) -> PermissionResult {
    let name = tool.name();
    // 1. deny 规则（整工具或内容匹配）：任一子命令命中即拒。
    if rule_hits(rules, &name, input, MatchMode::Any) {
        return deny(format!("denied by permission rule: {name}"));
    }
    // 2. ask 规则：bypass 模式也尊重（内容 ask 例外）
    if rule_hits(ask_rules, &name, input, MatchMode::Any) {
        return ask(format!("permission rule requires confirmation: {name}"));
    }
    // 2b. WebFetch 预批准域名自动放行。
    //     注意：无 url 字段的畸形调用不命中，继续走后续检查。
    if name == "WebFetch"
        && let Some(url) = input.get("url").and_then(|v| v.as_str())
        && crate::preapproved::is_preapproved_url(url)
    {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: "preapproved host".into(),
        };
    }
    // 3. 只读工具直接放行。两个例外：
    //    WebFetch（非预批准域名仍需用户批准）；
    //    MCP 工具（readOnlyHint 由服务器自报，是不可信输入，不得短路权限门）。
    if tool.is_read_only(input) && name != "WebFetch" && !name.starts_with("mcp__") {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: "read-only tool".into(),
        };
    }
    // 4. safetyCheck：敏感路径，bypass 免疫，必须提示
    if let Some(reason) = safety_check(tool, input) {
        return ask(reason);
    }
    // 5. bypass 检查
    if mode == PermissionMode::BypassPermissions {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: "bypassPermissions mode".into(),
        };
    }
    // 6. acceptEdits：编辑类工具自动允许
    if mode == PermissionMode::AcceptEdits && tool.is_edit_tool(input) {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: "acceptEdits mode".into(),
        };
    }
    // 7. allow 规则：Bash 需要全部子命令命中才放行。
    if rule_hits(allow_rules, &name, input, MatchMode::All) {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: format!("allowed by permission rule: {name}"),
        };
    }
    match mode {
        PermissionMode::DontAsk => deny("dontAsk mode denies non-read-only tools"),
        // Task 工具族豁免（plan 模式提示 "create a task list to track the work"）：
        // plan 模式允许建/改任务列表，其余非只读工具照常 deny。
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
        // safetyCheck 优先于 acceptEdits（bypass 免疫）
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
        // 无规则 → 询问（技能执行非只读）
        let result = allow(&[]);
        assert_eq!(result.behavior, PermissionBehavior::Ask);
        // 精确匹配
        let result = allow(&["Skill(review-pr)"]);
        assert_eq!(result.behavior, PermissionBehavior::Allow);
        // 前缀匹配（CC `review:*` 语义）
        let result = allow(&["Skill(review:*)"]);
        assert_eq!(result.behavior, PermissionBehavior::Allow);
        // 通配
        let result = allow(&["Skill(*)"]);
        assert_eq!(result.behavior, PermissionBehavior::Allow);
        // 不匹配的精确规则不放过
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

    /// 安全回归：deny 规则不得被顺序操作符绕过。
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
        // 引号内的分隔符不是操作符，也不该造出假命中。
        assert_eq!(
            bash_decision("echo 'a; b'", &["Bash(b)"], &[]).behavior,
            PermissionBehavior::Ask,
            "引号内文本不切分为子命令"
        );
    }

    /// 安全回归：allow 规则必须全部子命令命中才放行。
    #[test]
    fn allow_rule_requires_every_sub_command_to_match() {
        // 单命令：照常放行。
        assert_eq!(
            bash_decision("ls -la", &[], &["Bash(ls)"]).behavior,
            PermissionBehavior::Allow
        );
        // 追加的第二条命令未被规则覆盖 → 必须询问。
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
        // 全部子命令命中 → 放行。
        assert_eq!(
            bash_decision("ls -la && ls /tmp", &[], &["Bash(ls)"]).behavior,
            PermissionBehavior::Allow
        );
        // 引号不闭合（切分不可信）→ 不放行。
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

    /// 安全回归：路径规则匹配前归一化，`..` 不能绕过目录边界。
    #[test]
    fn file_rules_normalize_paths() {
        let tool = ReadTool::new();
        let denied = |path: &str| {
            can_use_tool(
                &tool as &dyn Tool,
                &serde_json::json!({ "file_path": path }),
                PermissionMode::Default,
                &["Read(/etc/)".to_string()],
                &[],
                &[],
            )
            .behavior
        };
        assert_eq!(denied("/etc/passwd"), PermissionBehavior::Deny);
        assert_eq!(denied("/etc/../etc/passwd"), PermissionBehavior::Deny);
        assert_eq!(denied("/etc/./ssh/../passwd"), PermissionBehavior::Deny);
        // 目录外的路径不受影响（只读工具放行）。
        assert_eq!(denied("/var/log/x"), PermissionBehavior::Allow);
        // 相对路径按 cwd 展开后与绝对规则对表。
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

    /// MCP 服务器自报的 readOnlyHint 不得短路权限门（不可信输入）。
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
        // 显式 allow 规则仍可放行。
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
        // 内置只读工具不受影响。
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

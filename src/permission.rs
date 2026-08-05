use crate::tool::Tool;

/// 权限模式（对标 Claude Code：default/acceptEdits/auto/bypassPermissions/dontAsk/plan）。
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

/// 规则内容匹配：`Tool(content)` 的 content 对当前调用是否成立。
/// Bash 匹配命令前缀；文件类工具匹配路径前缀；WebFetch 支持 `domain:` 与 URL 前缀；
/// `*` 匹配一切；`prefix:` 前缀忽略。
fn content_matches(tool_name: &str, input: &serde_json::Value, content: &str) -> bool {
    let content = content
        .strip_prefix("prefix:")
        .unwrap_or(content)
        .trim_end_matches('*');
    if content.is_empty() {
        return true;
    }
    let target = match tool_name {
        "Bash" => input.get("command").and_then(|v| v.as_str()),
        "Read" | "Edit" | "Write" | "Grep" | "Glob" => input
            .get("file_path")
            .or_else(|| input.get("path"))
            .and_then(|v| v.as_str()),
        "WebFetch" => {
            let url = input.get("url").and_then(|v| v.as_str());
            if let Some(domain) = content.strip_prefix("domain:")
                && let Some(url) = url
                && let Ok(parsed) = url::Url::parse(url)
                && let Some(host) = parsed.host_str()
            {
                // domain: 规则按 hostname 匹配（对标 Claude Code WebFetchTool）。
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
fn rule_matches(rule: &str, tool_name: &str, input: &serde_json::Value) -> bool {
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
        content_matches(tool_name, input, content)
    } else if rule.contains("__") {
        // mcp__server 规则：工具名前缀匹配
        tool_name.starts_with(rule)
    } else {
        rule == tool_name
    }
}

fn rule_hits(rules: &[String], tool_name: &str, input: &serde_json::Value) -> bool {
    rules.iter().any(|r| rule_matches(r, tool_name, input))
}

/// safetyCheck 敏感目录（对标 Claude Code）：写工具目标落在这些目录内 → 必须提示（bypass 免疫）。
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

/// 统一权限门：模式 × 规则表 × 工具属性 → allow/deny/ask（对标 Claude Code 判定顺序）。
pub fn can_use_tool(
    tool: &dyn Tool,
    input: &serde_json::Value,
    mode: PermissionMode,
    rules: &[String],
    ask_rules: &[String],
    allow_rules: &[String],
) -> PermissionResult {
    let name = tool.name();
    // 1. deny 规则（整工具或内容匹配）
    if rule_hits(rules, &name, input) {
        return deny(format!("denied by permission rule: {name}"));
    }
    // 2. ask 规则：bypass 模式也尊重（对标 Claude Code 内容 ask 例外）
    if rule_hits(ask_rules, &name, input) {
        return ask(format!("permission rule requires confirmation: {name}"));
    }
    // 2b. WebFetch 预批准域名自动放行（对标 Claude Code isPreapprovedHost）。
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
    // 3. 只读工具直接放行（WebFetch 例外：非预批准域名仍需用户批准，对标 Claude Code）
    if tool.is_read_only(input) && name != "WebFetch" {
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
    // 6. acceptEdits：编辑类工具自动允许（对标 Claude Code）
    if mode == PermissionMode::AcceptEdits && tool.is_edit_tool(input) {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: "acceptEdits mode".into(),
        };
    }
    // 7. allow 规则
    if rule_hits(allow_rules, &name, input) {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: format!("allowed by permission rule: {name}"),
        };
    }
    match mode {
        PermissionMode::DontAsk => deny("dontAsk mode denies non-read-only tools"),
        // Task 工具族豁免（对标 CC plan mode 提示 "create a task list to track the work"）：
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

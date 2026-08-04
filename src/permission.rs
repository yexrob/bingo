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

/// 统一权限门：模式 × 工具属性 → allow/deny/ask。
/// ask 需要交互决策（headless 下由调用方降级）。
pub fn can_use_tool(
    tool: &dyn Tool,
    input: &serde_json::Value,
    mode: PermissionMode,
) -> PermissionResult {    if mode == PermissionMode::BypassPermissions {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: "bypassPermissions mode".into(),
        };
    }
    if tool.is_read_only(input) {
        return PermissionResult {
            behavior: PermissionBehavior::Allow,
            reason: "read-only tool".into(),
        };
    }
    match mode {
        PermissionMode::DontAsk => deny("dontAsk mode denies non-read-only tools"),
        PermissionMode::Plan => deny("plan mode denies tool execution"),
        _ => {
            let mut reason = format!("{} needs permission", tool.name());
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
    use crate::tool::read::ReadTool;

    #[test]
    fn read_only_always_allowed() {
        let tool = ReadTool::new();
        let input = serde_json::json!({"file_path": "Cargo.toml"});
        let result = can_use_tool(&tool as &dyn Tool, &input, PermissionMode::Default);
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[test]
    fn bypass_allows_everything() {
        let tool = ReadTool::new();
        let input = serde_json::json!({"file_path": "Cargo.toml"});
        let result = can_use_tool(&tool as &dyn Tool, &input, PermissionMode::BypassPermissions);
        assert_eq!(result.behavior, PermissionBehavior::Allow);
    }

    #[test]
    fn write_tool_asks_in_default_mode() {
        let tool = crate::tool::bash::BashTool::new();
        let input = serde_json::json!({"command": "ls"});
        let result = can_use_tool(&tool as &dyn Tool, &input, PermissionMode::Default);
        assert_eq!(result.behavior, PermissionBehavior::Ask);
    }

    #[test]
    fn dont_ask_denies_write() {
        let tool = crate::tool::bash::BashTool::new();
        let input = serde_json::json!({"command": "ls"});
        let result = can_use_tool(&tool as &dyn Tool, &input, PermissionMode::DontAsk);
        assert_eq!(result.behavior, PermissionBehavior::Deny);
    }
}

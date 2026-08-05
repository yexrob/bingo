use std::collections::HashMap;

use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("failed to read settings: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse settings: {0}")]
    Parse(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Settings {
    #[serde(rename = "permissionMode")]
    pub permission_mode: Option<String>,
    /// TUI 主题：auto（跟随终端背景）/ dark / light。默认 auto。
    pub theme: Option<String>,
    /// 发送 cache_control（prompt caching）。默认关闭：非官方端点处理不稳定。
    #[serde(rename = "cacheControl")]
    pub cache_control: Option<bool>,
    pub hooks: HooksConfig,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    /// 权限规则表（对标 Claude Code permissions.allow/deny/ask，规则语法 `Tool(content)`）。
    pub permissions: PermissionRules,
}

/// 权限规则（对标 Claude Code settings permissions 段）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct PermissionRules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub ask: Vec<String>,
}

/// MCP server 定义（对标 Claude Code mcpServers）。
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    #[serde(rename = "PreToolUse")]
    pub pre_tool_use: Vec<HookRule>,
    #[serde(rename = "PostToolUse")]
    pub post_tool_use: Vec<HookRule>,
    #[serde(rename = "PreCompact")]
    pub pre_compact: Vec<HookRule>,
    #[serde(rename = "PostCompact")]
    pub post_compact: Vec<HookRule>,
    #[serde(rename = "UserPromptSubmit")]
    pub user_prompt_submit: Vec<HookRule>,
    #[serde(rename = "Stop")]
    pub stop: Vec<HookRule>,
    #[serde(rename = "SessionStart")]
    pub session_start: Vec<HookRule>,
    #[serde(rename = "SessionEnd")]
    pub session_end: Vec<HookRule>,
    #[serde(rename = "TaskCreated")]
    pub task_created: Vec<HookRule>,
    #[serde(rename = "TaskCompleted")]
    pub task_completed: Vec<HookRule>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HookRule {
    #[serde(default)]
    pub matcher: String,
    pub hooks: Vec<Hook>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Hook {
    #[serde(rename = "type")]
    pub kind: String,
    pub command: String,
}

/// 配置分层（对标 Claude Code，D9）：user / project / local 浅层合并，后者覆盖前者。
pub fn load_settings(
    user_dir: &std::path::Path,
    project_dir: &std::path::Path,
) -> Result<Settings, SettingsError> {
    let mut settings = Settings::default();
    for path in [
        user_dir.join("bingo").join("settings.json"),
        project_dir.join(".bingo").join("settings.json"),
        project_dir.join(".bingo").join("local.json"),
    ] {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let layer: Settings = serde_json::from_str(&raw)?;
        merge(&mut settings, layer);
    }
    Ok(settings)
}

fn merge(base: &mut Settings, layer: Settings) {
    if let Some(mode) = layer.permission_mode {
        base.permission_mode = Some(mode);
    }
    if let Some(theme) = layer.theme {
        base.theme = Some(theme);
    }
    if !layer.hooks.pre_tool_use.is_empty() {
        base.hooks.pre_tool_use = layer.hooks.pre_tool_use;
    }
    if !layer.hooks.post_tool_use.is_empty() {
        base.hooks.post_tool_use = layer.hooks.post_tool_use;
    }
    if !layer.mcp_servers.is_empty() {
        base.mcp_servers = layer.mcp_servers;
    }
    if !layer.hooks.pre_compact.is_empty() {
        base.hooks.pre_compact = layer.hooks.pre_compact;
    }
    if !layer.hooks.post_compact.is_empty() {
        base.hooks.post_compact = layer.hooks.post_compact;
    }
    if !layer.permissions.allow.is_empty() {
        base.permissions.allow.extend(layer.permissions.allow);
    }
    if !layer.permissions.deny.is_empty() {
        base.permissions.deny.extend(layer.permissions.deny);
    }
    if !layer.permissions.ask.is_empty() {
        base.permissions.ask.extend(layer.permissions.ask);
    }
    for (base_hooks, layer_hooks) in [
        (&mut base.hooks.user_prompt_submit, &layer.hooks.user_prompt_submit),
        (&mut base.hooks.stop, &layer.hooks.stop),
        (&mut base.hooks.session_start, &layer.hooks.session_start),
        (&mut base.hooks.session_end, &layer.hooks.session_end),
        (&mut base.hooks.task_created, &layer.hooks.task_created),
        (&mut base.hooks.task_completed, &layer.hooks.task_completed),
    ] {
        if !layer_hooks.is_empty() {
            *base_hooks = layer_hooks.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn loads_and_merges_layers() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write(&tmp, ".bingo/settings.json", r#"{"permissionMode":"acceptEdits"}"#);
        write(&tmp, ".bingo/local.json", r#"{"permissionMode":"plan"}"#);
        write(&tmp, "user/bingo/settings.json", r#"{"hooks":{"PreToolUse":[{"matcher":"","hooks":[{"type":"command","command":"echo hi"}]}]}}"#);

        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.permission_mode.as_deref(), Some("plan"));
        assert_eq!(settings.hooks.pre_tool_use.len(), 1);
        assert_eq!(settings.hooks.pre_tool_use[0].hooks[0].command, "echo hi");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_files_default() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.permission_mode, None);
        assert!(settings.hooks.pre_tool_use.is_empty());
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

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
    /// 发送 cache_control（prompt caching）。默认关闭：非官方端点处理不稳定。
    #[serde(rename = "cacheControl")]
    pub cache_control: Option<bool>,
    pub hooks: HooksConfig,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: HashMap<String, McpServerConfig>,
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

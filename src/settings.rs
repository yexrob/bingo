use std::collections::HashMap;

use serde::{Deserialize, Serialize};
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
    /// API key（`apiKey`）：settings 优先，回落 ANTHROPIC_API_KEY/DEEPSEEK_API_KEY。
    /// 放 user 层（`~/.config/bingo/settings.json`）；项目层会入库，注意别提交。
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
    /// API 端点（`apiBaseUrl`）：settings 优先，回落 ANTHROPIC_BASE_URL。
    #[serde(rename = "apiBaseUrl")]
    pub api_base_url: Option<String>,
    /// 命名 provider（`providers`，Anthropic 协议）：`/provider <名>` 切换。
    /// 顶层 apiKey/apiBaseUrl（或 env）构成默认 provider "default"。
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    /// 思考级别（`thinkingLevel`）：off | low | medium | high。
    /// 缺省不发 thinking 参数（兼容 DeepSeek 等端点）；映射
    /// budget_tokens 2048/8192/16384 发给模型。
    #[serde(rename = "thinkingLevel", default)]
    pub thinking_level: Option<String>,
    #[serde(rename = "permissionMode")]
    pub permission_mode: Option<String>,
    /// TUI 主题：auto（跟随终端背景）/ dark / light。默认 auto。
    pub theme: Option<String>,
    /// 发送 cache_control（prompt caching）。默认关闭：非官方端点处理不稳定。
    #[serde(rename = "cacheControl")]
    pub cache_control: Option<bool>,
    /// `!` 命令（bash 模式）执行后是否把输出交给模型回应（对标 CC
    /// `respondToBashCommands`，默认 true；false = 纯执行不查模型）。
    #[serde(rename = "respondToBashCommands")]
    pub respond_to_bash_commands: Option<bool>,
    pub hooks: HooksConfig,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    /// 禁用的 MCP 服务器名单（对标 Claude Code disabledMcpServers）。
    #[serde(rename = "disabledMcpServers", default)]
    pub disabled_mcp_servers: Vec<String>,
    /// 权限规则表（对标 Claude Code permissions.allow/deny/ask，规则语法 `Tool(content)`）。
    pub permissions: PermissionRules,
}

/// 命名 provider（Anthropic 协议端点）。
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    #[serde(rename = "apiKey")]
    pub api_key: String,
    #[serde(rename = "apiBaseUrl")]
    pub api_base_url: String,
}

/// 权限规则（对标 Claude Code settings permissions 段）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PermissionRules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub ask: Vec<String>,
}

/// MCP server 定义（对标 Claude Code mcpServers）。
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    /// 传输类型：stdio（缺省）| sse | http | ws。仅 stdio 已落地，其余连接时报错。
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
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
    if let Some(key) = layer.api_key {
        base.api_key = Some(key);
    }
    if let Some(url) = layer.api_base_url {
        base.api_base_url = Some(url);
    }
    if !layer.providers.is_empty() {
        base.providers.extend(layer.providers);
    }
    if let Some(level) = layer.thinking_level {
        base.thinking_level = Some(level);
    }
    if let Some(mode) = layer.permission_mode {
        base.permission_mode = Some(mode);
    }
    if let Some(theme) = layer.theme {
        base.theme = Some(theme);
    }
    if let Some(respond) = layer.respond_to_bash_commands {
        base.respond_to_bash_commands = Some(respond);
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
    base.disabled_mcp_servers.extend(layer.disabled_mcp_servers);
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

/// 读改写 `.bingo/settings.json` 的顶层字段（/permissions /theme 持久化）：
/// 保留文件内其他配置，仅覆盖 patch 中的键；无文件则新建。
pub fn upsert_project_settings(
    project_dir: &std::path::Path,
    patch: &serde_json::Value,
) -> Result<(), SettingsError> {
    use std::io::Write;
    let dir = project_dir.join(".bingo");
    let path = dir.join("settings.json");
    let mut root: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if let Some(obj) = root.as_object_mut()
        && let Some(patch_obj) = patch.as_object()
    {
        for (k, v) in patch_obj {
            obj.insert(k.clone(), v.clone());
        }
    }
    std::fs::create_dir_all(&dir)?;
    let mut file = std::fs::File::create(&path)?;
    write!(file, "{}", serde_json::to_string_pretty(&root)?)?;
    Ok(())
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
    fn merges_respond_to_bash_commands() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-bash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write(&tmp, "user/bingo/settings.json", r#"{"respondToBashCommands":true}"#);
        write(&tmp, ".bingo/settings.json", r#"{"respondToBashCommands":false}"#);

        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.respond_to_bash_commands, Some(false));

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

    #[test]
    fn parses_and_merges_api_config() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-{}-api", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write(&tmp, ".bingo/settings.json", r#"{"apiKey":"sk-project","apiBaseUrl":"https://project.example"}"#);
        write(&tmp, "user/bingo/settings.json", r#"{"apiKey":"sk-user","apiBaseUrl":"https://user.example"}"#);

        // user 层优先（layer 顺序 user → project → local，后者覆盖前者）。
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.api_key.as_deref(), Some("sk-project"));
        assert_eq!(settings.api_base_url.as_deref(), Some("https://project.example"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parses_providers_and_thinking_level() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-{}-prov", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write(
            &tmp,
            ".bingo/settings.json",
            r#"{"thinkingLevel":"high","providers":{"deepseek":{"apiKey":"sk-ds","apiBaseUrl":"https://api.deepseek.com"},"local":{"apiKey":"k","apiBaseUrl":"http://127.0.0.1:11434"}}}"#,
        );
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.thinking_level.as_deref(), Some("high"));
        let ds = settings.providers.get("deepseek").unwrap();
        assert_eq!(ds.api_key, "sk-ds");
        assert_eq!(ds.api_base_url, "https://api.deepseek.com");
        assert_eq!(settings.providers.len(), 2);

        // 层间合并：user 层 provider 与 project 层并存。
        write(
            &tmp,
            "user/bingo/settings.json",
            r#"{"thinkingLevel":"low","providers":{"custom":{"apiKey":"sk-c","apiBaseUrl":"https://c.example"}}}"#,
        );
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.thinking_level.as_deref(), Some("high"), "project 覆盖 user");
        assert!(settings.providers.contains_key("deepseek"));
        assert!(settings.providers.contains_key("custom"));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

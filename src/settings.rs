use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::error::ErrorCode;

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("failed to read settings: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse settings: {0}")]
    Parse(#[from] serde_json::Error),
}

impl ErrorCode for SettingsError {
    fn error_code(&self) -> &'static str {
        match self {
            SettingsError::Io(_) | SettingsError::Parse(_) => "CONFIG_INVALID",
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// API key (`apiKey`): settings win, fall back to ANTHROPIC_API_KEY/DEEPSEEK_API_KEY.
    /// Put it in the user layer (`~/.config/bingo/settings.json`); the project layer gets
    /// committed — mind not to check it in.
    #[serde(rename = "apiKey")]
    pub api_key: Option<String>,
    /// API endpoint (`apiBaseUrl`): settings win, fall back to ANTHROPIC_BASE_URL.
    #[serde(rename = "apiBaseUrl")]
    pub api_base_url: Option<String>,
    /// Named providers (`providers`, Anthropic protocol): `/provider <name>` switches.
    /// Top-level apiKey/apiBaseUrl (or env) form the default provider "default".
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    /// Current provider (`provider`, default "default"): persisted here by the
    /// `/provider` and `/model` menu switches; restored at startup (an invalid
    /// name falls back to default with a warning).
    pub provider: Option<String>,
    /// 默认模型（`model`）：`/model` 选择持久化于此。
    /// 优先级 `--model` > settings（user < project < local）> 内置默认。
    pub model: Option<String>,
    /// Whether the default provider sends image attachments to the model (`sendImages`).
    /// Named providers use their own `supportsImages`; None = don't send.
    #[serde(rename = "sendImages", default)]
    pub send_images: Option<bool>,
    /// Thinking level (`thinkingLevel`): off | low | medium | high | xhigh | max.
    /// Default sends no thinking parameter (compatible with DeepSeek etc.); the other
    /// levels send `{"type":"adaptive"}` + `output_config.effort` — the Claude 5 family
    /// removed budget_tokens; effort carries the depth.
    #[serde(rename = "thinkingLevel", default)]
    pub thinking_level: Option<String>,
    #[serde(rename = "permissionMode")]
    pub permission_mode: Option<String>,
    /// TUI theme: auto (follow terminal background) / dark / light. Default auto.
    pub theme: Option<String>,
    /// Send cache_control (prompt caching). Off by default: non-official endpoints
    /// handle it unreliably.
    #[serde(rename = "cacheControl")]
    pub cache_control: Option<bool>,
    /// Whether `!` commands (bash mode) hand their output to the model after running
    /// (`respondToBashCommands`, default true; false = pure execution, no model query).
    #[serde(rename = "respondToBashCommands")]
    pub respond_to_bash_commands: Option<bool>,
    /// Shell program (`shell`) for the Bash tool and hooks. Default per platform:
    /// macOS /bin/zsh, other Unix /bin/bash, Windows powershell.exe (PowerShell-family
    /// shells run with -Command; any other configured shell with -c, e.g. Git Bash).
    pub shell: Option<String>,
    pub hooks: HooksConfig,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    /// Disabled MCP server names.
    #[serde(rename = "disabledMcpServers", default)]
    pub disabled_mcp_servers: Vec<String>,
    /// Permission rule tables (allow/deny/ask, rule syntax `Tool(content)`).
    pub permissions: PermissionRules,
    /// Experimental feature switches (`experimental`).
    #[serde(default)]
    pub experimental: ExperimentalSettings,
    /// Team settings (`team`): D31 project-level crew.
    #[serde(default)]
    pub team: TeamSettings,
    /// Share upload settings (`share`): `bingo share` 上传端配置。
    #[serde(default)]
    pub share: ShareSettings,
}

/// Share upload settings (`share`): `bingo share` 默认上传到官网分享服务。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ShareSettings {
    /// 官网上传基址（`baseUrl`，缺省 `https://bingo.ruobin.dev`）。
    /// 上传服务公开，无需 token。
    #[serde(rename = "baseUrl", default)]
    pub base_url: Option<String>,
}

/// Team settings (D31). Responsibility: "whether to start"; the team file
/// (.bingo/team.json) governs "what to start".
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TeamSettings {
    /// Auto-start the team on project start (`team.autoStart`). Default true (the
    /// requirement literally reads "start reads by default"); double opt-out: this
    /// switch + `--no-team` CLI.
    #[serde(rename = "autoStart", default)]
    pub auto_start: Option<bool>,
}

/// Experimental features (all off by default).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExperimentalSettings {
    /// Agent channels (`agentChannels`): when enabled, the main session gains
    /// Channel/Post tools and direct subagents gain the Post tool.
    #[serde(rename = "agentChannels", default)]
    pub agent_channels: bool,
    /// Per-channel total message cap (`channelMessageLimit`, default 500; beyond it the
    /// channel freezes and the main agent is notified).
    #[serde(rename = "channelMessageLimit")]
    pub channel_message_limit: Option<u64>,
    /// Per-agent per-channel message cap (`agentMessageLimit`, default 50).
    #[serde(rename = "agentMessageLimit")]
    pub agent_message_limit: Option<u64>,
}

/// Named provider. v1 = Anthropic-protocol endpoint; v2 adds the optional
/// `protocol` field (values `anthropic` | `openai`, default `anthropic` —
/// every existing config parses unchanged, D33).
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    #[serde(rename = "apiKey")]
    pub api_key: String,
    /// Endpoint base URL; empty/missing falls back to the protocol default
    /// (anthropic → api.anthropic.com, openai → api.openai.com).
    #[serde(rename = "apiBaseUrl", default)]
    pub api_base_url: String,
    /// Wire protocol: `anthropic` (default) | `openai` (Responses API).
    #[serde(default)]
    pub protocol: Option<String>,
    /// Whether this provider's model accepts image content (`supportsImages`;
    /// None/default = don't send images).
    #[serde(rename = "supportsImages", default)]
    pub supports_images: Option<bool>,
}

/// Permission rules (settings permissions section).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PermissionRules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub ask: Vec<String>,
}

/// MCP server definition.
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    /// Transport type: stdio (default) | http (streamable HTTP).
    /// sse / ws not implemented yet; configuring them errors at connect time.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// Launch command for stdio servers (required for type=stdio).
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Endpoint for http servers (required for type=http).
    pub url: Option<String>,
    /// Custom headers for http requests (Authorization etc.).
    #[serde(default)]
    pub headers: HashMap<String, String>,
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

/// Config layering (D9): user / project / local shallow merge, later layers override.
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
    if let Some(p) = layer.provider {
        base.provider = Some(p);
    }
    if let Some(model) = layer.model {
        base.model = Some(model);
    }
    if let Some(v) = layer.send_images {
        base.send_images = Some(v);
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
    if let Some(cache) = layer.cache_control {
        base.cache_control = Some(cache);
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
    // experimental: merge field by field (any layer enabling a switch turns it on; caps
    // are overridden by later layers).
    if layer.experimental.agent_channels {
        base.experimental.agent_channels = true;
    }
    if let Some(v) = layer.experimental.channel_message_limit {
        base.experimental.channel_message_limit = Some(v);
    }
    if let Some(v) = layer.experimental.agent_message_limit {
        base.experimental.agent_message_limit = Some(v);
    }
    // team: autoStart overridden by later layers (user → project → local).
    if let Some(v) = layer.team.auto_start {
        base.team.auto_start = Some(v);
    }
    // share: baseUrl overridden by later layers.
    if let Some(v) = layer.share.base_url {
        base.share.base_url = Some(v);
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

/// Read-modify-write the top-level fields of `.bingo/settings.json`
/// (/permissions /theme persistence): keep other config in the file, only override the
/// keys in the patch; create the file if missing.
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

    /// cacheControl must merge layer by layer — skipping it would leave prompt caching
    /// permanently off.
    #[test]
    fn merges_cache_control() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write(&tmp, "user/bingo/settings.json", r#"{"cacheControl":true}"#);

        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.cache_control, Some(true), "user 层 cacheControl 生效");

        // Project layer overrides user layer.
        write(&tmp, ".bingo/settings.json", r#"{"cacheControl":false}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.cache_control, Some(false));

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

        // Project layer overrides user (layer order user → project → local, later wins).
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.api_key.as_deref(), Some("sk-project"));
        assert_eq!(settings.api_base_url.as_deref(), Some("https://project.example"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parses_experimental_settings() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-{}-exp", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // All off by default.
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert!(!settings.experimental.agent_channels);
        assert!(settings.experimental.channel_message_limit.is_none());
        write(
            &tmp,
            ".bingo/settings.json",
            r#"{"experimental":{"agentChannels":true,"channelMessageLimit":100,"agentMessageLimit":10}}"#,
        );
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert!(settings.experimental.agent_channels);
        let limits = crate::channels::ChannelLimits::from_settings(&settings);
        assert_eq!(limits.channel_total, 100);
        assert_eq!(limits.per_agent, 10);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// model overrides layer by layer: later layer (local > project > user) wins,
    /// default None.
    #[test]
    fn merges_model() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-{}-model", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.model, None, "缺省不配置模型");

        write(&tmp, "user/bingo/settings.json", r#"{"model":"claude-sonnet-5"}"#);
        write(&tmp, ".bingo/settings.json", r#"{"model":"claude-opus-5"}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.model.as_deref(), Some("claude-opus-5"), "project 覆盖 user");

        write(&tmp, ".bingo/local.json", r#"{"model":"deepseek-v4"}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.model.as_deref(), Some("deepseek-v4"), "local 覆盖 project");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// provider 逐层合并：后层胜出，缺省 None（运行时回落 "default"）。
    #[test]
    fn merges_provider() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-{}-provsel", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.provider, None, "缺省不配置 provider");

        write(&tmp, "user/bingo/settings.json", r#"{"provider":"deepseek"}"#);
        write(&tmp, ".bingo/settings.json", r#"{"provider":"local"}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.provider.as_deref(), Some("local"), "project 覆盖 user");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// D33 settings v2: `protocol` optional (defaults to anthropic behavior),
    /// `apiBaseUrl` optional (empty = protocol default), both additive — an
    /// openai provider parses without breaking anthropic-only configs.
    #[test]
    fn parses_provider_protocol_v2() {
        let json = r#"{
            "providers": {
                "codex": { "protocol": "openai", "apiKey": "sk-oa", "supportsImages": true },
                "road": { "apiKey": "sk-road", "apiBaseUrl": "https://sub2apis.ruobin.dev/" }
            }
        }"#;
        let settings: Settings = serde_json::from_str(json).unwrap();
        let codex = settings.providers.get("codex").unwrap();
        assert_eq!(codex.protocol.as_deref(), Some("openai"));
        assert_eq!(codex.api_base_url, "", "apiBaseUrl 可缺省");
        assert_eq!(codex.supports_images, Some(true));
        // v1 config without protocol keeps parsing (anthropic default).
        let road = settings.providers.get("road").unwrap();
        assert_eq!(road.protocol, None);
        assert_eq!(road.api_base_url, "https://sub2apis.ruobin.dev/");
    }

    /// Unknown protocol values are a config error at provider build time
    /// (surfaced at startup, CONFIG_INVALID).
    #[test]
    fn unknown_protocol_is_config_error() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "bogus".to_string(),
            ProviderConfig {
                api_key: "k".into(),
                api_base_url: String::new(),
                protocol: Some("chatgpt".into()),
                supports_images: None,
            },
        );
        let client = crate::api::client::Client::from_settings_with(&settings, |_| {
            Err(std::env::VarError::NotPresent)
        });
        let err = client.err().unwrap();
        assert_eq!(
            crate::error::map_error(&err),
            "CONFIG_INVALID",
            "未知 protocol 应落配置错误"
        );
        assert!(err.to_string().contains("chatgpt"), "错误文案应点名非法值");
    }

    #[test]
    fn merges_team_auto_start_across_layers() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-{}-team", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // Default: None (runtime falls back to true, see D31).
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.team.auto_start, None);
        // User layer sets true.
        write(
            &tmp,
            "user/bingo/settings.json",
            r#"{"team":{"autoStart":true}}"#,
        );
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.team.auto_start, Some(true));
        // Project layer unset → keep user-layer value; local layer false overrides.
        write(&tmp, ".bingo/settings.json", r#"{"permissionMode":"plan"}"#);
        write(&tmp, ".bingo/local.json", r#"{"team":{"autoStart":false}}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.team.auto_start, Some(false), "local 覆盖 user");
        // Unknown fields (older versions without a team section) must be ignored, not
        // error: clear local's override and re-check.
        write(&tmp, ".bingo/local.json", r#"{"permissionMode":"plan"}"#);
        write(&tmp, ".bingo/settings.json", r#"{"team":{"autoStart":true,"futureField":1}}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.team.auto_start, Some(true), "未知字段忽略");
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

        // Cross-layer merge: user-layer providers coexist with project-layer ones.
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

    /// supportsImages/sendImages: default None (don't send), merged layer by layer.
    #[test]
    fn parses_image_support_flags() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-{}-img", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write(
            &tmp,
            ".bingo/settings.json",
            r#"{"sendImages":true,"providers":{"road":{"apiKey":"k","apiBaseUrl":"https://road.example","supportsImages":true},"ds":{"apiKey":"k","apiBaseUrl":"https://ds.example"}}}"#,
        );
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.send_images, Some(true));
        assert_eq!(settings.providers["road"].supports_images, Some(true));
        assert_eq!(settings.providers["ds"].supports_images, None, "缺省不发图片");

        // Cross-layer override: project-layer sendImages overrides user layer (later wins).
        write(&tmp, "user/bingo/settings.json", r#"{"sendImages":false}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.send_images, Some(true), "project 覆盖 user");
        // Only the user layer present: its value takes effect.
        write(&tmp, ".bingo/settings.json", r#"{"model":"m"}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.send_images, Some(false));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

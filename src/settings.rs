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
    /// 默认模型（`model`）：`/model` 选择持久化于此。
    /// 优先级 `--model` > settings（user < project < local）> 内置默认。
    pub model: Option<String>,
    /// 默认（default）provider 是否把图片附件发给模型（`sendImages`）。
    /// 命名 provider 用各自的 `supportsImages`；None = 不发送。
    #[serde(rename = "sendImages", default)]
    pub send_images: Option<bool>,
    /// 思考级别（`thinkingLevel`）：off | low | medium | high | xhigh | max。
    /// 缺省不发 thinking 参数（兼容 DeepSeek 等端点）；其余档位发
    /// `{"type":"adaptive"}` + `output_config.effort`——Claude 5 家族已移除
    /// budget_tokens，深度由 effort 承担。
    #[serde(rename = "thinkingLevel", default)]
    pub thinking_level: Option<String>,
    #[serde(rename = "permissionMode")]
    pub permission_mode: Option<String>,
    /// TUI 主题：auto（跟随终端背景）/ dark / light。默认 auto。
    pub theme: Option<String>,
    /// 发送 cache_control（prompt caching）。默认关闭：非官方端点处理不稳定。
    #[serde(rename = "cacheControl")]
    pub cache_control: Option<bool>,
    /// `!` 命令（bash 模式）执行后是否把输出交给模型回应
    /// （`respondToBashCommands`，默认 true；false = 纯执行不查模型）。
    #[serde(rename = "respondToBashCommands")]
    pub respond_to_bash_commands: Option<bool>,
    pub hooks: HooksConfig,
    #[serde(rename = "mcpServers")]
    pub mcp_servers: HashMap<String, McpServerConfig>,
    /// 禁用的 MCP 服务器名单。
    #[serde(rename = "disabledMcpServers", default)]
    pub disabled_mcp_servers: Vec<String>,
    /// 权限规则表（allow/deny/ask，规则语法 `Tool(content)`）。
    pub permissions: PermissionRules,
    /// 实验特性开关（`experimental`）。
    #[serde(default)]
    pub experimental: ExperimentalSettings,
    /// team 设置（`team`）：D31 项目级编队。
    #[serde(default)]
    pub team: TeamSettings,
}

/// team 设置（D31）。职责：管「要不要拉起」；team 文件（.bingo/team.json）管「拉起什么」。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TeamSettings {
    /// 项目启动时自动拉起 team（`team.autoStart`）。缺省 true（需求字面
    /// 「启动默认读取」）；双 opt-out：本开关 + `--no-team` CLI。
    #[serde(rename = "autoStart", default)]
    pub auto_start: Option<bool>,
}

/// 实验特性（默认全关）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExperimentalSettings {
    /// agent 频道互发（`agentChannels`）：开启后主会话获得 Channel/Post
    /// 工具，直接子代理获得 Post 工具。
    #[serde(rename = "agentChannels", default)]
    pub agent_channels: bool,
    /// 每频道消息总上限（`channelMessageLimit`，默认 500；超限冻结频道并通知主 agent）。
    #[serde(rename = "channelMessageLimit")]
    pub channel_message_limit: Option<u64>,
    /// 每 agent 每频道发言上限（`agentMessageLimit`，默认 50）。
    #[serde(rename = "agentMessageLimit")]
    pub agent_message_limit: Option<u64>,
}

/// 命名 provider（Anthropic 协议端点）。
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    #[serde(rename = "apiKey")]
    pub api_key: String,
    #[serde(rename = "apiBaseUrl")]
    pub api_base_url: String,
    /// 该 provider 的模型是否接受图片内容（`supportsImages`；
    /// None/缺省 = 不发送图片）。
    #[serde(rename = "supportsImages", default)]
    pub supports_images: Option<bool>,
}

/// 权限规则（settings permissions 段）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct PermissionRules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub ask: Vec<String>,
}

/// MCP server 定义。
#[derive(Debug, Clone, Deserialize)]
pub struct McpServerConfig {
    /// 传输类型：stdio（缺省）| http（streamable HTTP）。
    /// sse / ws 暂未落地，配置后连接时报错。
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// stdio 服务器的启动命令（type=stdio 必需）。
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// http 服务器端点（type=http 必需）。
    pub url: Option<String>,
    /// http 请求自定义头（Authorization 等鉴权头）。
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

/// 配置分层（D9）：user / project / local 浅层合并，后者覆盖前者。
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
    // experimental：逐字段合并（开关任一层开启即开；上限后层覆盖前层）。
    if layer.experimental.agent_channels {
        base.experimental.agent_channels = true;
    }
    if let Some(v) = layer.experimental.channel_message_limit {
        base.experimental.channel_message_limit = Some(v);
    }
    if let Some(v) = layer.experimental.agent_message_limit {
        base.experimental.agent_message_limit = Some(v);
    }
    // team：autoStart 后层覆盖前层（user → project → local）。
    if let Some(v) = layer.team.auto_start {
        base.team.auto_start = Some(v);
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

    /// cacheControl 必须逐层合并——漏掉它 prompt caching 永远关闭。
    #[test]
    fn merges_cache_control() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write(&tmp, "user/bingo/settings.json", r#"{"cacheControl":true}"#);

        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.cache_control, Some(true), "user 层 cacheControl 生效");

        // project 层覆盖 user 层。
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

        // project 层覆盖 user（layer 顺序 user → project → local，后者覆盖前者）。
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.api_key.as_deref(), Some("sk-project"));
        assert_eq!(settings.api_base_url.as_deref(), Some("https://project.example"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn parses_experimental_settings() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-{}-exp", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // 缺省全关。
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

    /// model 逐层覆盖：后层（local > project > user）胜出，缺省 None。
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

    #[test]
    fn merges_team_auto_start_across_layers() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-{}-team", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // 缺省：None（运行时回落 true，见 D31）。
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.team.auto_start, None);
        // user 层设 true。
        write(
            &tmp,
            "user/bingo/settings.json",
            r#"{"team":{"autoStart":true}}"#,
        );
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.team.auto_start, Some(true));
        // project 层未设 → 保持 user 层值；local 层 false 覆盖。
        write(&tmp, ".bingo/settings.json", r#"{"permissionMode":"plan"}"#);
        write(&tmp, ".bingo/local.json", r#"{"team":{"autoStart":false}}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.team.auto_start, Some(false), "local 覆盖 user");
        // 未知字段（旧版本无 team 段）应忽略不报错：清掉 local 的覆盖再看。
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

    /// supportsImages/sendImages：缺省 None（不发送），逐层合并。
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

        // 层间覆盖：project 层 sendImages 覆盖 user 层（后层胜出）。
        write(&tmp, "user/bingo/settings.json", r#"{"sendImages":false}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.send_images, Some(true), "project 覆盖 user");
        // 只有 user 层时其值生效。
        write(&tmp, ".bingo/settings.json", r#"{"model":"m"}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.send_images, Some(false));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

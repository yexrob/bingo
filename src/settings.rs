use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::error::ErrorCode;

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("failed to read settings: {0}")]
    Io(#[from] std::io::Error),
    /// Read failure on a concrete layer file (anything but NotFound): the path
    /// names which of the three layers is broken.
    #[error("failed to read settings {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    /// Parse failure names the offending layer file — with three layers, a
    /// bare line/column would leave the user guessing which file to open.
    #[error("failed to parse settings {path}: {source}")]
    Parse {
        path: std::path::PathBuf,
        source: serde_json::Error,
    },
    #[error("failed to encode settings: {0}")]
    Encode(serde_json::Error),
}

impl ErrorCode for SettingsError {
    fn error_code(&self) -> &'static str {
        match self {
            SettingsError::Io(_)
            | SettingsError::Read { .. }
            | SettingsError::Parse { .. }
            | SettingsError::Encode(_) => "CONFIG_INVALID",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
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
    /// TUI motion: auto (default) / off。`off` 或 env `BINGO_NO_MOTION=1`：
    /// 动效（欢迎卡更新提示呼吸等）静止为基色，提示本身不消失。
    /// 这是 bingo 第一个动效开关，同时服务「用户关停」与「测试确定性」。
    pub motion: Option<String>,
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
    /// Public-share endpoint settings (`share`), consulted only for `--public` uploads.
    #[serde(default)]
    pub share: ShareSettings,
}

/// Share publishing settings (`share`): used only when `bingo share --public` uploads.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct ShareSettings {
    /// 官网上传基址（`baseUrl`，缺省 `https://bingo.ruobin.dev`）。
    /// 上传服务公开，无需 token。
    #[serde(rename = "baseUrl", default)]
    pub base_url: Option<String>,
}

/// Team settings (D31). Responsibility: "whether to start"; the team file
/// (.bingo/team.json) governs "what to start".
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct TeamSettings {
    /// Auto-start the team on project start (`team.autoStart`). Default true (the
    /// requirement literally reads "start reads by default"); double opt-out: this
    /// switch + `--no-team` CLI.
    #[serde(rename = "autoStart", default)]
    pub auto_start: Option<bool>,
}

/// Experimental features (all off by default).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ProviderConfig {
    /// Static API key (v2: optional — OAuth providers omit it, D33 §5:
    /// apiKey wins over OAuth; both missing is a config error at startup).
    #[serde(rename = "apiKey", default)]
    pub api_key: Option<String>,
    /// Endpoint base URL; empty/missing falls back to the protocol default
    /// (anthropic → api.anthropic.com, openai → api.openai.com).
    #[serde(rename = "apiBaseUrl", default)]
    pub api_base_url: String,
    /// Wire protocol: `anthropic` (default) | `openai` (Responses API).
    #[serde(default)]
    pub protocol: Option<String>,
    /// OAuth login config (D33 §6): present means the provider authenticates
    /// via OAuth when no `apiKey` is set (apiKey wins over OAuth). v1 kind:
    /// `codex` (ChatGPT subscription).
    #[serde(default)]
    pub oauth: Option<OauthConfig>,
    /// Whether this provider's model accepts image content (`supportsImages`;
    /// None/default = don't send images).
    #[serde(rename = "supportsImages", default)]
    pub supports_images: Option<bool>,
}

/// OAuth provider config (`providers.<name>.oauth`, D33).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct OauthConfig {
    /// Built-in flow kind: `codex` (ChatGPT subscription device flow/PKCE).
    pub kind: String,
    /// Optional stored-account selector (multi-account; v1 stores one —
    /// P2 stores a single account, so the field is parsed but unused yet).
    #[serde(default)]
    #[allow(dead_code)]
    pub account: Option<String>,
}

/// Permission rules (settings permissions section).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct PermissionRules {
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub ask: Vec<String>,
}

/// MCP server definition.
#[derive(Debug, Clone, PartialEq, Deserialize)]
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

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct HookRule {
    #[serde(default)]
    pub matcher: String,
    pub hooks: Vec<Hook>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
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
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            // Absent layers are normal; any other IO failure (permissions, bad
            // mount) must not silently degrade to "no config" — the user's API
            // key and rules would vanish without a trace.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(SettingsError::Read { path, source: e }),
        };
        let layer: Settings = serde_json::from_str(&raw).map_err(|e| SettingsError::Parse {
            path: path.clone(),
            source: e,
        })?;
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
    if let Some(motion) = layer.motion {
        base.motion = Some(motion);
    }
    if let Some(cache) = layer.cache_control {
        base.cache_control = Some(cache);
    }
    if let Some(respond) = layer.respond_to_bash_commands {
        base.respond_to_bash_commands = Some(respond);
    }
    if let Some(shell) = layer.shell {
        base.shell = Some(shell);
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
        (
            &mut base.hooks.user_prompt_submit,
            &layer.hooks.user_prompt_submit,
        ),
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

/// Every recognized top-level settings key (`/config` flags anything else as
/// a probable typo — serde ignores unknown fields silently by design, which
/// doubles as forward compatibility but eats misspellings).
pub const KNOWN_KEYS: &[&str] = &[
    "apiKey",
    "apiBaseUrl",
    "providers",
    "provider",
    "model",
    "sendImages",
    "thinkingLevel",
    "permissionMode",
    "theme",
    "motion",
    "cacheControl",
    "respondToBashCommands",
    "shell",
    "hooks",
    "mcpServers",
    "disabledMcpServers",
    "permissions",
    "experimental",
    "team",
    "share",
];

/// The three layer files, lowest to highest precedence (user < project < local).
pub fn layer_paths(
    user_dir: &std::path::Path,
    project_dir: &std::path::Path,
) -> [std::path::PathBuf; 3] {
    [
        user_dir.join("bingo").join("settings.json"),
        project_dir.join(".bingo").join("settings.json"),
        project_dir.join(".bingo").join("local.json"),
    ]
}

/// Top-level keys present in a layer file (missing/broken file = none).
pub fn layer_keys(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|v| v.as_object().map(|o| o.keys().cloned().collect::<Vec<_>>()))
        .unwrap_or_default()
}

/// Persist a UI selection where it takes effect: each key goes to the highest
/// layer that already defines it (writing below a defining layer is silently
/// ineffective — the user-layer `theme` was dead in any project whose
/// `.bingo/settings.json` mentioned theme), and to the USER layer when no
/// layer defines it. `/theme` in a random directory no longer conjures a
/// `.bingo/` out of thin air.
pub fn upsert_scoped_settings(
    user_dir: &std::path::Path,
    project_dir: &std::path::Path,
    patch: &serde_json::Value,
) -> Result<(), SettingsError> {
    let paths = layer_paths(user_dir, project_dir);
    let Some(patch_obj) = patch.as_object() else {
        return Ok(());
    };
    // Group keys by target layer, then write each touched file once.
    let mut per_layer: [serde_json::Map<String, serde_json::Value>; 3] = Default::default();
    let keys_per_layer: Vec<Vec<String>> = paths.iter().map(|p| layer_keys(p)).collect();
    for (key, value) in patch_obj {
        let target = (0..3)
            .rev()
            .find(|&i| keys_per_layer[i].contains(key))
            .unwrap_or(0);
        per_layer[target].insert(key.clone(), value.clone());
    }
    for (i, layer_patch) in per_layer.iter().enumerate() {
        if layer_patch.is_empty() {
            continue;
        }
        upsert_file(&paths[i], &serde_json::Value::Object(layer_patch.clone()))?;
    }
    Ok(())
}

/// Remove a value from an array-valued key in EVERY layer that lists it —
/// union-merged keys (`disabledMcpServers`) cannot be "un-set" by writing one
/// layer: `/mcp enable` used to write the project layer while the user layer
/// merged the name right back on the next start.
pub fn remove_from_union_lists(
    user_dir: &std::path::Path,
    project_dir: &std::path::Path,
    key: &str,
    value: &str,
) -> Result<(), SettingsError> {
    for path in layer_paths(user_dir, project_dir) {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(mut root) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let Some(list) = root.get_mut(key).and_then(|v| v.as_array_mut()) else {
            continue;
        };
        let before = list.len();
        list.retain(|v| v.as_str() != Some(value));
        if list.len() != before {
            let patch = serde_json::json!({ key: root[key].clone() });
            upsert_file(&path, &patch)?;
        }
    }
    Ok(())
}

/// Read-modify-write the top-level fields of one settings file: keep other
/// config, only override the keys in the patch; create the file if missing.
fn upsert_file(path: &std::path::Path, patch: &serde_json::Value) -> Result<(), SettingsError> {
    use std::io::Write;
    let dir = path
        .parent()
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| SettingsError::Io(std::io::Error::other("settings path has no parent")))?;
    let mut root: serde_json::Value = std::fs::read_to_string(path)
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
    // tmp + fsync + rename (same discipline as AuthStore): a kill or full disk
    // mid-write must never leave a truncated settings.json — a broken file
    // refuses the next startup.
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        writeln!(
            file,
            "{}",
            serde_json::to_string_pretty(&root).map_err(SettingsError::Encode)?
        )?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Project-layer write for explicitly project-scoped state (/permissions
/// rules, MCP disable lists): these are union-merged and project-intentioned,
/// so `.bingo/` creation is the point, not a side effect.
pub fn upsert_project_settings(
    project_dir: &std::path::Path,
    patch: &serde_json::Value,
) -> Result<(), SettingsError> {
    upsert_file(&project_dir.join(".bingo").join("settings.json"), patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, rel: &str, content: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Scoped writes land where they take effect: undefined keys go to the
    /// user layer (no conjured `.bingo/`); keys already defined in a higher
    /// layer are updated there (a user-layer write below would be dead).
    #[test]
    fn scoped_upsert_targets_the_effective_layer() {
        let tmp = std::env::temp_dir().join(format!("bingo-scoped-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let user_dir = tmp.join("config");
        let project = tmp.join("proj");
        std::fs::create_dir_all(&project).unwrap();

        // Nothing defines theme → user layer, project untouched.
        upsert_scoped_settings(&user_dir, &project, &serde_json::json!({ "theme": "dark" }))
            .unwrap();
        let user_file = user_dir.join("bingo/settings.json");
        assert!(
            std::fs::read_to_string(&user_file)
                .unwrap()
                .contains("dark"),
            "user 层承接未定义键"
        );
        assert!(!project.join(".bingo").exists(), "不凭空创建项目层");

        // Project defines model → the pair splits: model updates in project,
        // provider (undefined) goes to user.
        write(&project, ".bingo/settings.json", r#"{"model": "old"}"#);
        upsert_scoped_settings(
            &user_dir,
            &project,
            &serde_json::json!({ "model": "new", "provider": "p" }),
        )
        .unwrap();
        let proj_raw = std::fs::read_to_string(project.join(".bingo/settings.json")).unwrap();
        assert!(proj_raw.contains("\"model\": \"new\""), "{proj_raw}");
        assert!(
            !proj_raw.contains("provider"),
            "未定义键不进项目层: {proj_raw}"
        );
        let user_raw = std::fs::read_to_string(&user_file).unwrap();
        assert!(user_raw.contains("\"provider\": \"p\""), "{user_raw}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Union-list removal touches EVERY layer that lists the value —
    /// `/mcp enable` used to write the project layer while the user layer
    /// merged the name straight back on the next start.
    #[test]
    fn union_removal_covers_all_layers() {
        let tmp = std::env::temp_dir().join(format!("bingo-union-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let user_dir = tmp.join("config");
        let project = tmp.join("proj");
        write(
            &user_dir,
            "bingo/settings.json",
            r#"{"disabledMcpServers": ["files", "keep"]}"#,
        );
        write(
            &project,
            ".bingo/settings.json",
            r#"{"disabledMcpServers": ["files"]}"#,
        );
        remove_from_union_lists(&user_dir, &project, "disabledMcpServers", "files").unwrap();
        let merged = load_settings(&user_dir, &project).unwrap();
        assert_eq!(
            merged.disabled_mcp_servers,
            vec!["keep".to_string()],
            "跨层清除后合并结果不再复活"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Merge-completeness guard: a layer that sets EVERY field, merged onto the
    /// default base, must equal the layer itself. A field forgotten in `merge`
    /// keeps its default and fails the comparison — this is exactly how `shell`
    /// and `motion` silently stopped working. When adding a Settings field,
    /// extend this fixture in the same change.
    #[test]
    fn merge_covers_every_field() {
        let full = r#"{
            "apiKey": "sk-full",
            "apiBaseUrl": "https://full.example",
            "providers": {"p": {"apiKey": "sk-p", "apiBaseUrl": "https://p.example",
                                "protocol": "openai", "oauth": {"kind": "codex"},
                                "supportsImages": true}},
            "provider": "p",
            "model": "model-full",
            "sendImages": true,
            "thinkingLevel": "high",
            "permissionMode": "plan",
            "theme": "dark",
            "motion": "off",
            "cacheControl": true,
            "respondToBashCommands": false,
            "shell": "/bin/fish",
            "hooks": {
                "PreToolUse": [{"matcher": "", "hooks": [{"type": "command", "command": "a"}]}],
                "PostToolUse": [{"matcher": "", "hooks": [{"type": "command", "command": "b"}]}],
                "PreCompact": [{"matcher": "", "hooks": [{"type": "command", "command": "c"}]}],
                "PostCompact": [{"matcher": "", "hooks": [{"type": "command", "command": "d"}]}],
                "UserPromptSubmit": [{"matcher": "", "hooks": [{"type": "command", "command": "e"}]}],
                "Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "f"}]}],
                "SessionStart": [{"matcher": "", "hooks": [{"type": "command", "command": "g"}]}],
                "SessionEnd": [{"matcher": "", "hooks": [{"type": "command", "command": "h"}]}],
                "TaskCreated": [{"matcher": "", "hooks": [{"type": "command", "command": "i"}]}],
                "TaskCompleted": [{"matcher": "", "hooks": [{"type": "command", "command": "j"}]}]
            },
            "mcpServers": {"m": {"command": "mcp", "args": ["--x"], "env": {"K": "V"}}},
            "disabledMcpServers": ["m"],
            "permissions": {"allow": ["Bash(a:*)"], "deny": ["Bash(b:*)"], "ask": ["Bash(c:*)"]},
            "experimental": {"agentChannels": true, "channelMessageLimit": 7, "agentMessageLimit": 3},
            "team": {"autoStart": false},
            "share": {"baseUrl": "https://share.example"}
        }"#;
        let layer: Settings = serde_json::from_str(full).unwrap();
        let mut merged = Settings::default();
        merge(&mut merged, layer.clone());
        assert_eq!(
            merged, layer,
            "merge 漏拷字段（新加字段需同步扩展本 fixture）"
        );

        // KNOWN_KEYS 与结构体同步：fixture 的每个键都必须被识别，
        // 反向 KNOWN_KEYS 里没有 fixture 外的臆造键。
        let value: serde_json::Value = serde_json::from_str(full).unwrap();
        let fixture_keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for key in &fixture_keys {
            assert!(KNOWN_KEYS.contains(key), "KNOWN_KEYS 缺 {key}");
        }
        for key in KNOWN_KEYS {
            assert!(fixture_keys.contains(key), "KNOWN_KEYS 有多余键 {key}");
        }
    }

    #[test]
    fn loads_and_merges_layers() {
        let tmp = std::env::temp_dir().join(format!("bingo-settings-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        write(
            &tmp,
            ".bingo/settings.json",
            r#"{"permissionMode":"acceptEdits"}"#,
        );
        write(&tmp, ".bingo/local.json", r#"{"permissionMode":"plan"}"#);
        write(
            &tmp,
            "user/bingo/settings.json",
            r#"{"hooks":{"PreToolUse":[{"matcher":"","hooks":[{"type":"command","command":"echo hi"}]}]}}"#,
        );

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
        write(
            &tmp,
            "user/bingo/settings.json",
            r#"{"respondToBashCommands":true}"#,
        );
        write(
            &tmp,
            ".bingo/settings.json",
            r#"{"respondToBashCommands":false}"#,
        );

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
        assert_eq!(
            settings.cache_control,
            Some(true),
            "user 层 cacheControl 生效"
        );

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
        write(
            &tmp,
            ".bingo/settings.json",
            r#"{"apiKey":"sk-project","apiBaseUrl":"https://project.example"}"#,
        );
        write(
            &tmp,
            "user/bingo/settings.json",
            r#"{"apiKey":"sk-user","apiBaseUrl":"https://user.example"}"#,
        );

        // Project layer overrides user (layer order user → project → local, later wins).
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.api_key.as_deref(), Some("sk-project"));
        assert_eq!(
            settings.api_base_url.as_deref(),
            Some("https://project.example")
        );

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

        write(
            &tmp,
            "user/bingo/settings.json",
            r#"{"model":"claude-sonnet-5"}"#,
        );
        write(&tmp, ".bingo/settings.json", r#"{"model":"claude-opus-5"}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(
            settings.model.as_deref(),
            Some("claude-opus-5"),
            "project 覆盖 user"
        );

        write(&tmp, ".bingo/local.json", r#"{"model":"deepseek-v4"}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(
            settings.model.as_deref(),
            Some("deepseek-v4"),
            "local 覆盖 project"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// provider 逐层合并：后层胜出，缺省 None（运行时回落 "default"）。
    #[test]
    fn merges_provider() {
        let tmp =
            std::env::temp_dir().join(format!("bingo-settings-{}-provsel", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(settings.provider, None, "缺省不配置 provider");

        write(
            &tmp,
            "user/bingo/settings.json",
            r#"{"provider":"deepseek"}"#,
        );
        write(&tmp, ".bingo/settings.json", r#"{"provider":"local"}"#);
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(
            settings.provider.as_deref(),
            Some("local"),
            "project 覆盖 user"
        );

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
        assert_eq!(codex.api_key.as_deref(), Some("sk-oa"));
        // v1 config without protocol keeps parsing (anthropic default).
        let road = settings.providers.get("road").unwrap();
        assert_eq!(road.protocol, None);
        assert_eq!(road.api_base_url, "https://sub2apis.ruobin.dev/");
    }

    /// ① 无 apiKey + oauth 配置解析成功（main 实测 bug 回归，D33 §5）：
    /// `apiKey` 可缺省，OAuth provider 不带静态 key。
    #[test]
    fn parses_oauth_provider_without_api_key() {
        let json = r#"{
            "providers": {
                "codex": { "protocol": "openai", "oauth": { "kind": "codex" } }
            }
        }"#;
        let settings: Settings = serde_json::from_str(json).unwrap();
        let codex = settings.providers.get("codex").unwrap();
        assert_eq!(codex.api_key, None, "无 apiKey 解析成功");
        assert_eq!(codex.oauth.as_ref().map(|o| o.kind.as_str()), Some("codex"));
        assert_eq!(codex.protocol.as_deref(), Some("openai"));
    }

    /// ④ 存量配置零迁移：v1 必填 apiKey 的 provider 原样解析。
    #[test]
    fn parses_v1_provider_unchanged() {
        let json = r#"{
            "providers": {
                "deepseek": { "apiKey": "sk-ds", "apiBaseUrl": "https://api.deepseek.com" }
            }
        }"#;
        let settings: Settings = serde_json::from_str(json).unwrap();
        let ds = settings.providers.get("deepseek").unwrap();
        assert_eq!(ds.api_key.as_deref(), Some("sk-ds"));
        assert_eq!(ds.api_base_url, "https://api.deepseek.com");
        assert_eq!(ds.protocol, None, "缺省 anthropic");
        assert_eq!(ds.oauth, None);
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
                api_key: Some("k".into()),
                api_base_url: String::new(),
                protocol: Some("chatgpt".into()),
                supports_images: None,
                oauth: None,
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
        write(
            &tmp,
            ".bingo/settings.json",
            r#"{"team":{"autoStart":true,"futureField":1}}"#,
        );
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
        assert_eq!(ds.api_key.as_deref(), Some("sk-ds"));
        assert_eq!(ds.api_base_url, "https://api.deepseek.com");
        assert_eq!(settings.providers.len(), 2);

        // Cross-layer merge: user-layer providers coexist with project-layer ones.
        write(
            &tmp,
            "user/bingo/settings.json",
            r#"{"thinkingLevel":"low","providers":{"custom":{"apiKey":"sk-c","apiBaseUrl":"https://c.example"}}}"#,
        );
        let settings = load_settings(&tmp.join("user"), &tmp).unwrap();
        assert_eq!(
            settings.thinking_level.as_deref(),
            Some("high"),
            "project 覆盖 user"
        );
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
        assert_eq!(
            settings.providers["ds"].supports_images, None,
            "缺省不发图片"
        );

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

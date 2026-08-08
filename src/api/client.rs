//! `Client` is the provider facade (D33): it owns the provider table,
//! the current-provider switch and the display info used by `/provider`
//! and the `/model` menu. Protocol behavior lives in the adapters
//! (`api::providers`); consumers never see wire JSON.

use std::collections::HashMap;
use std::sync::Arc;

use crate::api::contract::{BoxStream, NeutralRequest, ProviderClient, SystemBlock};
use crate::api::providers;
use crate::api::types::Message;

pub use crate::api::contract::{AssistantAccumulator, ClientError};

/// cfg(test) test hook re-export (tui/chat.rs timeout tests, error.rs drift
/// test use these).
#[cfg(test)]
pub(crate) use crate::api::contract::transport_offline_code;
#[cfg(test)]
pub(crate) use crate::api::providers::anthropic::test_hooks;

/// Per-protocol default endpoint base URL (used when a named provider leaves
/// `apiBaseUrl` empty, D33).
fn protocol_default_base_url(protocol: Option<&str>) -> String {
    match protocol.unwrap_or("anthropic") {
        "openai" => providers::openai::API_BASE.to_string(),
        _ => providers::anthropic::API_BASE.to_string(),
    }
}

/// User home (auth.json lives under ~/.local/share/bingo; bingo requires
/// HOME at startup, so a missing var degrades to an empty path).
fn home_dir() -> std::path::PathBuf {
    std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

/// Display info for a provider (the `/provider` listing and `/model` menu):
/// auth material (masked by the caller) + endpoint URL.
#[derive(Clone)]
struct EndpointInfo {
    /// Static key (None = OAuth/stored-key provider — no key to mask).
    api_key: Option<String>,
    base_url: String,
    /// Wire protocol label ("anthropic" / "openai") — the /provider listing.
    protocol: String,
    /// Shared OAuth token provider: `/provider login|logout` mutate the same
    /// instance the adapter reads, so credentials take effect live.
    oauth: Option<Arc<crate::api::auth::TokenProvider>>,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    /// Current provider: adapter (source of truth for capabilities) + the
    /// display info paired with it (kept in lockstep on switches).
    endpoint: Arc<std::sync::RwLock<(Arc<dyn ProviderClient>, EndpointInfo)>>,
    /// Named-provider table (default is not in the table for listing, but is
    /// stored under the reserved key "default").
    providers: HashMap<String, (Arc<dyn ProviderClient>, EndpointInfo)>,
    /// Names coming from the built-in preset registry (D34): the /provider
    /// listing shows them with a 内置 badge.
    preset_names: Vec<String>,
}

impl Client {
    /// Settings first, falling back to environment variables
    /// (ANTHROPIC_API_KEY/DEEPSEEK_API_KEY, ANTHROPIC_BASE_URL). Reports
    /// MissingApiKey when neither settings nor env has a key.
    pub fn from_settings(settings: &crate::settings::Settings) -> Result<Self, ClientError> {
        Self::from_settings_with(settings, |name| std::env::var(name))
    }

    /// Injectable variant of from_settings (tests use a fake env, avoiding
    /// real environment variables). The auth store resolves against the
    /// user's HOME.
    pub(crate) fn from_settings_with(
        settings: &crate::settings::Settings,
        env: impl Fn(&str) -> std::result::Result<String, std::env::VarError>,
    ) -> Result<Self, ClientError> {
        Self::build(settings, env, &home_dir())
    }

    /// Build against an explicit home (tests isolate the auth.json read;
    /// production main passes the same HOME).
    #[cfg(test)]
    pub(crate) fn from_settings_at(
        settings: &crate::settings::Settings,
        env: impl Fn(&str) -> std::result::Result<String, std::env::VarError>,
        home: &std::path::Path,
    ) -> Result<Self, ClientError> {
        Self::build(settings, env, home)
    }

    fn build(
        settings: &crate::settings::Settings,
        env: impl Fn(&str) -> std::result::Result<String, std::env::VarError>,
        home: &std::path::Path,
    ) -> Result<Self, ClientError> {
        let http = reqwest::Client::new();
        // A missing top-level key no longer aborts construction: the default
        // provider degrades to an unconfigured fail-fast adapter so the TUI
        // (and `/provider login`) stays reachable — the old hard failure
        // locked subscription-only users out before the login command.
        let api_key = settings
            .api_key
            .clone()
            .or_else(|| env("ANTHROPIC_API_KEY").ok())
            .or_else(|| env("DEEPSEEK_API_KEY").ok());
        let base_url = settings.api_base_url.clone().unwrap_or_else(|| {
            env("ANTHROPIC_BASE_URL").unwrap_or_else(|_| providers::anthropic::API_BASE.to_string())
        });
        let mut providers: HashMap<String, (Arc<dyn ProviderClient>, EndpointInfo)> =
            HashMap::new();
        let mut preset_names = Vec::new();
        // ① Built-in official subscriptions (D34 §6.5): visible and loginable
        // with zero config; a user `providers.<name>` entry overrides the
        // preset field-by-field (absent fields fall back to the preset).
        for preset in providers::presets::PRESETS {
            let user = settings.providers.get(preset.name);
            let protocol = user
                .and_then(|c| c.protocol.clone())
                .unwrap_or_else(|| preset.protocol.to_string());
            // apiKey presets (opencode-go) resolve their key from auth.json
            // live per request (`stored_key` below) — a /provider login in
            // the running session takes effect without a restart.
            let api_key = user.and_then(|c| c.api_key.clone());
            let base_url = match user {
                Some(c) if !c.api_base_url.is_empty() => c.api_base_url.clone(),
                _ => preset.base_url.to_string(),
            };
            let supports_images = user
                .and_then(|c| c.supports_images)
                .unwrap_or(preset.supports_images);
            let oauth = user.and_then(|c| c.oauth.clone()).or_else(|| {
                preset.oauth_kind.map(|kind| crate::settings::OauthConfig {
                    kind: kind.to_string(),
                    account: None,
                })
            });
            let allowlist = preset.model_allowlist.map(|models| {
                providers::openai::ModelAllowlist(models.iter().map(|m| m.to_string()).collect())
            });
            let built = providers::build_provider(
                preset.name,
                http.clone(),
                Some(&protocol),
                api_key.clone(),
                base_url.clone(),
                supports_images,
                oauth.as_ref(),
                home,
                allowlist,
                true,
            )
            .map_err(|message| {
                ClientError::Config(format!("provider \"{}\": {message}", preset.name))
            })?;
            providers.insert(
                preset.name.to_string(),
                (
                    built.adapter,
                    EndpointInfo {
                        api_key,
                        base_url,
                        protocol,
                        oauth: built.token_provider,
                    },
                ),
            );
            preset_names.push(preset.name.to_string());
        }
        // ② User-only providers (preset names are already merged above).
        for (name, cfg) in &settings.providers {
            if providers::presets::preset(name).is_some() {
                continue;
            }
            let protocol = cfg.protocol.as_deref();
            let base_url = if cfg.api_base_url.is_empty() {
                protocol_default_base_url(protocol)
            } else {
                cfg.api_base_url.clone()
            };
            let built = providers::build_provider(
                name,
                http.clone(),
                protocol,
                cfg.api_key.clone(),
                base_url.clone(),
                // Both protocols define image content blocks: the capability is the
                // baseline, and `supportsImages: false` is the opt-out for an endpoint that
                // speaks the protocol but rejects them (some compat proxies).
                cfg.supports_images.unwrap_or(true),
                cfg.oauth.as_ref(),
                home,
                None,
                false,
            )
            .map_err(|message| {
                // Config error (e.g. unknown protocol) — surfaced at
                // startup with the same code family as settings parse
                // failures, before any request goes out.
                ClientError::Config(format!("provider \"{name}\": {message}"))
            })?;
            providers.insert(
                name.clone(),
                (
                    built.adapter,
                    EndpointInfo {
                        api_key: cfg.api_key.clone(),
                        base_url,
                        protocol: protocol.unwrap_or("anthropic").to_string(),
                        oauth: built.token_provider,
                    },
                ),
            );
        }
        // default 端点也入 providers 表（key "default"）：set_provider /
        // with_provider("default") 走通（含「切回 default」），/model 二级
        // 对 default 拉列表用顶层端点、标签与内容一致（P0-C）。default 为
        // 保留名：顶层配置优先（后插入覆盖用户同名的 providers 定义）。
        let default_adapter = match &api_key {
            Some(key) => providers::anthropic(
                http.clone(),
                key.clone(),
                base_url.clone(),
                settings.send_images.unwrap_or(true),
            ),
            None => providers::unconfigured(),
        };
        let default_info = EndpointInfo {
            api_key,
            base_url,
            protocol: "anthropic".to_string(),
            oauth: None,
        };
        providers.insert(
            "default".to_string(),
            (default_adapter.clone(), default_info.clone()),
        );
        Ok(Self {
            http,
            endpoint: Arc::new(std::sync::RwLock::new((default_adapter, default_info))),
            providers,
            preset_names,
        })
    }

    #[cfg(test)]
    pub fn new(api_key: String, base_url: String) -> Self {
        let http = reqwest::Client::new();
        let adapter = providers::anthropic(http.clone(), api_key.clone(), base_url.clone(), false);
        let info = EndpointInfo {
            api_key: Some(api_key),
            base_url,
            protocol: "anthropic".to_string(),
            oauth: None,
        };
        Self {
            http,
            endpoint: Arc::new(std::sync::RwLock::new((adapter.clone(), info.clone()))),
            providers: HashMap::from([("default".to_string(), (adapter, info))]),
            preset_names: Vec::new(),
        }
    }

    /// Named-provider list (default excluded; the /provider listing — callers prepend "default"
    /// explicitly, so both the /model menu and /provider output lead with "default").
    pub fn provider_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .providers
            .keys()
            .filter(|n| n.as_str() != "default")
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// Endpoint of a named provider (key/url; "default" = the top-level config).
    /// Unknown names return None.
    pub fn provider_endpoint(&self, name: &str) -> Option<(Option<String>, String)> {
        self.providers
            .get(name)
            .map(|(_, info)| (info.api_key.clone(), info.base_url.clone()))
    }

    /// Named providers whose endpoint accepts image blocks. A text-only session can still put an
    /// attachment in front of a model by forking a subagent onto one of these, so the list is
    /// what makes that route discoverable instead of guessable. "default" is excluded: as a
    /// sub-agent argument it means "share the parent endpoint", which is the one that can't.
    pub fn image_capable_providers(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .providers
            .iter()
            .filter(|(name, (adapter, _))| {
                name.as_str() != "default" && adapter.capabilities().supports_images
            })
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names
    }

    /// Wire protocol label of a named provider ("anthropic"/"openai";
    /// the /provider listing). Unknown names return None.
    pub fn provider_protocol(&self, name: &str) -> Option<String> {
        self.providers
            .get(name)
            .map(|(_, info)| info.protocol.clone())
    }

    /// 当前生效的 provider 端点（key/url 引用）。
    pub fn current_endpoint(&self) -> (Option<String>, String) {
        let current = self.endpoint.read().unwrap_or_else(|p| p.into_inner());
        (current.1.api_key.clone(), current.1.base_url.clone())
    }

    /// Whether the current endpoint accepts image content blocks
    /// (`supportsImages`/`sendImages` config).
    pub fn supports_images(&self) -> bool {
        self.endpoint
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .0
            .capabilities()
            .supports_images
    }

    /// Switch to a named provider; unknown names error out ("default" = the
    /// top-level endpoint, always switchable back to).
    pub fn set_provider(&self, name: &str) -> Result<(), String> {
        let Some((adapter, info)) = self.providers.get(name).cloned() else {
            return Err(format!("未找到 provider \"{name}\"（/provider 查看列表）"));
        };
        *self.endpoint.write().unwrap_or_else(|p| p.into_inner()) = (adapter, info);
        Ok(())
    }

    /// Derive an endpoint-independent Client (for sub-agents that pin a
    /// provider): the new Client locks that provider's endpoint, and the
    /// providers table is shared (same name table). Without a provider you
    /// should just clone (shared endpoint, follows the parent session's
    /// switches).
    pub fn with_provider(&self, name: &str) -> Result<Client, String> {
        let Some((adapter, info)) = self.providers.get(name).cloned() else {
            return Err(format!("未找到 provider \"{name}\"（/provider 查看列表）"));
        };
        Ok(Client {
            http: self.http.clone(),
            endpoint: Arc::new(std::sync::RwLock::new((adapter, info))),
            providers: self.providers.clone(),
            preset_names: self.preset_names.clone(),
        })
    }

    /// Start a streaming request on the current provider.
    pub async fn stream(&self, request: &NeutralRequest) -> Result<BoxStream, ClientError> {
        self.current().stream(request).await
    }

    /// Non-streaming completion on the current provider (compact summaries,
    /// memory extraction).
    pub async fn complete_text(&self, request: &NeutralRequest) -> Result<String, ClientError> {
        self.current().complete_text(request).await
    }

    /// List the models the current endpoint supports (the `/model` menu).
    pub async fn list_models(&self) -> Result<Vec<String>, ClientError> {
        self.current().list_models().await
    }

    /// Input token count on the current provider (D12: the budget display
    /// goes through the official count_tokens API).
    pub async fn count_tokens(
        &self,
        model: &str,
        system: &[SystemBlock],
        messages: &[Message],
    ) -> Result<u64, ClientError> {
        self.current().count_tokens(model, system, messages).await
    }

    /// Whether the provider comes from the built-in preset registry (D34) —
    /// the /provider listing shows a 内置 badge.
    pub fn is_preset(&self, name: &str) -> bool {
        self.preset_names.iter().any(|p| p == name)
    }

    /// Auth state of a named provider (the /provider listing; "default" is
    /// the top-level endpoint). Unknown names return None.
    pub fn auth_status(&self, name: &str) -> Option<crate::api::contract::AuthStatus> {
        self.providers.get(name).map(|(p, _)| p.auth_status())
    }

    /// The shared OAuth token provider of a named provider — the SAME
    /// instance the adapter authenticates with. `/provider login|logout`
    /// must go through this so credentials take effect in the running
    /// session instead of after a restart.
    pub fn token_provider(&self, name: &str) -> Option<Arc<crate::api::auth::TokenProvider>> {
        self.providers
            .get(name)
            .and_then(|(_, info)| info.oauth.clone())
    }

    /// Whether a provider has any usable credential right now (read live).
    pub fn is_configured(&self, name: &str) -> bool {
        match self.auth_status(name) {
            Some(crate::api::contract::AuthStatus::ApiKey) => true,
            Some(crate::api::contract::AuthStatus::StoredKey { configured }) => configured,
            Some(crate::api::contract::AuthStatus::OAuth { account }) => account.is_some(),
            Some(crate::api::contract::AuthStatus::Unconfigured) | None => false,
        }
    }

    /// The model a provider starts with when none was remembered: preset
    /// allowlists lead with their flagship; anthropic-protocol endpoints get
    /// the built-in default. None = no safe guess (the caller warns).
    pub fn provider_default_model(&self, name: &str) -> Option<String> {
        if let Some(preset) = providers::presets::preset(name)
            && let Some(first) = preset.model_allowlist.and_then(|list| list.first())
        {
            return Some(first.to_string());
        }
        match self.provider_protocol(name).as_deref() {
            Some("anthropic") => Some(crate::api::types::DEFAULT_MODEL.to_string()),
            _ => None,
        }
    }

    fn current(&self) -> Arc<dyn ProviderClient> {
        self.endpoint
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .0
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_settings_prefers_settings_over_env() {
        let settings = crate::settings::Settings {
            api_key: Some("sk-settings".into()),
            api_base_url: Some("https://settings.example".into()),
            ..Default::default()
        };
        let env = |name: &str| -> Result<String, std::env::VarError> {
            match name {
                "ANTHROPIC_API_KEY" => Ok("sk-env".into()),
                "ANTHROPIC_BASE_URL" => Ok("https://env.example".into()),
                _ => Err(std::env::VarError::NotPresent),
            }
        };
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-settings"));
        assert_eq!(client.current_endpoint().1, "https://settings.example");
    }

    #[test]
    fn from_settings_falls_back_to_env() {
        let settings = crate::settings::Settings::default();
        let env = |name: &str| -> Result<String, std::env::VarError> {
            match name {
                "DEEPSEEK_API_KEY" => Ok("sk-deepseek".into()),
                "ANTHROPIC_BASE_URL" => Ok("https://deepseek.example".into()),
                _ => Err(std::env::VarError::NotPresent),
            }
        };
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-deepseek"));
        assert_eq!(client.current_endpoint().1, "https://deepseek.example");
    }

    /// A missing top-level key no longer aborts construction: the default
    /// provider degrades to an unconfigured fail-fast adapter, so the TUI
    /// (where `/provider login` lives) stays reachable.
    #[test]
    fn missing_key_degrades_default_to_unconfigured() {
        let settings = crate::settings::Settings::default();
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert!(matches!(
            client.auth_status("default"),
            Some(crate::api::contract::AuthStatus::Unconfigured)
        ));
        assert!(!client.is_configured("default"));
        // Presets stay reachable for login.
        assert!(client.provider_names().contains(&"codex".to_string()));
    }

    #[test]
    fn from_settings_defaults_base_url() {
        let settings = crate::settings::Settings {
            api_key: Some("sk".into()),
            ..Default::default()
        };
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().1, providers::anthropic::API_BASE);
    }

    #[test]
    fn provider_switch_changes_endpoint() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                api_key: Some("sk-ds".into()),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        settings.providers.insert(
            "local".to_string(),
            crate::settings::ProviderConfig {
                api_key: Some("sk-local".into()),
                api_base_url: "http://127.0.0.1:11434".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-main"));
        assert_eq!(
            client.provider_names(),
            vec!["codex", "deepseek", "local", "opencode-go"],
            "presets 与用户 provider 合并"
        );

        client.set_provider("deepseek").unwrap();
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-ds"));
        assert_eq!(client.current_endpoint().1, "https://api.deepseek.com");

        assert!(client.set_provider("nope").is_err(), "未知 provider 报错");
        // An unknown provider does not affect the current endpoint.
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-ds"));
    }

    /// P0-C: "default" endpoint is switchable and resolvable — provider_names excludes it,
    /// set_provider/with_provider("default") reach the top-level endpoint, provider_endpoint
    /// returns its URL (for the /provider listing).
    #[test]
    fn default_provider_is_switchable_and_listed_as_endpoint() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            api_base_url: Some("https://main.example".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                api_key: Some("sk-ds".into()),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        // default 不出现在命名列表（调用方显式补出）。
        assert_eq!(
            client.provider_names(),
            vec!["codex", "deepseek", "opencode-go"],
            "presets 可见"
        );
        assert_eq!(
            client.provider_endpoint("default"),
            Some((
                Some("sk-main".to_string()),
                "https://main.example".to_string()
            ))
        );
        assert_eq!(
            client.provider_endpoint("deepseek").unwrap().1,
            "https://api.deepseek.com"
        );
        assert_eq!(client.provider_endpoint("nope"), None);

        // 切到 deepseek 再切回 default：顶层端点恢复（含 supports_images）。
        client.set_provider("deepseek").unwrap();
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-ds"));
        client.set_provider("default").unwrap();
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-main"));
        assert_eq!(client.current_endpoint().1, "https://main.example");

        // with_provider("default") fork 出顶层端点（/model 二级对 default
        // 拉列表用，标签与内容一致）。
        let fork = client.with_provider("default").unwrap();
        assert_eq!(fork.current_endpoint().0.as_deref(), Some("sk-main"));
    }

    /// ② apiKey 优先 + ③ 双缺失报 CONFIG_INVALID（main 实测 bug 回归，
    /// D33 §5：apiKey wins over OAuth；both missing → config error）。
    #[test]
    fn oauth_config_resolution() {
        // ② 同时配置 apiKey + oauth → ApiKey 生效。
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "codex".to_string(),
            crate::settings::ProviderConfig {
                api_key: Some("sk-static".into()),
                api_base_url: String::new(),
                supports_images: None,
                protocol: Some("openai".into()),
                oauth: Some(crate::settings::OauthConfig {
                    kind: "codex".into(),
                    account: None,
                }),
            },
        );
        let client =
            Client::from_settings_with(&settings, |_| Err(std::env::VarError::NotPresent)).unwrap();
        assert!(
            matches!(
                client.auth_status("codex"),
                Some(crate::api::contract::AuthStatus::ApiKey)
            ),
            "apiKey 优先于 oauth"
        );

        // ③ 无 apiKey 无 oauth → CONFIG_INVALID（启动即报）。
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "bare".to_string(),
            crate::settings::ProviderConfig {
                api_key: None,
                api_base_url: String::new(),
                supports_images: None,
                protocol: Some("openai".into()),
                oauth: None,
            },
        );
        let err = Client::from_settings_with(&settings, |_| Err(std::env::VarError::NotPresent))
            .err()
            .unwrap();
        assert_eq!(crate::error::map_error(&err), "CONFIG_INVALID");
        assert!(err.to_string().contains("缺少 apiKey 或 oauth"), "{err}");
    }

    /// P5（D34）：空 settings 下内置 presets 可见、端点/认证正确；apiKey 型
    /// preset（opencode-go）未配置 key 时 auth 为空串（401 引导）。
    #[test]
    fn preset_visibility_zero_config() {
        let tmp = std::env::temp_dir().join(format!("bingo-preset-vis-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        let client =
            Client::from_settings_at(&settings, |_| Err(std::env::VarError::NotPresent), &tmp)
                .unwrap();
        assert_eq!(
            client.provider_names(),
            vec!["codex", "opencode-go"],
            "presets 零配置可见"
        );
        assert!(client.is_preset("codex"));
        assert!(client.is_preset("opencode-go"));
        assert!(!client.is_preset("default"));
        // codex：preset 端点 + OAuth（未登录）。
        assert_eq!(
            client.provider_endpoint("codex").unwrap().1,
            "https://chatgpt.com/backend-api",
            "codex preset 端点"
        );
        assert!(
            matches!(
                client.auth_status("codex"),
                Some(crate::api::contract::AuthStatus::OAuth { account: None })
            ),
            "隔离 home 下 codex preset 未登录"
        );
        // opencode-go：apiKey 型，未配置 → StoredKey（auth.json 实时读，
        // 本会话登录立即翻到已配置）。
        assert_eq!(
            client.provider_endpoint("opencode-go").unwrap().1,
            "https://opencode.ai/zen/go"
        );
        assert!(matches!(
            client.auth_status("opencode-go"),
            Some(crate::api::contract::AuthStatus::StoredKey { configured: false })
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// P5 merge 矩阵：用户仅覆盖 apiBaseUrl → protocol/oauth/白名单回落 preset。
    #[test]
    fn preset_field_merge_keeps_template() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "codex".to_string(),
            crate::settings::ProviderConfig {
                api_key: None,
                api_base_url: "https://custom.example".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        let client =
            Client::from_settings_with(&settings, |_| Err(std::env::VarError::NotPresent)).unwrap();
        assert_eq!(
            client.provider_endpoint("codex").unwrap().1,
            "https://custom.example",
            "apiBaseUrl 用户覆盖"
        );
        assert!(
            matches!(
                client.auth_status("codex"),
                Some(crate::api::contract::AuthStatus::OAuth { .. })
            ),
            "oauth 回落 preset（无需重述）"
        );
        assert_eq!(
            client.provider_protocol("codex").as_deref(),
            Some("openai"),
            "protocol 回落 preset"
        );
    }

    /// P5 零污染：presets 不写回 settings（用户配置文件保持原样）。
    #[test]
    fn presets_do_not_pollute_settings() {
        let settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        let _client =
            Client::from_settings_with(&settings, |_| Err(std::env::VarError::NotPresent)).unwrap();
        assert!(
            settings.providers.is_empty(),
            "presets 是运行时概念，settings 保持用户私有"
        );
    }

    /// supports_images：default 读顶层 sendImages；命名 provider 读各自
    /// supportsImages；切换端点时跟随。
    #[test]
    fn supports_images_follows_endpoint_switch() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            send_images: Some(true),
            ..Default::default()
        };
        settings.providers.insert(
            "vision".to_string(),
            crate::settings::ProviderConfig {
                api_key: Some("sk-v".into()),
                api_base_url: "https://vision.example".into(),
                supports_images: Some(true),
                protocol: None,
                oauth: None,
            },
        );
        settings.providers.insert(
            "text-only".to_string(),
            crate::settings::ProviderConfig {
                api_key: Some("sk-t".into()),
                api_base_url: "https://text.example".into(),
                supports_images: Some(false),
                protocol: None,
                oauth: None,
            },
        );
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert!(client.supports_images(), "default 读顶层 sendImages");

        client.set_provider("text-only").unwrap();
        assert!(!client.supports_images(), "显式 false 覆盖");
        client.set_provider("vision").unwrap();
        assert!(client.supports_images(), "supportsImages=true 生效");
    }
}

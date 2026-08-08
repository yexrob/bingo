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
    std::env::var("HOME").map(std::path::PathBuf::from).unwrap_or_default()
}

/// Display info for a provider (the `/provider` listing and `/model` menu):
/// auth material (masked by the caller) + endpoint URL.
#[derive(Debug, Clone)]
struct EndpointInfo {
    /// Static key (None = OAuth provider — no key to mask).
    api_key: Option<String>,
    base_url: String,
    /// Wire protocol label ("anthropic" / "openai") — the /provider listing.
    protocol: String,
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
}

impl Client {
    /// Settings first, falling back to environment variables
    /// (ANTHROPIC_API_KEY/DEEPSEEK_API_KEY, ANTHROPIC_BASE_URL). Reports
    /// MissingApiKey when neither settings nor env has a key.
    pub fn from_settings(settings: &crate::settings::Settings) -> Result<Self, ClientError> {
        Self::from_settings_with(settings, |name| std::env::var(name))
    }

    /// Injectable variant of from_settings (tests use a fake env, avoiding
    /// real environment variables).
    pub(crate) fn from_settings_with(
        settings: &crate::settings::Settings,
        env: impl Fn(&str) -> std::result::Result<String, std::env::VarError>,
    ) -> Result<Self, ClientError> {
        let http = reqwest::Client::new();
        let api_key = settings
            .api_key
            .clone()
            .or_else(|| env("ANTHROPIC_API_KEY").ok())
            .or_else(|| env("DEEPSEEK_API_KEY").ok())
            .ok_or(ClientError::MissingApiKey)?;
        let base_url = settings.api_base_url.clone().unwrap_or_else(|| {
            env("ANTHROPIC_BASE_URL")
                .unwrap_or_else(|_| providers::anthropic::API_BASE.to_string())
        });
        let mut providers = settings
            .providers
            .iter()
            .map(|(name, cfg)| {
                let protocol = cfg.protocol.as_deref();
                let base_url = if cfg.api_base_url.is_empty() {
                    protocol_default_base_url(protocol)
                } else {
                    cfg.api_base_url.clone()
                };
                let adapter = providers::build_provider(
                    name,
                    http.clone(),
                    protocol,
                    cfg.api_key.clone(),
                    base_url.clone(),
                    cfg.supports_images.unwrap_or(false),
                    cfg.oauth.as_ref(),
                    &home_dir(),
                )
                .map_err(|message| {
                    // Config error (e.g. unknown protocol) — surfaced at
                    // startup with the same code family as settings parse
                    // failures, before any request goes out.
                    ClientError::Config(format!("provider \"{name}\": {message}"))
                })?;
                Ok((
                    name.clone(),
                    (
                        adapter,
                        EndpointInfo {
                            api_key: cfg.api_key.clone(),
                            base_url,
                            protocol: protocol.unwrap_or("anthropic").to_string(),
                        },
                    ),
                ))
            })
            .collect::<Result<HashMap<String, (Arc<dyn ProviderClient>, EndpointInfo)>, ClientError>>()?;
        // default 端点也入 providers 表（key "default"）：set_provider /
        // with_provider("default") 走通（含「切回 default」），/model 二级
        // 对 default 拉列表用顶层端点、标签与内容一致（P0-C）。default 为
        // 保留名：顶层配置优先（后插入覆盖用户同名的 providers 定义）。
        let default_adapter = providers::anthropic(
            http.clone(),
            api_key.clone(),
            base_url.clone(),
            settings.send_images.unwrap_or(false),
        );
        let default_info = EndpointInfo {
            api_key: Some(api_key),
            base_url,
            protocol: "anthropic".to_string(),
        };
        providers.insert("default".to_string(), (default_adapter.clone(), default_info.clone()));
        Ok(Self {
            http,
            endpoint: Arc::new(std::sync::RwLock::new((default_adapter, default_info))),
            providers,
        })
    }

    #[cfg(test)]
    pub fn new(api_key: String, base_url: String) -> Self {
        let http = reqwest::Client::new();
        let adapter =
            providers::anthropic(http.clone(), api_key.clone(), base_url.clone(), false);
        let info = EndpointInfo {
            api_key: Some(api_key),
            base_url,
            protocol: "anthropic".to_string(),
        };
        Self {
            http,
            endpoint: Arc::new(std::sync::RwLock::new((adapter.clone(), info.clone()))),
            providers: HashMap::from([("default".to_string(), (adapter, info))]),
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

    /// Wire protocol label of a named provider ("anthropic"/"openai";
    /// the /provider listing). Unknown names return None.
    pub fn provider_protocol(&self, name: &str) -> Option<String> {
        self.providers.get(name).map(|(_, info)| info.protocol.clone())
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
        })
    }

    /// Start a streaming request on the current provider.
    pub async fn stream(
        &self,
        request: &NeutralRequest,
    ) -> Result<BoxStream, ClientError> {
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

    /// Auth state of a named provider (the /provider listing; "default" is
    /// the top-level endpoint). Unknown names return None.
    pub fn auth_status(&self, name: &str) -> Option<crate::api::contract::AuthStatus> {
        self.providers.get(name).map(|(p, _)| p.auth_status())
    }

    fn current(&self) -> Arc<dyn ProviderClient> {
        self.endpoint.read().unwrap_or_else(|p| p.into_inner()).0.clone()
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

    #[test]
    fn from_settings_missing_key_errors() {
        let settings = crate::settings::Settings::default();
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        assert!(matches!(
            Client::from_settings_with(&settings, env),
            Err(ClientError::MissingApiKey)
        ));
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
        assert_eq!(client.provider_names(), vec!["deepseek", "local"]);

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
        assert_eq!(client.provider_names(), vec!["deepseek"]);
        assert_eq!(
            client.provider_endpoint("default"),
            Some((Some("sk-main".to_string()), "https://main.example".to_string()))
        );
        assert_eq!(client.provider_endpoint("deepseek").unwrap().1, "https://api.deepseek.com");
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
        let client = Client::from_settings_with(&settings, |_| {
            Err(std::env::VarError::NotPresent)
        })
        .unwrap();
        assert!(
            matches!(client.auth_status("codex"), Some(crate::api::contract::AuthStatus::ApiKey)),
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

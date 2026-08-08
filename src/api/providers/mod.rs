//! Provider protocol implementations (D33). Each adapter implements
//! `ProviderClient` against the neutral contract (api::contract); the
//! registry below is the only place that knows how config maps to an
//! adapter.

pub mod anthropic;
pub mod openai;
pub mod presets;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use crate::api::auth::{OauthFlowConfig, TokenProvider};
use crate::api::contract::ProviderClient;
use crate::settings::OauthConfig;

/// How a provider authenticates outbound requests.
#[derive(Debug, Clone)]
pub enum AuthSource {
    /// Static key (settings `apiKey` wins over OAuth — D33 §10).
    ApiKey(String),
    /// Key stored in auth.json (`/provider login <name> --manual <key>`),
    /// resolved per request — a login in the running session takes effect
    /// without a restart.
    StoredKey(StoredKey),
    /// OAuth token provider (D33 §6): lazy/eager refresh, single-flight.
    OAuth(Arc<TokenProvider>),
}

/// Live auth.json key lookup for apiKey-type presets (opencode-go).
#[derive(Debug, Clone)]
pub struct StoredKey {
    home: std::path::PathBuf,
    provider: String,
}

impl StoredKey {
    pub fn new(home: &Path, provider: &str) -> Self {
        Self {
            home: home.to_path_buf(),
            provider: provider.to_string(),
        }
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// The stored key, if the user has logged in (read live on purpose).
    pub fn key(&self) -> Option<String> {
        match crate::auth::AuthStore::new(&self.home).get(&self.provider) {
            Ok(Some(crate::auth::AuthEntry::Api { key })) if !key.is_empty() => Some(key),
            _ => None,
        }
    }
}

/// A built provider: the adapter plus its shared OAuth token provider (when
/// the auth is OAuth) — login/logout must mutate the same instance the
/// adapter reads, or credentials only take effect after a restart.
pub struct BuiltProvider {
    pub adapter: Arc<dyn ProviderClient>,
    pub token_provider: Option<Arc<TokenProvider>>,
}

impl BuiltProvider {
    fn plain(adapter: Arc<dyn ProviderClient>) -> Self {
        Self {
            adapter,
            token_provider: None,
        }
    }
}

/// Build a provider adapter from settings config. `protocol` is the settings
/// `protocol` field (None = "anthropic", backward compatible); an unknown
/// value is a config error at startup. OAuth is only meaningful for the
/// openai protocol (codex endpoint) — anthropic + oauth is a config error.
/// `stored_key` opts an api-key preset into the auth.json live lookup when
/// neither settings key nor oauth is present (instead of a config error).
/// (Single construction point; the parameter list is the config surface.)
#[allow(clippy::too_many_arguments)]
pub fn build_provider(
    name: &str,
    http: reqwest::Client,
    protocol: Option<&str>,
    api_key: Option<String>,
    base_url: String,
    supports_images: bool,
    oauth: Option<&OauthConfig>,
    home: &Path,
    model_allowlist: Option<openai::ModelAllowlist>,
    stored_key: bool,
) -> Result<BuiltProvider, String> {
    match protocol.unwrap_or("anthropic") {
        "anthropic" => {
            let Some(api_key) = api_key else {
                return Err("anthropic provider 缺少 apiKey".into());
            };
            let base_url = if base_url.is_empty() {
                anthropic::API_BASE.to_string()
            } else {
                base_url
            };
            Ok(BuiltProvider::plain(anthropic(
                http,
                api_key,
                base_url,
                supports_images,
            )))
        }
        "openai" => {
            // oauth.kind=codex → the ChatGPT subscription endpoint variant
            // (Path 2, D33 §6.1b): chatgpt.com/backend-api + /codex/responses.
            let codex = matches!(oauth, Some(c) if c.kind == "codex");
            let base_url = if base_url.is_empty() {
                if codex {
                    "https://chatgpt.com/backend-api".to_string()
                } else {
                    openai::API_BASE.to_string()
                }
            } else {
                base_url
            };
            // D33 §5: apiKey wins over OAuth; both missing → stored-key
            // lookup for presets, config error otherwise.
            let (auth, token_provider) = match api_key {
                Some(key) => (AuthSource::ApiKey(key), None),
                None => match oauth {
                    Some(oauth_cfg) => {
                        let tp = build_oauth(name, oauth_cfg, home)?;
                        (AuthSource::OAuth(tp.clone()), Some(tp))
                    }
                    None if stored_key => (AuthSource::StoredKey(StoredKey::new(home, name)), None),
                    None => {
                        return Err(
                            "provider 缺少 apiKey 或 oauth 配置（/provider login 或补 apiKey）"
                                .into(),
                        );
                    }
                },
            };
            let variant = if codex {
                openai::OpenAiVariant::Codex
            } else {
                openai::OpenAiVariant::Default
            };
            Ok(BuiltProvider {
                adapter: openai(
                    http,
                    auth,
                    base_url,
                    supports_images,
                    variant,
                    model_allowlist,
                ),
                token_provider,
            })
        }
        other => Err(format!(
            "未知 protocol \"{other}\"（可用：anthropic / openai）"
        )),
    }
}

/// oauth.kind → a configured TokenProvider (v1: `codex` only); the auth.json
/// entry is keyed by the provider name.
fn build_oauth(name: &str, cfg: &OauthConfig, home: &Path) -> Result<Arc<TokenProvider>, String> {
    match cfg.kind.as_str() {
        "codex" => Ok(Arc::new(TokenProvider::new(
            home,
            name,
            OauthFlowConfig::codex(),
        ))),
        other => Err(format!("未知 oauth.kind \"{other}\"（可用：codex）")),
    }
}

/// Placeholder adapter for the default provider before any credentials exist:
/// every request fails fast with the onboarding hint instead of putting an
/// empty key on the wire. The TUI still starts, so `/provider login` is
/// reachable — the old hard startup failure locked subscription-only users
/// out of the only place they could log in.
struct Unconfigured;

#[async_trait::async_trait]
impl ProviderClient for Unconfigured {
    fn capabilities(&self) -> crate::api::contract::Capabilities {
        crate::api::contract::Capabilities::default()
    }

    fn auth_status(&self) -> crate::api::contract::AuthStatus {
        crate::api::contract::AuthStatus::Unconfigured
    }

    async fn stream(
        &self,
        _request: &crate::api::contract::NeutralRequest,
    ) -> Result<crate::api::contract::BoxStream, crate::api::contract::ClientError> {
        Err(crate::api::contract::ClientError::MissingApiKey)
    }

    async fn complete_text(
        &self,
        _request: &crate::api::contract::NeutralRequest,
    ) -> Result<String, crate::api::contract::ClientError> {
        Err(crate::api::contract::ClientError::MissingApiKey)
    }

    async fn list_models(&self) -> Result<Vec<String>, crate::api::contract::ClientError> {
        Err(crate::api::contract::ClientError::MissingApiKey)
    }

    async fn count_tokens(
        &self,
        _model: &str,
        _system: &[crate::api::contract::SystemBlock],
        _messages: &[crate::api::types::Message],
    ) -> Result<u64, crate::api::contract::ClientError> {
        Err(crate::api::contract::ClientError::MissingApiKey)
    }
}

/// The unconfigured-default adapter (see [`Unconfigured`]).
pub fn unconfigured() -> Arc<dyn ProviderClient> {
    Arc::new(Unconfigured)
}

pub fn anthropic(
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    supports_images: bool,
) -> Arc<dyn ProviderClient> {
    Arc::new(anthropic::AnthropicProvider::new(
        http,
        api_key,
        base_url,
        supports_images,
    ))
}

pub fn openai(
    http: reqwest::Client,
    auth: AuthSource,
    base_url: String,
    supports_images: bool,
    variant: openai::OpenAiVariant,
    model_allowlist: Option<openai::ModelAllowlist>,
) -> Arc<dyn ProviderClient> {
    Arc::new(openai::OpenAIProvider::new(
        http,
        auth,
        base_url,
        supports_images,
        variant,
        model_allowlist,
    ))
}

/// Exponential backoff + jitter: from 500ms, capped at 32s (shared by every
/// adapter's retry loop).
fn backoff(attempt: u32) -> Duration {
    let base_ms = (500u64 << attempt.min(6)).min(32_000);
    let jitter = rand_jitter(base_ms);
    Duration::from_millis(base_ms + jitter)
}

fn rand_jitter(scale: u64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos % (scale / 2 + 1)
}

fn retryable(status: &reqwest::StatusCode) -> bool {
    status.is_server_error() || *status == reqwest::StatusCode::TOO_MANY_REQUESTS
}

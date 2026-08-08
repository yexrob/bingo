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
    /// OAuth token provider (D33 §6): lazy/eager refresh, single-flight.
    OAuth(Arc<TokenProvider>),
}

/// Build a provider adapter from settings config. `protocol` is the settings
/// `protocol` field (None = "anthropic", backward compatible); an unknown
/// value is a config error at startup. OAuth is only meaningful for the
/// openai protocol (codex endpoint) — anthropic + oauth is a config error.
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
) -> Result<Arc<dyn ProviderClient>, String> {
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
            Ok(anthropic(http, api_key, base_url, supports_images))
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
            // D33 §5: apiKey wins over OAuth; both missing → config error.
            // D33 §5: apiKey wins over OAuth; both missing → config error
            // (apiKey presets resolve their key from auth.json in client.rs).
            let auth = match api_key {
                Some(key) => AuthSource::ApiKey(key),
                None => match oauth {
                    Some(oauth_cfg) => build_oauth(name, oauth_cfg, home)?,
                    None => {
                        return Err(
                            "provider 缺少 apiKey 或 oauth 配置（/provider login 或补 apiKey）"
                                .into(),
                        )
                    }
                },
            };
            let variant = if codex {
                openai::OpenAiVariant::Codex
            } else {
                openai::OpenAiVariant::Default
            };
            Ok(openai(http, auth, base_url, supports_images, variant, model_allowlist))
        }
        other => Err(format!(
            "未知 protocol \"{other}\"（可用：anthropic / openai）"
        )),
    }
}

/// oauth.kind → a configured TokenProvider (v1: `codex` only); the auth.json
/// entry is keyed by the provider name.
fn build_oauth(name: &str, cfg: &OauthConfig, home: &Path) -> Result<AuthSource, String> {
    match cfg.kind.as_str() {
        "codex" => {
            let provider = TokenProvider::new(home, name, OauthFlowConfig::codex());
            Ok(AuthSource::OAuth(Arc::new(provider)))
        }
        other => Err(format!("未知 oauth.kind \"{other}\"（可用：codex）")),
    }
}

pub fn anthropic(
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    supports_images: bool,
) -> Arc<dyn ProviderClient> {
    Arc::new(anthropic::AnthropicProvider::new(http, api_key, base_url, supports_images))
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

//! Provider protocol implementations (D33). Each adapter implements
//! `ProviderClient` against the neutral contract (api::contract); the
//! registry below is the only place that knows how config maps to an
//! adapter.

pub mod anthropic;
pub mod openai;

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
    api_key: String,
    base_url: String,
    supports_images: bool,
    oauth: Option<&OauthConfig>,
    home: &Path,
) -> Result<Arc<dyn ProviderClient>, String> {
    match protocol.unwrap_or("anthropic") {
        "anthropic" => {
            if api_key.is_empty() {
                return Err("anthropic provider 缺少 apiKey".into());
            }
            let base_url = if base_url.is_empty() {
                anthropic::API_BASE.to_string()
            } else {
                base_url
            };
            Ok(anthropic(http, api_key, base_url, supports_images))
        }
        "openai" => {
            let base_url = if base_url.is_empty() {
                openai::API_BASE.to_string()
            } else {
                base_url
            };
            let auth = if !api_key.is_empty() {
                AuthSource::ApiKey(api_key)
            } else if let Some(oauth_cfg) = oauth {
                build_oauth(name, oauth_cfg, home)?
            } else {
                return Err("provider 缺少 apiKey 或 oauth 配置（/provider login 或补 apiKey）".into());
            };
            Ok(openai(http, auth, base_url, supports_images))
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
) -> Arc<dyn ProviderClient> {
    Arc::new(openai::OpenAIProvider::new(http, auth, base_url, supports_images))
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

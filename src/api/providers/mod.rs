//! Provider protocol implementations (D33). Each adapter implements
//! `ProviderClient` against the neutral contract (api::contract); the
//! registry below is the only place that knows how config maps to an
//! adapter.

pub mod anthropic;
pub mod openai;

use std::sync::Arc;
use std::time::Duration;

use crate::api::contract::ProviderClient;

/// Build a provider adapter from settings config. `protocol` is the settings
/// `protocol` field (None = "anthropic", backward compatible); an unknown
/// value is a config error at startup.
pub fn build_provider(
    http: reqwest::Client,
    protocol: Option<&str>,
    api_key: String,
    base_url: String,
    supports_images: bool,
) -> Result<Arc<dyn ProviderClient>, String> {
    match protocol.unwrap_or("anthropic") {
        "anthropic" => {
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
            Ok(openai(http, api_key, base_url, supports_images))
        }
        other => Err(format!(
            "未知 protocol \"{other}\"（可用：anthropic / openai）"
        )),
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
    api_key: String,
    base_url: String,
    supports_images: bool,
) -> Arc<dyn ProviderClient> {
    Arc::new(openai::OpenAIProvider::new(http, api_key, base_url, supports_images))
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

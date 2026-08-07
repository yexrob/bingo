//! Provider protocol implementations (D33). Each adapter implements
//! `ProviderClient` against the neutral contract (api::contract); the
//! registry below is the only place that knows how config maps to an
//! adapter. P0 ships the anthropic protocol only; the `protocol` field
//! (settings v2) and the openai adapter land in the next commit.

pub mod anthropic;

use std::sync::Arc;

use crate::api::contract::ProviderClient;

/// Build an anthropic-protocol adapter (P0: the only protocol).
pub fn anthropic(
    http: reqwest::Client,
    api_key: String,
    base_url: String,
    supports_images: bool,
) -> Arc<dyn ProviderClient> {
    Arc::new(anthropic::AnthropicProvider::new(http, api_key, base_url, supports_images))
}

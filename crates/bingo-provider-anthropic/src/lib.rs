//! The Anthropic Messages API as a `Provider` plugin.
//!
//! One HTTP client, one endpoint, no retries: the provider *classifies* a
//! failure and hands it back, and the turn loop owns the retry ladder and the
//! overflow compaction (`crates/bingo-core/src/turn.rs`). Everything below
//! `lib.rs` is pure — request encoding, SSE framing, the event state machine,
//! error classification, the capability table — so the wire format is pinned
//! by fixtures and snapshots rather than by a live endpoint.

pub mod error;
pub mod events;
pub mod models;
pub mod request;
pub mod sse;
pub mod stream;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    AuthStatus, CancellationToken, ConfigClaim, Merge, ModelCapabilities, ModelInfo, ModelRequest,
    ModelStream, Plugin, PluginError, PluginManifest, Provider, ProviderError, Registrar,
};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::stream::IDLE_TIMEOUT;

/// The endpoint every Claude account shares.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// The Messages API version this adapter speaks (old
/// `providers/anthropic.rs:432-436`).
const API_VERSION: &str = "2023-06-01";

const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
const BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";

/// The `anthropic` settings key.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct AnthropicConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

/// The slice the host hands `register`: the claimed keys and nothing else.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Settings {
    pub anthropic: AnthropicConfig,
}

/// One endpoint, one key. Cheap to clone through the `Arc` the registry holds.
#[derive(Debug)]
pub struct AnthropicProvider {
    http: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    /// Where a person puts a key, named in the auth status.
    settings_file: Option<PathBuf>,
}

impl AnthropicProvider {
    /// The settings slice, resolved against the environment.
    pub fn new(config: AnthropicConfig, settings_file: PathBuf) -> Self {
        let mut provider = Self::with_endpoint(
            resolve(API_KEY_ENV, config.api_key),
            resolve(BASE_URL_ENV, config.base_url).unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        );
        provider.settings_file = Some(settings_file);
        provider
    }

    fn missing_key_hint(&self) -> String {
        match &self.settings_file {
            Some(file) => format!(
                "Set {API_KEY_ENV}, or add \"anthropic\": {{\"apiKey\": \"...\"}} to {}.",
                file.display()
            ),
            None => format!("Set {API_KEY_ENV}, or configure anthropic.apiKey in settings."),
        }
    }

    /// An endpoint as given, with no environment lookup — what a test or an
    /// embedder uses when the credentials are already resolved.
    pub fn with_endpoint(api_key: Option<String>, base_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            settings_file: None,
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Missing here rather than at the first request: `auth()` reads the same
    /// field, so the CLI can fail with `AUTH_REQUIRED` before any turn starts.
    fn headers(&self) -> Result<HeaderMap, ProviderError> {
        let key = self.api_key.as_deref().ok_or_else(|| ProviderError::Auth {
            message: format!(
                "no Anthropic API key: set {API_KEY_ENV} or the `anthropic.apiKey` setting"
            ),
        })?;
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(key).map_err(|e| ProviderError::Auth {
                message: format!("the api key is not a valid header value: {e}"),
            })?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static(API_VERSION));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }

    fn post(&self, path: &str, body: &Value) -> Result<reqwest::RequestBuilder, ProviderError> {
        Ok(self
            .http
            .post(format!("{}{path}", self.base_url))
            .headers(self.headers()?)
            .json(body))
    }

    fn get(&self, path: &str) -> Result<reqwest::RequestBuilder, ProviderError> {
        Ok(self
            .http
            .get(format!("{}{path}", self.base_url))
            .headers(self.headers()?))
    }

    /// One round trip. A non-success status never leaves this function: every
    /// caller above it sees a classified `ProviderError` instead. The wait for
    /// the response carries the same idle guard as the body that follows it,
    /// because one silence is worth exactly as much as the other.
    async fn send(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        let response = tokio::time::timeout(IDLE_TIMEOUT, builder.send())
            .await
            .map_err(|_| ProviderError::Timeout)?
            .map_err(|e| ProviderError::Transport {
                message: e.to_string(),
            })?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status().as_u16();
        let retry_after = header(&response, "retry-after");
        let body = response.text().await.unwrap_or_default();
        Err(error::classify(status, &body, retry_after.as_deref()))
    }

    async fn json(&self, builder: reqwest::RequestBuilder) -> Result<Value, ProviderError> {
        self.send(builder)
            .await?
            .json()
            .await
            .map_err(|e| ProviderError::Stream {
                message: format!("unreadable response body: {e}"),
            })
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn capabilities(&self, model: &str) -> ModelCapabilities {
        models::capabilities(model)
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        let body = request::encode(&request, &self.capabilities(&request.model));
        let response = self.send(self.post("/v1/messages", &body)?).await?;
        Ok(stream::model_stream(stream::chunks(response), cancel))
    }

    async fn count_tokens(&self, request: &ModelRequest) -> Result<u64, ProviderError> {
        let body = request::count_tokens(request, &self.capabilities(&request.model));
        let counted = self
            .json(self.post("/v1/messages/count_tokens", &body)?)
            .await?;
        Ok(counted
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0))
    }

    async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(models::parse(&self.json(self.get("/v1/models")?).await?))
    }

    fn auth(&self) -> AuthStatus {
        match self.api_key {
            Some(_) => AuthStatus::Ready,
            None => AuthStatus::Missing {
                hint: self.missing_key_hint(),
            },
        }
    }
}

/// The environment wins over settings, so one shell can point a run at a
/// different key or endpoint without editing a file. A blank value counts as
/// unset in either place, so an exported empty variable does not shadow a
/// configured one.
fn resolve(variable: &str, from_settings: Option<String>) -> Option<String> {
    [std::env::var(variable).ok(), from_settings]
        .into_iter()
        .flatten()
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

fn header(response: &reqwest::Response, name: &str) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn settings_schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.provider.anthropic",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["provider:anthropic"],
    requires: &[],
    config: Some(ConfigClaim {
        // One endpoint at a time: a project that names its own key and base
        // url replaces the user's pair whole rather than half-overriding it.
        keys: &[("anthropic", Merge::Replace)],
        schema: settings_schema,
    }),
};

/// Registers one `AnthropicProvider`, built from the `anthropic` settings key.
#[derive(Debug, Default, Clone, Copy)]
pub struct AnthropicPlugin;

#[async_trait]
impl Plugin for AnthropicPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        let settings_file = registrar.env().config_dir.join("settings.json");
        let provider = AnthropicProvider::new(settings.anthropic, settings_file);
        registrar.provider(Arc::new(provider) as Arc<dyn Provider>);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use bingo_sdk::Contribution;
    use serde_json::json;
    use std::path::PathBuf;

    /// A recorded wire body under `fixtures/`. Tests read it from the manifest
    /// directory, because a test binary's working directory is not the crate's.
    pub(crate) fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name)
    }

    /// A provider the ambient environment cannot reach into, so the assertions
    /// hold on a machine that already exports `ANTHROPIC_API_KEY`.
    fn hermetic(key: Option<&str>) -> AnthropicProvider {
        AnthropicProvider::with_endpoint(key.map(str::to_string), DEFAULT_BASE_URL)
    }

    #[test]
    fn the_plugin_registers_the_provider_it_claims() {
        let mut registrar = Registrar::new(
            "bingo.provider.anthropic",
            json!({}),
            bingo_sdk::Env::rooted("/tmp"),
        );
        AnthropicPlugin.register(&mut registrar).expect("register");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), 1);
        match &contributions[0] {
            Contribution::Provider(provider) => assert_eq!(provider.id(), "anthropic"),
            other => panic!("expected a provider, got {other:?}"),
        }
        assert_eq!(MANIFEST.provides, &["provider:anthropic"]);
        assert_eq!(MANIFEST.id, "bingo.provider.anthropic");
    }

    #[test]
    fn the_claimed_key_merges_by_replacement_and_has_a_schema() {
        let claim = MANIFEST.config.expect("the plugin claims settings");
        assert_eq!(claim.keys, &[("anthropic", Merge::Replace)]);
        let schema = serde_json::to_value((claim.schema)()).expect("a json schema");
        assert!(
            schema.to_string().contains("apiKey") && schema.to_string().contains("baseUrl"),
            "the schema names the camelCase keys: {schema}"
        );
    }

    #[test]
    fn a_claimed_api_key_reaches_the_provider() {
        let mut registrar = Registrar::new(
            "bingo.provider.anthropic",
            json!({ "anthropic": { "apiKey": "sk-ant-from-settings" } }),
            bingo_sdk::Env::rooted("/tmp"),
        );
        AnthropicPlugin.register(&mut registrar).expect("register");
        match &registrar.into_contributions()[0] {
            Contribution::Provider(provider) => assert_eq!(provider.auth(), AuthStatus::Ready),
            other => panic!("expected a provider, got {other:?}"),
        }
    }

    /// `std::env::set_var` is unsafe in Rust 2024 and this workspace forbids
    /// `unsafe`, so the environment half of the precedence rule is exercised
    /// through the resolver the provider is built from.
    #[test]
    fn no_key_anywhere_leaves_authentication_missing() {
        assert!(matches!(hermetic(None).auth(), AuthStatus::Missing { .. }));
        assert_eq!(hermetic(Some("sk-ant-test")).auth(), AuthStatus::Ready);
        assert_eq!(resolve("BINGO_NO_SUCH_VARIABLE", None), None);
        assert_eq!(resolve("BINGO_NO_SUCH_VARIABLE", Some("  ".into())), None);
        assert_eq!(
            resolve("BINGO_NO_SUCH_VARIABLE", Some(" from-settings ".into())),
            Some("from-settings".into())
        );
    }

    #[tokio::test]
    async fn without_a_key_a_turn_fails_before_it_reaches_the_wire() {
        let request = ModelRequest {
            model: "claude-sonnet-4-5-20250929".into(),
            max_tokens: 1024,
            system: Vec::new(),
            messages: vec![bingo_sdk::Message::text(
                bingo_sdk::Role::User,
                "does not matter",
            )],
            tools: Vec::new(),
            reasoning: None,
            provider_options: Default::default(),
        };
        let error = hermetic(None)
            .stream(request, CancellationToken::new())
            .await
            .err();
        assert!(
            matches!(error, Some(ProviderError::Auth { .. })),
            "{error:?}"
        );
        assert_eq!(
            error.map(|e| e.code()),
            Some(bingo_sdk::ErrorCode::AuthRequired)
        );
    }

    #[test]
    fn the_base_url_defaults_and_a_setting_overrides_it_without_a_trailing_slash() {
        if std::env::var(BASE_URL_ENV).is_err() {
            let default = AnthropicProvider::new(
                AnthropicConfig::default(),
                PathBuf::from("/tmp/settings.json"),
            );
            assert_eq!(default.base_url(), DEFAULT_BASE_URL);
        }
        let custom = AnthropicProvider::with_endpoint(None, "http://127.0.0.1:8080/");
        assert_eq!(custom.base_url(), "http://127.0.0.1:8080");
    }
}

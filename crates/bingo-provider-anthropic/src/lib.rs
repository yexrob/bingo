//! The Anthropic Messages API as a `Provider` plugin.
//!
//! One HTTP client, one endpoint, no retries: the provider *classifies* a
//! failure and hands it back, and the turn loop owns the retry ladder and the
//! overflow compaction (`crates/bingo-core/src/turn.rs`). Everything below
//! `lib.rs` is pure — request encoding, SSE framing, the event state machine,
//! error classification, the catalogue reader — so the wire format is pinned
//! by fixtures and snapshots rather than by a live endpoint.

pub mod error;
pub mod events;
pub mod instances;
pub mod key;
pub mod models;
pub mod request;
pub mod settings;
pub mod sse;
pub mod stream;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    AuthStatus, CancellationToken, ConfigClaim, EndpointCapabilities, LoginMethod, Merge,
    ModelInfo, ModelRequest, ModelStream, Plugin, PluginError, PluginManifest, Prompter, Provider,
    ProviderError, Registrar,
};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::Value;

use crate::key::ApiKey;
use crate::stream::IDLE_TIMEOUT;

pub use crate::settings::{AnthropicConfig, AnthropicEndpoint, Settings};

/// The endpoint every Claude account shares.
pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

/// The id the default endpoint registers under; an instance registers under
/// its own name (ADR-0017 §2).
const PROVIDER_ID: &str = "anthropic";

/// The Messages API version this adapter speaks (old
/// `providers/anthropic.rs:432-436`).
const API_VERSION: &str = "2023-06-01";

const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
const BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";

/// One endpoint, one key, one name. Cheap to clone through the `Arc` the
/// registry holds.
#[derive(Debug)]
pub struct AnthropicProvider {
    http: reqwest::Client,
    /// What `--provider`, `/model <id>/<model>` and `auth.json` call this
    /// endpoint: `anthropic`, or an instance's own name.
    id: String,
    key: ApiKey,
    base_url: String,
    images: bool,
}

impl AnthropicProvider {
    /// One endpoint under its own name.
    pub fn keyed(
        id: impl Into<String>,
        key: ApiKey,
        base_url: impl Into<String>,
        images: bool,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            id: id.into(),
            key,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            images,
        }
    }

    /// An endpoint as given, with no store and no environment lookup — what a
    /// test or an embedder uses when the credentials are already resolved.
    pub fn with_endpoint(api_key: Option<String>, base_url: impl Into<String>) -> Self {
        let key = ApiKey::detached(PROVIDER_ID, instances::default_places(None), api_key);
        Self::keyed(PROVIDER_ID, key, base_url, true)
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Missing here rather than at the first request: `auth()` reads the same
    /// key, so the CLI can fail with `AUTH_REQUIRED` before any turn starts.
    fn headers(&self) -> Result<HeaderMap, ProviderError> {
        let key = self.key.bearer()?;
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&key).map_err(|e| ProviderError::Auth {
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
        &self.id
    }

    /// Every Claude endpoint counts tokens and caches prefixes, and forwards
    /// images unless a proxy says it strips them; what each model can do is
    /// the kernel catalogue's to say (ADR-0004).
    fn endpoint(&self, _model: &str) -> EndpointCapabilities {
        EndpointCapabilities {
            images: self.images,
            count_tokens: true,
            caching: true,
        }
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        let body = request::encode(&request, &self.endpoint(&request.model));
        let response = self.send(self.post("/v1/messages", &body)?).await?;
        Ok(stream::model_stream(stream::chunks(response), cancel))
    }

    async fn count_tokens(&self, request: &ModelRequest) -> Result<u64, ProviderError> {
        let body = request::count_tokens(request, &self.endpoint(&request.model));
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
        self.key.status()
    }

    /// A key is pasted, never negotiated (ADR-0017 §4).
    async fn login(
        &self,
        prompter: Arc<dyn Prompter>,
        method: Option<LoginMethod>,
    ) -> Result<String, ProviderError> {
        self.key.login(prompter, method).await
    }

    async fn logout(&self) -> Result<String, ProviderError> {
        self.key.forget()
    }
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
        // url replaces the user's pair whole rather than half-overriding it —
        // its instances with it (ADR-0017).
        keys: &[("anthropic", Merge::Replace)],
        schema: settings_schema,
    }),
};

/// Registers what the `anthropic` key names: the default endpoint, and one
/// provider per instance under it (ADR-0017 §2).
#[derive(Debug, Default, Clone, Copy)]
pub struct AnthropicPlugin;

#[async_trait]
impl Plugin for AnthropicPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        for provider in instances::providers(settings, registrar.env())? {
            registrar.provider(provider);
        }
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

    fn providers(directory: &tempfile::TempDir, settings: Value) -> Vec<Arc<dyn Provider>> {
        let mut registrar = Registrar::new(
            "bingo.provider.anthropic",
            settings,
            bingo_sdk::Env::rooted(directory.path()),
        );
        AnthropicPlugin.register(&mut registrar).expect("register");
        registrar
            .into_contributions()
            .into_iter()
            .map(|contribution| match contribution {
                Contribution::Provider(provider) => provider,
                other => panic!("expected a provider, got {other:?}"),
            })
            .collect()
    }

    fn ids(providers: &[Arc<dyn Provider>]) -> Vec<String> {
        providers.iter().map(|p| p.id().to_string()).collect()
    }

    #[test]
    fn the_plugin_registers_a_provider_per_endpoint_the_settings_name() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        assert_eq!(ids(&providers(&directory, json!({}))), ["anthropic"]);
        assert_eq!(
            ids(&providers(
                &directory,
                json!({ "anthropic": { "instances": { "proxy2": {}, "proxy1": {} } } })
            )),
            ["anthropic", "proxy1", "proxy2"],
            "the default first, then the instances in the order a person reads"
        );
        assert_eq!(MANIFEST.provides, &["provider:anthropic"]);
        assert_eq!(MANIFEST.id, "bingo.provider.anthropic");
    }

    #[test]
    fn an_instance_that_takes_a_registered_name_is_refused_at_boot() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let mut registrar = Registrar::new(
            "bingo.provider.anthropic",
            json!({ "anthropic": { "instances": { "openai": {} } } }),
            bingo_sdk::Env::rooted(directory.path()),
        );
        let refused = AnthropicPlugin
            .register(&mut registrar)
            .expect_err("a refusal")
            .to_string();
        assert!(refused.contains("`openai`"), "{refused}");
    }

    #[test]
    fn the_claimed_key_merges_by_replacement_and_has_a_schema() {
        let claim = MANIFEST.config.expect("the plugin claims settings");
        assert_eq!(claim.keys, &[("anthropic", Merge::Replace)]);
        let schema = serde_json::to_value((claim.schema)()).expect("a json schema");
        let schema = schema.to_string();
        for key in ["apiKey", "baseUrl", "images", "instances"] {
            assert!(schema.contains(key), "the schema names {key}: {schema}");
        }
    }

    #[test]
    fn a_claimed_api_key_reaches_the_provider() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let providers = providers(
            &directory,
            json!({ "anthropic": { "apiKey": "sk-ant-from-settings" } }),
        );
        assert_eq!(providers[0].auth(), AuthStatus::Ready);
    }

    /// Each instance's credential is its own: the store entry under its name,
    /// else its own `apiKey`. Nothing ambient reaches a named one.
    #[test]
    fn an_instance_reads_its_own_key_and_nothing_ambient() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = bingo_auth_oauth::CredentialStore::new(directory.path().join(".bingo/data"));
        store
            .write(
                "proxy2",
                bingo_auth_oauth::Entry::Api {
                    key: "sk-pasted".into(),
                },
            )
            .expect("a write");
        let providers = providers(
            &directory,
            json!({ "anthropic": { "instances": {
                "proxy1": { "apiKey": "sk-one", "images": false },
                "proxy2": {},
                "proxy3": {},
            }}}),
        );
        let by_id = |id: &str| {
            providers
                .iter()
                .find(|p| p.id() == id)
                .unwrap_or_else(|| panic!("no {id}"))
                .clone()
        };
        assert_eq!(by_id("proxy1").auth(), AuthStatus::Ready);
        assert!(!by_id("proxy1").endpoint("claude-sonnet-4-5").images);
        assert_eq!(
            by_id("proxy2").auth(),
            AuthStatus::Ready,
            "the store entry under the instance's own name is its key"
        );
        assert!(
            matches!(by_id("proxy3").auth(), AuthStatus::Missing { hint }
                if hint.contains("/login proxy3") && !hint.contains(API_KEY_ENV)),
            "an instance names its own sign-in and no variable: {:?}",
            by_id("proxy3").auth()
        );
    }

    #[test]
    fn no_key_anywhere_leaves_authentication_missing() {
        assert!(matches!(hermetic(None).auth(), AuthStatus::Missing { .. }));
        assert_eq!(hermetic(Some("sk-ant-test")).auth(), AuthStatus::Ready);
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
        assert_eq!(hermetic(None).base_url(), DEFAULT_BASE_URL);
        let custom = AnthropicProvider::with_endpoint(None, "http://127.0.0.1:8080/");
        assert_eq!(custom.base_url(), "http://127.0.0.1:8080");
    }

    /// The same provider a person runs: `/login anthropic` pastes a key into
    /// `auth.json`, the endpoint sends it, and `/logout anthropic` takes it
    /// out (ADR-0017 §4).
    #[tokio::test]
    async fn the_default_key_provider_takes_a_pasted_key() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = Arc::new(bingo_auth_oauth::CredentialStore::new(
            directory.path().to_path_buf(),
        ));
        let key = ApiKey::new(
            PROVIDER_ID,
            instances::default_places(None),
            store.clone(),
            None,
        );
        let provider = AnthropicProvider::keyed(PROVIDER_ID, key, DEFAULT_BASE_URL, true);
        assert_eq!(
            provider
                .login(Arc::new(Pasting), None)
                .await
                .expect("a paste"),
            "Signed in to anthropic with a pasted key."
        );
        assert_eq!(
            provider
                .headers()
                .expect("headers")
                .get("x-api-key")
                .and_then(|value| value.to_str().ok()),
            Some("sk-ant-pasted")
        );
        assert_eq!(
            provider.logout().await.expect("a logout"),
            "Signed out of anthropic."
        );
        assert!(matches!(provider.auth(), AuthStatus::Missing { .. }));
    }

    /// A person at the paste dialog.
    struct Pasting;

    #[async_trait]
    impl Prompter for Pasting {
        async fn ask(
            &self,
            _kind: bingo_sdk::InteractionKind,
            _answers: Vec<bingo_sdk::AnswerSpec>,
        ) -> Result<bingo_sdk::Answer, bingo_sdk::KernelError> {
            Ok(bingo_sdk::Answer::Text {
                text: "sk-ant-pasted".into(),
            })
        }
    }
}

//! The OpenAI Responses API as a `Provider` plugin.
//!
//! One HTTP client, one endpoint, no retries: the provider *classifies* a
//! failure and hands it back, and the turn loop owns the retry ladder and the
//! overflow compaction (`crates/bingo-core/src/turn.rs`). Everything below
//! `lib.rs` is pure — request encoding, SSE framing, the event state machine,
//! error classification, the effort table, the catalogue reader — so the wire
//! format is pinned by fixtures and snapshots rather than by a live endpoint.
//!
//! Stateless by design: `store` is always `false`, so the journal stays the
//! source of truth and every turn re-sends the whole conversation, carrying
//! the model's encrypted reasoning state with it.

pub mod effort;
pub mod error;
pub mod events;
pub mod input;
pub mod models;
pub mod request;
pub mod sse;
pub mod stream;
pub mod variant;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    AuthStatus, CancellationToken, ConfigClaim, EndpointCapabilities, Merge, ModelInfo,
    ModelRequest, ModelStream, Plugin, PluginError, PluginManifest, Provider, ProviderError,
    Registrar,
};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::stream::IDLE_TIMEOUT;
use crate::variant::{ORIGINATOR, Variant};

/// The endpoint every OpenAI API key shares.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";

const API_KEY_ENV: &str = "OPENAI_API_KEY";
const BASE_URL_ENV: &str = "OPENAI_BASE_URL";

/// The `openai` settings key.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct OpenAiConfig {
    pub api_key: Option<String>,
    pub base_url: Option<String>,
    /// Whether image parts reach the model. `false` for a proxy that strips
    /// them: what the *model* can see is the kernel catalogue's to say
    /// (ADR-0004), this is only what the endpoint forwards.
    pub images: bool,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: None,
            images: true,
        }
    }
}

/// The slice the host hands `register`: the claimed keys and nothing else.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Settings {
    pub openai: OpenAiConfig,
}

/// One endpoint, one key. Cheap to clone through the `Arc` the registry holds.
#[derive(Debug)]
pub struct OpenAiProvider {
    http: reqwest::Client,
    api_key: Option<String>,
    base_url: String,
    variant: Variant,
    images: bool,
    /// Where a person puts a key, named in the auth status.
    settings_file: Option<PathBuf>,
}

impl OpenAiProvider {
    /// The settings slice, resolved against the environment.
    pub fn new(config: OpenAiConfig, settings_file: PathBuf) -> Self {
        let mut provider = Self::with_endpoint(
            resolve(API_KEY_ENV, config.api_key),
            resolve(BASE_URL_ENV, config.base_url).unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
        );
        provider.images = config.images;
        provider.settings_file = Some(settings_file);
        provider
    }

    /// An endpoint as given, with no environment lookup — what a test or an
    /// embedder uses when the credentials are already resolved.
    pub fn with_endpoint(api_key: Option<String>, base_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            variant: Variant::Default,
            images: true,
            settings_file: None,
        }
    }

    /// The ChatGPT subscription endpoint. Encoded and tested now; registered
    /// when OAuth lands (M10), because it has no API-key form.
    pub fn with_variant(mut self, variant: Variant) -> Self {
        self.variant = variant;
        self
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn variant(&self) -> Variant {
        self.variant
    }

    fn missing_key_hint(&self) -> String {
        match &self.settings_file {
            Some(file) => format!(
                "Set {API_KEY_ENV}, or add \"openai\": {{\"apiKey\": \"...\"}} to {}.",
                file.display()
            ),
            None => format!("Set {API_KEY_ENV}, or configure openai.apiKey in settings."),
        }
    }

    /// Missing here rather than at the first request: `auth()` reads the same
    /// field, so the CLI can fail with `AUTH_REQUIRED` before any turn starts.
    fn headers(&self) -> Result<HeaderMap, ProviderError> {
        let key = self.api_key.as_deref().ok_or_else(|| ProviderError::Auth {
            message: format!("no OpenAI API key: set {API_KEY_ENV} or the `openai.apiKey` setting"),
        })?;
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {key}")).map_err(|e| ProviderError::Auth {
                message: format!("the api key is not a valid header value: {e}"),
            })?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if self.variant == Variant::Codex {
            add_codex_headers(&mut headers, key);
        }
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
impl Provider for OpenAiProvider {
    fn id(&self) -> &str {
        self.variant.provider_id()
    }

    /// Responses caches prefixes on its own and cannot count tokens ahead of
    /// a request; what each model can do is the kernel catalogue's to say
    /// (ADR-0004).
    fn endpoint(&self, _model: &str) -> EndpointCapabilities {
        EndpointCapabilities {
            images: self.images,
            count_tokens: false,
            caching: true,
        }
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        let body = request::encode(&request, self.variant);
        let response = self.send(self.post(self.variant.path(), &body)?).await?;
        Ok(stream::model_stream(stream::chunks(response), cancel))
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

/// The subscription endpoint identifies its client and routes by account; the
/// account id is a claim inside the bearer token itself. A token that is not
/// a JWT simply omits the header, which is what an API key against the public
/// endpoint would do anyway.
fn add_codex_headers(headers: &mut HeaderMap, token: &str) {
    headers.insert("originator", HeaderValue::from_static(ORIGINATOR));
    if let Some(account) = variant::account_id(token)
        && let Ok(value) = HeaderValue::from_str(&account)
    {
        headers.insert("ChatGPT-Account-Id", value);
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
    id: "bingo.provider.openai",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["provider:openai"],
    requires: &[],
    config: Some(ConfigClaim {
        // One endpoint at a time: a project that names its own key and base
        // url replaces the user's trio rather than half-overriding it.
        keys: &[("openai", Merge::Replace)],
        schema: settings_schema,
    }),
};

/// Registers one `OpenAiProvider`, built from the `openai` settings key.
#[derive(Debug, Default, Clone, Copy)]
pub struct OpenAiPlugin;

#[async_trait]
impl Plugin for OpenAiPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        let settings_file = registrar.env().config_dir.join("settings.json");
        let provider = OpenAiProvider::new(settings.openai, settings_file);
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
    /// hold on a machine that already exports `OPENAI_API_KEY`.
    fn hermetic(key: Option<&str>) -> OpenAiProvider {
        OpenAiProvider::with_endpoint(key.map(str::to_string), DEFAULT_BASE_URL)
    }

    fn header_of(provider: &OpenAiProvider, name: &str) -> Option<String> {
        provider
            .headers()
            .expect("headers")
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    #[test]
    fn the_plugin_registers_the_provider_it_claims() {
        let mut registrar = Registrar::new(
            "bingo.provider.openai",
            json!({}),
            bingo_sdk::Env::rooted("/tmp"),
        );
        OpenAiPlugin.register(&mut registrar).expect("register");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), 1);
        match &contributions[0] {
            Contribution::Provider(provider) => assert_eq!(provider.id(), "openai"),
            other => panic!("expected a provider, got {other:?}"),
        }
        assert_eq!(MANIFEST.provides, &["provider:openai"]);
        assert_eq!(MANIFEST.id, "bingo.provider.openai");
    }

    #[test]
    fn the_claimed_key_merges_by_replacement_and_has_a_schema() {
        let claim = MANIFEST.config.expect("the plugin claims settings");
        assert_eq!(claim.keys, &[("openai", Merge::Replace)]);
        let schema = serde_json::to_value((claim.schema)()).expect("a json schema");
        let schema = schema.to_string();
        for key in ["apiKey", "baseUrl", "images"] {
            assert!(schema.contains(key), "the schema names {key}: {schema}");
        }
    }

    #[test]
    fn a_claimed_api_key_reaches_the_provider() {
        let mut registrar = Registrar::new(
            "bingo.provider.openai",
            json!({ "openai": { "apiKey": "sk-from-settings" } }),
            bingo_sdk::Env::rooted("/tmp"),
        );
        OpenAiPlugin.register(&mut registrar).expect("register");
        match &registrar.into_contributions()[0] {
            Contribution::Provider(provider) => assert_eq!(provider.auth(), AuthStatus::Ready),
            other => panic!("expected a provider, got {other:?}"),
        }
    }

    #[test]
    fn images_default_to_forwarded_and_a_proxy_can_turn_them_off() {
        assert!(hermetic(None).endpoint("gpt-5.4").images);
        let stripped = OpenAiProvider::new(
            OpenAiConfig {
                images: false,
                ..OpenAiConfig::default()
            },
            PathBuf::from("/tmp/settings.json"),
        );
        assert_eq!(
            stripped.endpoint("gpt-5.4"),
            EndpointCapabilities {
                images: false,
                count_tokens: false,
                caching: true,
            }
        );
    }

    /// `std::env::set_var` is unsafe in Rust 2024 and this workspace forbids
    /// `unsafe`, so the environment half of the precedence rule is exercised
    /// through the resolver the provider is built from.
    #[test]
    fn no_key_anywhere_leaves_authentication_missing() {
        assert!(matches!(hermetic(None).auth(), AuthStatus::Missing { .. }));
        assert_eq!(hermetic(Some("sk-test")).auth(), AuthStatus::Ready);
        assert_eq!(resolve("BINGO_NO_SUCH_VARIABLE", None), None);
        assert_eq!(resolve("BINGO_NO_SUCH_VARIABLE", Some("  ".into())), None);
        assert_eq!(
            resolve("BINGO_NO_SUCH_VARIABLE", Some(" from-settings ".into())),
            Some("from-settings".into())
        );
    }

    #[test]
    fn the_missing_key_hint_names_the_variable_and_the_settings_file() {
        let provider = OpenAiProvider::new(
            OpenAiConfig::default(),
            PathBuf::from("/home/me/.config/bingo/settings.json"),
        );
        assert_eq!(
            provider.missing_key_hint(),
            "Set OPENAI_API_KEY, or add \"openai\": {\"apiKey\": \"...\"} to \
             /home/me/.config/bingo/settings.json."
        );
        assert_eq!(
            hermetic(None).auth(),
            AuthStatus::Missing {
                hint: "Set OPENAI_API_KEY, or configure openai.apiKey in settings.".into()
            },
            "with no settings file the hint still names both places"
        );
    }

    #[tokio::test]
    async fn without_a_key_a_turn_fails_before_it_reaches_the_wire() {
        let request = ModelRequest {
            model: "gpt-5.4".into(),
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
            let default =
                OpenAiProvider::new(OpenAiConfig::default(), PathBuf::from("/tmp/settings.json"));
            assert_eq!(default.base_url(), DEFAULT_BASE_URL);
        }
        let custom = OpenAiProvider::with_endpoint(None, "http://127.0.0.1:8080/");
        assert_eq!(custom.base_url(), "http://127.0.0.1:8080");
    }

    #[test]
    fn the_public_endpoint_sends_a_bearer_key_and_nothing_of_the_subscription() {
        let provider = hermetic(Some("sk-test"));
        assert_eq!(
            header_of(&provider, "authorization").as_deref(),
            Some("Bearer sk-test")
        );
        assert_eq!(header_of(&provider, "originator"), None);
        assert_eq!(header_of(&provider, "chatgpt-account-id"), None);
        assert_eq!(provider.id(), "openai");
    }

    #[test]
    fn the_subscription_endpoint_adds_its_originator_and_account() {
        let token = codex_token("acc_42");
        let provider = hermetic(Some(&token)).with_variant(Variant::Codex);
        assert_eq!(
            header_of(&provider, "originator").as_deref(),
            Some(ORIGINATOR)
        );
        assert_eq!(
            header_of(&provider, "chatgpt-account-id").as_deref(),
            Some("acc_42")
        );
        assert_eq!(provider.id(), "codex");
    }

    #[test]
    fn a_subscription_token_with_no_account_claim_omits_the_header() {
        let provider = hermetic(Some("not-a-jwt")).with_variant(Variant::Codex);
        assert_eq!(
            header_of(&provider, "originator").as_deref(),
            Some(ORIGINATOR)
        );
        assert_eq!(header_of(&provider, "chatgpt-account-id"), None);
    }

    fn codex_token(account: &str) -> String {
        use base64::Engine;
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let payload = json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": account }
        });
        format!(
            "{}.{}.signature",
            URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(payload.to_string())
        )
    }
}

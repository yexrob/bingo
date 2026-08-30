//! The OpenAI Responses API as a `Provider` plugin — twice: the public
//! endpoint with an API key, and the ChatGPT subscription with an OAuth
//! bearer (ADR-0012 §6).
//!
//! One HTTP client, one endpoint per instance, no retries but one: the
//! provider *classifies* a failure and hands it back, and the turn loop owns
//! the retry ladder and the overflow compaction
//! (`crates/bingo-core/src/turn.rs`). The exception is a 401 on a
//! subscription bearer, which nothing above here could act on — a stale
//! access token is renewed and the request goes again once.
//!
//! Everything below `lib.rs` is pure — request encoding, SSE framing, the
//! event state machine, error classification, the effort table, the two
//! catalogue readers — so the wire format is pinned by fixtures and snapshots
//! rather than by a live endpoint. The flows behind `Credential::Tokens` live
//! in `bingo-auth-oauth`, the library tier, so a second subscription provider
//! need not import this one.
//!
//! Stateless by design: `store` is always `false`, so the journal stays the
//! source of truth and every turn re-sends the whole conversation, carrying
//! the model's encrypted reasoning state with it.

pub mod credential;
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
use std::time::Duration;

use async_trait::async_trait;
use bingo_auth_oauth::{CredentialStore, Issuer, TokenSource, jwt};
use bingo_sdk::{
    AuthStatus, CancellationToken, ConfigClaim, EndpointCapabilities, LoginMethod, Merge,
    ModelInfo, ModelRequest, ModelStream, Plugin, PluginError, PluginManifest, Prompter, Provider,
    ProviderError, Registrar,
};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::credential::Credential;
use crate::stream::IDLE_TIMEOUT;
use crate::variant::{ORIGINATOR, Variant};

/// The endpoint every OpenAI API key shares.
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com";

/// The subscription endpoint. The public one rejects a subscription bearer,
/// so this is not a base url a person chooses — only a proxy overrides it.
pub const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";

const API_KEY_ENV: &str = "OPENAI_API_KEY";
const BASE_URL_ENV: &str = "OPENAI_BASE_URL";

/// codex's own OAuth client and issuer (`openai/codex`, `codex-rs/login`).
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CODEX_ISSUER: &str = "https://auth.openai.com";

/// The catalogue path. The endpoint rejects a `client_version` that is not a
/// semver, so the version is sent as one.
const CODEX_MODELS_PATH: &str = "/codex/models?client_version=0.146.0";

/// A model menu waits for nobody: past this the static list is the answer.
const CODEX_MODELS_TIMEOUT: Duration = Duration::from_secs(10);

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

/// The `codex` settings key. No key and no token: a subscription credential
/// only ever comes from a login, and both fields exist for a proxy or a test.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct CodexConfig {
    pub base_url: Option<String>,
    pub issuer: Option<String>,
}

/// The slice the host hands `register`: the claimed keys and nothing else.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Settings {
    pub openai: OpenAiConfig,
    pub codex: CodexConfig,
}

/// One endpoint, one credential. Cheap to clone through the `Arc` the
/// registry holds.
#[derive(Debug)]
pub struct OpenAiProvider {
    http: reqwest::Client,
    credential: Credential,
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

    /// The ChatGPT subscription (ADR-0012 §6). The store is the host's, shared
    /// with every other provider that keeps a credential in it.
    pub fn codex(config: CodexConfig, store: Arc<CredentialStore>) -> Self {
        let source = TokenSource::new(
            Variant::Codex.provider_id(),
            codex_issuer(config.issuer),
            store,
            reqwest::Client::new(),
        );
        let base_url = config
            .base_url
            .unwrap_or_else(|| CODEX_BASE_URL.to_string());
        Self::with_tokens(Arc::new(source), base_url).with_variant(Variant::Codex)
    }

    /// An endpoint as given, with no environment lookup — what a test or an
    /// embedder uses when the credentials are already resolved.
    pub fn with_endpoint(api_key: Option<String>, base_url: impl Into<String>) -> Self {
        Self::with_credential(Credential::Key(api_key), base_url)
    }

    /// The same, over a credential that renews itself.
    pub fn with_tokens(source: Arc<TokenSource>, base_url: impl Into<String>) -> Self {
        Self::with_credential(Credential::Tokens(source), base_url)
    }

    fn with_credential(credential: Credential, base_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            credential,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            variant: Variant::Default,
            images: true,
            settings_file: None,
        }
    }

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
    /// credential, so the CLI can fail with `AUTH_REQUIRED` before any turn
    /// starts.
    async fn headers(&self) -> Result<HeaderMap, ProviderError> {
        self.compose(&self.credential.bearer().await?)
    }

    /// The header table for one bearer, so a retry after a refresh writes the
    /// new token into exactly the headers the first attempt carried.
    fn compose(&self, bearer: &str) -> Result<HeaderMap, ProviderError> {
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {bearer}")).map_err(|e| {
                ProviderError::Auth {
                    message: format!("the credential is not a valid header value: {e}"),
                }
            })?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if self.variant == Variant::Codex {
            add_codex_headers(&mut headers, bearer);
        }
        Ok(headers)
    }

    async fn post(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        Ok(self
            .http
            .post(format!("{}{path}", self.base_url))
            .headers(self.headers().await?)
            .json(body))
    }

    async fn get(&self, path: &str) -> Result<reqwest::RequestBuilder, ProviderError> {
        Ok(self
            .http
            .get(format!("{}{path}", self.base_url))
            .headers(self.headers().await?))
    }

    /// One round trip, with one second chance for a subscription bearer.
    async fn send(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        let again = builder.try_clone();
        match self.round_trip(builder).await {
            Err(error) if refused(&error) => self.after_refresh(again, error).await,
            result => result,
        }
    }

    /// A refusal on a subscription bearer is usually an access token that
    /// expired since the last request: the source renews once, the request
    /// goes again with the new bearer, and a second refusal is a credential a
    /// person has to sign in again.
    async fn after_refresh(
        &self,
        again: Option<reqwest::RequestBuilder>,
        first: ProviderError,
    ) -> Result<reqwest::Response, ProviderError> {
        let (Credential::Tokens(source), Some(again)) = (&self.credential, again) else {
            return Err(first);
        };
        let bearer = source
            .refreshed()
            .await
            .map_err(|error| credential::failure(source.provider(), error))?;
        match self.round_trip(again.headers(self.compose(&bearer)?)).await {
            Err(error) if refused(&error) => Err(ProviderError::Auth {
                message: credential::sign_in_again(source.provider()),
            }),
            result => result,
        }
    }

    /// A non-success status never leaves this function: every caller above it
    /// sees a classified `ProviderError` instead. The wait for the response
    /// carries the same idle guard as the body that follows it, because one
    /// silence is worth exactly as much as the other.
    async fn round_trip(
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

    /// The subscription catalogue, or the list M2 recorded when it cannot be
    /// read: a `/model` menu must not go down with a catalogue endpoint.
    async fn codex_models(&self) -> Vec<ModelInfo> {
        match self.codex_catalogue().await {
            Ok(models) if !models.is_empty() => models,
            _ => models::codex_fallback(),
        }
    }

    async fn codex_catalogue(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let request = self.get(CODEX_MODELS_PATH).await?;
        let body = tokio::time::timeout(CODEX_MODELS_TIMEOUT, self.json(request))
            .await
            .map_err(|_| ProviderError::Timeout)??;
        Ok(models::codex(&body))
    }

    /// Signing in is a subscription's business; a key is set, not negotiated.
    fn subscription(&self, action: &str) -> Result<&Arc<TokenSource>, ProviderError> {
        match &self.credential {
            Credential::Tokens(source) => Ok(source),
            Credential::Key(_) => Err(ProviderError::Unsupported {
                message: format!("{action}: the {} provider takes an API key", self.id()),
            }),
        }
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
        let response = self
            .send(self.post(self.variant.path(), &body).await?)
            .await?;
        Ok(stream::model_stream(stream::chunks(response), cancel))
    }

    async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        match self.variant {
            Variant::Codex => Ok(self.codex_models().await),
            Variant::Default => Ok(models::parse(
                &self.json(self.get("/v1/models").await?).await?,
            )),
        }
    }

    fn auth(&self) -> AuthStatus {
        self.credential.status(|| self.missing_key_hint())
    }

    async fn login(
        &self,
        prompter: Arc<dyn Prompter>,
        method: Option<LoginMethod>,
    ) -> Result<String, ProviderError> {
        let source = self.subscription("login")?;
        // A browser the flow cannot open is a flow a person cannot finish, so
        // the opt-out the library reads is read here too.
        let open_browser = std::env::var_os(bingo_auth_oauth::browser::NO_BROWSER_ENV).is_none();
        let tokens = source
            .login(
                prompter,
                method.unwrap_or(LoginMethod::Browser),
                open_browser,
            )
            .await
            .map_err(|error| credential::failure(self.id(), error))?;
        Ok(receipt(self.id(), tokens.email().or(tokens.account_id)))
    }

    async fn logout(&self) -> Result<String, ProviderError> {
        self.subscription("logout")?
            .logout()
            .await
            .map_err(|error| credential::failure(self.id(), error))?;
        Ok(format!("Signed out of {}.", self.id()))
    }
}

/// The subscription endpoint identifies its client and routes by account; the
/// account id is a claim inside the bearer token itself. A token that is not
/// a JWT simply omits the header, which is what an API key against the public
/// endpoint would do anyway.
fn add_codex_headers(headers: &mut HeaderMap, token: &str) {
    headers.insert("originator", HeaderValue::from_static(ORIGINATOR));
    if let Some(account) = jwt::account_id(token)
        && let Ok(value) = HeaderValue::from_str(&account)
    {
        headers.insert("ChatGPT-Account-Id", value);
    }
}

/// A person reads this line after a login; it names who they signed in as
/// when the issuer said, and nothing invented when it did not.
fn receipt(provider: &str, who: Option<String>) -> String {
    match who {
        Some(who) => format!("Signed in to {provider} as {who}."),
        None => format!("Signed in to {provider}."),
    }
}

/// 401 and 403 are one thing to the classifier and one thing here: the
/// credential, not the request.
fn refused(error: &ProviderError) -> bool {
    matches!(error, ProviderError::Auth { .. })
}

/// The endpoints ADR-0012 §6 lists, verified against codex's own source. Only
/// the base moves, and only for a proxy or a test.
fn codex_issuer(base: Option<String>) -> Issuer {
    Issuer {
        client_id: CODEX_CLIENT_ID.into(),
        base: base.unwrap_or_else(|| CODEX_ISSUER.to_string()),
        authorize_path: "/oauth/authorize".into(),
        token_path: "/oauth/token".into(),
        revoke_path: "/oauth/revoke".into(),
        device_code_path: "/api/accounts/deviceauth/usercode".into(),
        device_token_path: "/api/accounts/deviceauth/token".into(),
        device_verify_path: "/codex/device".into(),
        scope: "openid profile email offline_access".into(),
        // Without `codex_cli_simplified_flow` the issuer routes to the web
        // flow and the login ends in an authentication error.
        authorize_extra: vec![
            ("codex_cli_simplified_flow".into(), "true".into()),
            ("id_token_add_organizations".into(), "true".into()),
            ("originator".into(), ORIGINATOR.into()),
        ],
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
    provides: &["provider:openai", "provider:codex"],
    requires: &[],
    config: Some(ConfigClaim {
        // One endpoint at a time: a project that names its own key and base
        // url replaces the user's trio rather than half-overriding it.
        keys: &[("openai", Merge::Replace), ("codex", Merge::Replace)],
        schema: settings_schema,
    }),
};

/// Registers the two providers this one wire format serves: `openai` from the
/// `openai` settings key, `codex` from the credential store.
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
        let store = Arc::new(CredentialStore::new(registrar.env().data_dir.clone()));
        let openai = OpenAiProvider::new(settings.openai, settings_file);
        let codex = OpenAiProvider::codex(settings.codex, store);
        registrar.provider(Arc::new(openai) as Arc<dyn Provider>);
        registrar.provider(Arc::new(codex) as Arc<dyn Provider>);
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use bingo_auth_oauth::{Entry, tokens::unix_now};
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

    /// A subscription provider over a store in a temporary directory: the
    /// bearer comes from `auth.json` the way it does in a real session.
    fn subscribed(directory: &tempfile::TempDir, entry: Option<Entry>) -> OpenAiProvider {
        subscribed_to(directory, entry, None)
    }

    fn subscribed_to(
        directory: &tempfile::TempDir,
        entry: Option<Entry>,
        issuer: Option<String>,
    ) -> OpenAiProvider {
        let store = Arc::new(CredentialStore::new(directory.path().to_path_buf()));
        if let Some(entry) = entry {
            store.write("codex", entry).expect("a write");
        }
        OpenAiProvider::codex(
            CodexConfig {
                base_url: None,
                issuer,
            },
            store,
        )
    }

    fn signed_in(access: &str) -> Entry {
        Entry::OAuth {
            access: access.into(),
            refresh: "rt-1".into(),
            expires: unix_now() + 3_600,
            account_id: None,
        }
    }

    async fn header_of(provider: &OpenAiProvider, name: &str) -> Option<String> {
        provider
            .headers()
            .await
            .expect("headers")
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    fn registered(directory: &tempfile::TempDir) -> Vec<Contribution> {
        let mut registrar = Registrar::new(
            "bingo.provider.openai",
            json!({}),
            bingo_sdk::Env::rooted(directory.path()),
        );
        OpenAiPlugin.register(&mut registrar).expect("register");
        registrar.into_contributions()
    }

    fn ids(contributions: &[Contribution]) -> Vec<String> {
        contributions
            .iter()
            .map(|contribution| match contribution {
                Contribution::Provider(provider) => provider.id().to_string(),
                other => panic!("expected a provider, got {other:?}"),
            })
            .collect()
    }

    #[test]
    fn one_plugin_registers_both_providers_it_claims() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        assert_eq!(ids(&registered(&directory)), ["openai", "codex"]);
        assert_eq!(MANIFEST.provides, &["provider:openai", "provider:codex"]);
        assert_eq!(MANIFEST.id, "bingo.provider.openai");
    }

    #[test]
    fn both_claimed_keys_merge_by_replacement_and_have_a_schema() {
        let claim = MANIFEST.config.expect("the plugin claims settings");
        assert_eq!(
            claim.keys,
            &[("openai", Merge::Replace), ("codex", Merge::Replace)]
        );
        let schema = serde_json::to_value((claim.schema)()).expect("a json schema");
        let schema = schema.to_string();
        for key in ["apiKey", "baseUrl", "images", "issuer"] {
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
    fn the_codex_settings_key_moves_both_endpoints_for_a_proxy() {
        let provider = OpenAiProvider::codex(
            CodexConfig {
                base_url: Some("http://127.0.0.1:8080/".into()),
                issuer: Some("http://127.0.0.1:9090".into()),
            },
            Arc::new(CredentialStore::new(PathBuf::from("/tmp"))),
        );
        assert_eq!(provider.base_url(), "http://127.0.0.1:8080");
        assert_eq!(provider.variant(), Variant::Codex);
        assert_eq!(
            codex_issuer(None).base,
            CODEX_ISSUER,
            "the default issuer is codex's own"
        );
        assert_eq!(
            OpenAiProvider::codex(
                CodexConfig::default(),
                Arc::new(CredentialStore::new(PathBuf::from("/tmp")))
            )
            .base_url(),
            CODEX_BASE_URL
        );
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

    /// Each `Status` the source can report, as the hint a person reads.
    #[tokio::test]
    async fn a_subscription_says_how_to_sign_in_and_how_to_sign_in_again() {
        let empty = tempfile::tempdir().expect("a temporary directory");
        assert_eq!(
            subscribed(&empty, None).auth(),
            AuthStatus::Missing {
                hint: "Run `bingo login codex`, or `/login codex` in a session.".into()
            }
        );

        let held = tempfile::tempdir().expect("a temporary directory");
        assert_eq!(
            subscribed(&held, Some(signed_in("at-1"))).auth(),
            AuthStatus::Ready
        );

        // The third status needs the issuer to retire the refresh token.
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/oauth/token"))
            .respond_with(
                wiremock::ResponseTemplate::new(400)
                    .set_body_json(json!({ "error": "refresh_token_expired" })),
            )
            .mount(&server)
            .await;
        let retired = tempfile::tempdir().expect("a temporary directory");
        let provider = subscribed_to(
            &retired,
            Some(Entry::OAuth {
                access: "at-old".into(),
                refresh: "rt-dead".into(),
                expires: 1,
                account_id: None,
            }),
            Some(server.uri()),
        );
        assert!(provider.headers().await.is_err(), "the refresh is refused");
        assert_eq!(
            provider.auth(),
            AuthStatus::Expired {
                hint: "Run `bingo login codex` to sign in again.".into()
            }
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

    #[tokio::test]
    async fn the_public_endpoint_sends_a_bearer_key_and_nothing_of_the_subscription() {
        let provider = hermetic(Some("sk-test"));
        assert_eq!(
            header_of(&provider, "authorization").await.as_deref(),
            Some("Bearer sk-test")
        );
        assert_eq!(header_of(&provider, "originator").await, None);
        assert_eq!(header_of(&provider, "chatgpt-account-id").await, None);
        assert_eq!(provider.id(), "openai");
    }

    #[tokio::test]
    async fn the_subscription_endpoint_adds_its_originator_and_account() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let token = codex_token("acc_42");
        let provider = subscribed(&directory, Some(signed_in(&token)));
        assert_eq!(
            header_of(&provider, "authorization").await.as_deref(),
            Some(format!("Bearer {token}").as_str())
        );
        assert_eq!(
            header_of(&provider, "originator").await.as_deref(),
            Some(ORIGINATOR)
        );
        assert_eq!(
            header_of(&provider, "chatgpt-account-id").await.as_deref(),
            Some("acc_42")
        );
        assert_eq!(provider.id(), "codex");
    }

    #[tokio::test]
    async fn a_subscription_token_with_no_account_claim_omits_the_header() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let provider = subscribed(&directory, Some(signed_in("not-a-jwt")));
        assert_eq!(
            header_of(&provider, "originator").await.as_deref(),
            Some(ORIGINATOR)
        );
        assert_eq!(header_of(&provider, "chatgpt-account-id").await, None);
    }

    #[tokio::test]
    async fn signing_in_and_out_of_an_api_key_provider_is_unsupported() {
        let provider = hermetic(Some("sk-test"));
        let prompter: Arc<dyn Prompter> = Arc::new(NoPrompter);
        let login = provider
            .login(prompter, None)
            .await
            .expect_err("unsupported");
        assert!(
            matches!(login, ProviderError::Unsupported { .. }),
            "{login:?}"
        );
        assert_eq!(login.code(), bingo_sdk::ErrorCode::InvalidInput);
        assert!(matches!(
            provider.logout().await,
            Err(ProviderError::Unsupported { .. })
        ));
    }

    #[test]
    fn a_receipt_names_the_account_only_when_the_issuer_did() {
        assert_eq!(
            receipt("codex", Some("me@example.com".into())),
            "Signed in to codex as me@example.com."
        );
        assert_eq!(receipt("codex", None), "Signed in to codex.");
    }

    struct NoPrompter;

    #[async_trait]
    impl Prompter for NoPrompter {
        async fn ask(
            &self,
            _kind: bingo_sdk::InteractionKind,
            _answers: Vec<bingo_sdk::AnswerSpec>,
        ) -> Result<bingo_sdk::Answer, bingo_sdk::KernelError> {
            panic!("an api-key provider never asks")
        }
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

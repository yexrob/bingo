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
pub mod metered;
pub mod models;
pub mod request;
pub mod settings;
pub mod sse;
pub mod stream;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bingo_sdk::{
    AuthStatus, CancellationToken, ConfigClaim, EndpointCapabilities, LoginMethod, Merge,
    ModelInfo, ModelRequest, ModelStream, Plugin, PluginError, PluginManifest, Prompter, Provider,
    ProviderError, Registrar,
};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde_json::Value;

use crate::key::ApiKey;
use crate::metered::{Clock, meter, quiet};
use crate::stream::{CONNECT_TIMEOUT, IDLE_TIMEOUT};

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
            http: http(),
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

    /// One round trip. The clock comes back out with the response: the body
    /// that follows goes on stamping the same one (ADR-0056 §2).
    async fn send(
        &self,
        builder: reqwest::RequestBuilder,
    ) -> Result<(reqwest::Response, Clock), ProviderError> {
        self.round_trip(builder, IDLE_TIMEOUT).await
    }

    /// A non-success status never leaves this function: every caller above it
    /// sees a classified `ProviderError` instead. The upload and the wait for
    /// the status line share one guard, and it is a guard on silence: the body
    /// goes out metered, so a request that is still moving is never cut for
    /// its size (ADR-0056 §1).
    async fn round_trip(
        &self,
        builder: reqwest::RequestBuilder,
        idle: Duration,
    ) -> Result<(reqwest::Response, Clock), ProviderError> {
        let clock = Clock::new();
        let request = meter(builder, &clock)?;
        let sent = tokio::select! {
            biased;
            sent = self.http.execute(request) => sent,
            () = quiet(&clock, idle) => return Err(ProviderError::Timeout),
        };
        let response = sent.map_err(|e| ProviderError::Transport {
            message: e.to_string(),
        })?;
        // The status line is a byte moving too, so the body that follows is
        // given its whole idle rather than what the upload left of it.
        clock.stamp();
        if response.status().is_success() {
            return Ok((response, clock));
        }
        Err(refusal(response).await)
    }

    async fn json(&self, builder: reqwest::RequestBuilder) -> Result<Value, ProviderError> {
        let (response, _clock) = self.send(builder).await?;
        response.json().await.map_err(|e| ProviderError::Stream {
            message: format!("unreadable response body: {e}"),
        })
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        &self.id
    }

    /// A named instance serves the same models the default does (ADR-0017).
    fn family(&self) -> &str {
        "anthropic"
    }

    /// Every Claude endpoint counts tokens and caches prefixes, and forwards
    /// images unless a proxy says it strips them; what each model can do is
    /// the kernel catalogue's to say (ADR-0004).
    fn endpoint(&self, _model: &str) -> EndpointCapabilities {
        EndpointCapabilities {
            images: self.images,
            count_tokens: true,
            caching: true,
            ..EndpointCapabilities::default()
        }
    }

    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        let body = request::encode(&request, &self.endpoint(&request.model));
        let (response, clock) = self.send(self.post("/v1/messages", &body)?).await?;
        Ok(stream::model_stream(
            stream::chunks(response),
            clock,
            cancel,
        ))
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

/// A refused response as the error the turn loop's ladder reads.
async fn refusal(response: reqwest::Response) -> ProviderError {
    let status = response.status().as_u16();
    let retry_after = header(&response, "retry-after");
    let body = response.text().await.unwrap_or_default();
    error::classify(status, &body, retry_after.as_deref())
}

/// One client per provider, carrying the only bound on the phase that has
/// nothing to meter yet: reaching the server (ADR-0056 §1). A builder that
/// will not build loses that bound, not the provider — the endpoint is still
/// reachable without it.
fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
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
            Default::default(),
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

    /// The model catalogue is asked for a provider's *family* (ADR-0017): an
    /// instance's models are the default's, or the dropdown shows it empty.
    #[test]
    fn every_instance_serves_the_anthropic_family() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let listed = providers(
            &directory,
            json!({ "anthropic": { "instances": { "proxy1": {} } } }),
        );
        assert_eq!(listed.len(), 2);
        for provider in listed {
            assert_eq!(provider.family(), "anthropic", "{}", provider.id());
        }
    }

    #[test]
    fn an_instance_that_takes_a_registered_name_is_refused_at_boot() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let mut registrar = Registrar::new(
            "bingo.provider.anthropic",
            json!({ "anthropic": { "instances": { "openai": {} } } }),
            bingo_sdk::Env::rooted(directory.path()),
            Default::default(),
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
            session: None,
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

    // The three phases over a real socket (ADR-0056 §1). Not wiremock: it
    // reads the whole request before anything answers, which is the one thing
    // these assertions are about. Every bound below is the peer's own pacing
    // multiplied out, never a wall clock.

    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpSocket, TcpStream};

    /// Small enough that the kernel cannot swallow the request whole, so the
    /// peer's pace is the upload's pace.
    const PEER_BUFFER: u32 = 64 * 1024;

    /// What the slow peer takes in one go — several frames' worth, so it is
    /// the peer that paces the clock and not the frame size.
    const SIP: usize = 256 * 1024;

    /// Bigger than the buffers on either side, so the body is still going out
    /// while the assertions watch it: 32 sips, and 32 gaps to go with them.
    const UPLOAD: usize = 8 * 1024 * 1024;

    /// The gap the slow peer leaves between two sips, and the silence a test
    /// allows. Twenty-five gaps: a machine that stalls for twenty-four of them
    /// still passes, and the upload still outlasts the silence it is given.
    const GAP: Duration = Duration::from_millis(20);
    const IDLE: Duration = Duration::from_millis(500);

    const ANSWER: &[u8] = b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}";

    /// A listener on the loopback, and the url that reaches it.
    fn peer() -> (TcpListener, String) {
        let socket = TcpSocket::new_v4().expect("a socket");
        socket
            .set_recv_buffer_size(PEER_BUFFER)
            .expect("a small receive buffer");
        socket
            .bind("127.0.0.1:0".parse().expect("a loopback address"))
            .expect("a port");
        let listener = socket.listen(1).expect("a listener");
        let address = listener.local_addr().expect("the port it took");
        (listener, format!("http://{address}"))
    }

    fn talking_to(url: &str) -> AnthropicProvider {
        AnthropicProvider::with_endpoint(Some("sk-ant-test".into()), url)
    }

    fn uploading(provider: &AnthropicProvider, url: &str) -> reqwest::RequestBuilder {
        provider.http.post(url).body("x".repeat(UPLOAD))
    }

    /// The request head, one byte at a time so that none of the body is
    /// swallowed with it.
    async fn head(socket: &mut TcpStream) -> String {
        let mut seen = Vec::new();
        let mut byte = [0u8; 1];
        while !seen.ends_with(b"\r\n\r\n") && socket.read_exact(&mut byte).await.is_ok() {
            seen.push(byte[0]);
        }
        String::from_utf8_lossy(&seen).into_owned()
    }

    fn declared_length(head: &str) -> usize {
        head.to_lowercase()
            .lines()
            .find_map(|line| {
                line.strip_prefix("content-length:")
                    .map(str::trim)
                    .map(str::to_string)
            })
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("no content length in {head:?}"))
    }

    /// Reads the request a sip at a time, `gap` apart, and answers only once
    /// the whole of it has arrived. Hands back the head it read. The sip is
    /// exact rather than whatever happens to have landed, so the drain rate is
    /// the peer's own and not the kernel's.
    async fn a_trickle(listener: TcpListener, gap: Duration) -> String {
        let (mut socket, _) = listener.accept().await.expect("a connection");
        let head = head(&mut socket).await;
        let mut left = declared_length(&head);
        let mut sip = vec![0u8; SIP];
        while left > 0 {
            tokio::time::sleep(gap).await;
            let take = SIP.min(left);
            if socket.read_exact(&mut sip[..take]).await.is_err() {
                break;
            }
            left -= take;
        }
        socket.write_all(ANSWER).await.expect("the answer");
        head
    }

    /// Takes one mouthful and never reads again, holding the connection open
    /// so that nothing but the guard can end the request.
    async fn a_deaf_peer(listener: TcpListener) {
        let (mut socket, _) = listener.accept().await.expect("a connection");
        head(&mut socket).await;
        let mut sip = vec![0u8; SIP];
        let _ = socket.read(&mut sip).await;
        std::future::pending::<()>().await;
    }

    /// Reads everything and answers nothing.
    async fn a_mute_peer(listener: TcpListener) {
        let (mut socket, _) = listener.accept().await.expect("a connection");
        let mut sink = Vec::new();
        let _ = socket.read_to_end(&mut sink).await;
    }

    /// The 8.8 MB replay of ADR-0056 in miniature: the peer takes longer than
    /// the whole idle to read the body, and the request still lands.
    #[tokio::test]
    async fn a_slow_upload_is_not_a_silence() {
        let (listener, url) = peer();
        let reading = tokio::spawn(a_trickle(listener, GAP));
        let provider = talking_to(&url);
        let started = Instant::now();
        let sent = provider.round_trip(uploading(&provider, &url), IDLE).await;
        let took = started.elapsed();
        assert!(
            sent.is_ok(),
            "a moving upload is never cut: {:?}",
            sent.err()
        );
        assert!(took > IDLE, "it outlived its own idle: {took:?}");
        reading.await.expect("the peer");
    }

    /// A peer that stops reading has gone quiet: the request ends as the
    /// `Timeout` the turn loop's ladder knows, and not before the idle.
    #[tokio::test]
    async fn a_peer_that_stops_reading_ends_the_request() {
        let (listener, url) = peer();
        let deaf = tokio::spawn(a_deaf_peer(listener));
        let provider = talking_to(&url);
        let started = Instant::now();
        let sent = provider.round_trip(uploading(&provider, &url), IDLE).await;
        let took = started.elapsed();
        assert_eq!(sent.err(), Some(ProviderError::Timeout));
        assert!(took >= IDLE, "a silence is not one until it has lasted");
        deaf.abort();
    }

    /// A server that takes the whole request and then says nothing is the
    /// silence the guard was always for: a headless run must not hang on it.
    #[tokio::test]
    async fn a_server_that_accepts_and_says_nothing_times_out() {
        let (listener, url) = peer();
        let mute = tokio::spawn(a_mute_peer(listener));
        let provider = talking_to(&url);
        let started = Instant::now();
        let asking = provider.http.post(url.as_str()).body("hello");
        assert_eq!(
            provider.round_trip(asking, IDLE).await.err(),
            Some(ProviderError::Timeout)
        );
        assert!(started.elapsed() >= IDLE);
        mute.abort();
    }

    /// An exact size hint is what keeps `Content-Length` on the request; a
    /// chunked body is a different request, and a relay may refuse it.
    #[tokio::test]
    async fn the_request_head_carries_its_length_and_is_never_chunked() {
        let (listener, url) = peer();
        let reading = tokio::spawn(a_trickle(listener, Duration::ZERO));
        let provider = talking_to(&url);
        provider
            .round_trip(uploading(&provider, &url), IDLE * 10)
            .await
            .expect("the request lands");
        let head = reading.await.expect("the peer").to_lowercase();
        assert!(
            head.contains(&format!("content-length: {UPLOAD}")),
            "the head declares the whole body: {head}"
        );
        assert!(
            !head.contains("transfer-encoding"),
            "and chunks nothing: {head}"
        );
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

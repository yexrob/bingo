//! One provider's credential over time (ADR-0012 §3).
//!
//! Everything a caller needs is one of three questions — who am I, give me a
//! bearer, that bearer just bounced — and the answers must agree with each
//! other and with the file. So the cache is the only reader of the store, a
//! refresh happens once however many callers ask at once, and `status()` is
//! synchronous: the kernel refuses a session on the same fact a `/login` in
//! the same process is about to change.

use std::sync::{Arc, Mutex};

use bingo_sdk::{Answer, AnswerSpec, InteractionKind, LoginFlow, LoginMethod, Prompter};
use serde_json::Value;
use tokio::time::Instant;

use crate::browser;
use crate::device;
use crate::error::AuthError;
use crate::exchange;
use crate::issuer::Issuer;
use crate::loopback::Loopback;
use crate::pkce;
use crate::store::{CredentialStore, Entry};
use crate::tokens::{Tokens, unix_now};

/// What a person is, as far as this provider is concerned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    SignedOut,
    SignedIn {
        account: Option<String>,
    },
    /// A credential that was there and stopped working; `reason` is the
    /// issuer's own words.
    Expired {
        reason: String,
    },
}

/// The stored credential as this process holds it.
#[derive(Clone, Debug)]
enum Cached {
    Tokens(Tokens),
    /// A key minted elsewhere: used as the bearer as it is, never refreshed.
    Key(String),
    Expired(String),
}

/// `generation` counts completed renewals. A caller reads it before it waits
/// for the refresh lock and compares afterwards, which is how "refresh unless
/// somebody already did" is decided without a second freshness rule.
#[derive(Debug, Default)]
struct Cache {
    entry: Option<Cached>,
    generation: u64,
}

pub struct TokenSource {
    provider: String,
    issuer: Issuer,
    store: Arc<CredentialStore>,
    http: reqwest::Client,
    cache: Mutex<Cache>,
    refreshing: tokio::sync::Mutex<()>,
}

impl TokenSource {
    pub fn new(
        provider: &str,
        issuer: Issuer,
        store: Arc<CredentialStore>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            provider: provider.to_string(),
            issuer,
            store,
            http,
            cache: Mutex::new(Cache::default()),
            refreshing: tokio::sync::Mutex::new(()),
        }
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Synchronous by contract, so a refusal and a dialog cannot disagree.
    pub fn status(&self) -> Status {
        match self.cached().0 {
            None => Status::SignedOut,
            Some(Cached::Key(_)) => Status::SignedIn { account: None },
            Some(Cached::Tokens(tokens)) => Status::SignedIn {
                account: tokens.account_id,
            },
            Some(Cached::Expired(reason)) => Status::Expired { reason },
        }
    }

    /// The bearer for the next request: the cached token while it is fresh,
    /// a renewed one otherwise.
    pub async fn access_token(&self) -> Result<String, AuthError> {
        let (entry, seen) = self.cached();
        match entry {
            Some(Cached::Tokens(tokens)) if !tokens.is_fresh(unix_now()) => {
                self.refresh(seen).await
            }
            other => Self::usable(other),
        }
    }

    /// What a 401 asks for: renew even though the token still looked fresh.
    pub async fn refreshed(&self) -> Result<String, AuthError> {
        let (_, seen) = self.cached();
        self.refresh(seen).await
    }

    /// Sign in and store the result (ADR-0012 §4). The receipt is the
    /// caller's to compose; what comes back is what it composes from.
    pub async fn login(
        &self,
        prompter: Arc<dyn Prompter>,
        method: LoginMethod,
        open_browser: bool,
    ) -> Result<Tokens, AuthError> {
        match method {
            LoginMethod::Browser => self.login_browser(prompter, open_browser).await,
            LoginMethod::Device => self.login_device(prompter).await,
            LoginMethod::Paste => self.login_paste(prompter).await,
        }
    }

    /// Revocation is best effort: a person who signed out is signed out here
    /// whatever the issuer answers.
    pub async fn logout(&self) -> Result<(), AuthError> {
        if let Some(Cached::Tokens(tokens)) = self.cached().0
            && let Some(refresh) = tokens.refresh
        {
            let _ = exchange::revoke(&self.http, &self.issuer, &refresh).await;
        }
        self.store.remove(&self.provider)?;
        self.forget();
        Ok(())
    }

    /// The browser holds the whole flow; the interaction is only there so a
    /// person can read the URL and give up. It is opened before the callback
    /// is waited on, and carries the URL the browser was handed.
    async fn login_browser(
        &self,
        prompter: Arc<dyn Prompter>,
        open_browser: bool,
    ) -> Result<Tokens, AuthError> {
        let loopback = Loopback::bind().await?;
        let redirect_uri = loopback.redirect_uri();
        let verifier = pkce::verifier()?;
        let state = pkce::state()?;
        let url = self
            .issuer
            .authorize_url(&redirect_uri, &pkce::challenge(&verifier), &state);
        if open_browser {
            browser::open(&url);
        }
        let asked = prompter.ask(
            self.opening(LoginFlow::Browser { url }),
            vec![AnswerSpec::Cancel],
        );
        let flow = async {
            let code = loopback.receive(&state).await?;
            let reply = exchange::authorization_code(
                &self.http,
                &self.issuer,
                &code,
                &redirect_uri,
                &verifier,
            )
            .await?;
            self.persist(&reply)
        };
        tokio::select! {
            biased;
            _ = asked => Err(AuthError::Cancelled),
            tokens = flow => tokens,
        }
    }

    async fn login_device(&self, prompter: Arc<dyn Prompter>) -> Result<Tokens, AuthError> {
        let started = device::start(&self.http, &self.issuer).await?;
        let asked = prompter.ask(
            self.opening(LoginFlow::Device {
                url: started.verify_url.clone(),
                code: started.user_code.clone(),
            }),
            vec![AnswerSpec::Cancel],
        );
        let deadline = Instant::now() + device::MAX_WAIT;
        let flow = async {
            let grant = device::poll(&self.http, &self.issuer, &started, deadline).await?;
            let reply = device::exchange(&self.http, &self.issuer, &grant).await?;
            self.persist(&reply)
        };
        tokio::select! {
            biased;
            _ = asked => Err(AuthError::Cancelled),
            tokens = flow => tokens,
        }
    }

    /// A credential minted elsewhere. It is stored as a key, not a token set:
    /// there is nothing to refresh and pretending otherwise would invent an
    /// expiry nobody knows.
    async fn login_paste(&self, prompter: Arc<dyn Prompter>) -> Result<Tokens, AuthError> {
        let answer = prompter
            .ask(
                self.opening(LoginFlow::Paste),
                vec![AnswerSpec::Text, AnswerSpec::Cancel],
            )
            .await;
        let Ok(Answer::Text { text }) = answer else {
            return Err(AuthError::Cancelled);
        };
        let key = text.trim().to_string();
        if key.is_empty() {
            return Err(AuthError::Invalid("no credential was pasted".into()));
        }
        self.store
            .write(&self.provider, Entry::Api { key: key.clone() })?;
        self.remember(Cached::Key(key.clone()));
        Ok(Tokens {
            access: key,
            ..Tokens::default()
        })
    }

    fn opening(&self, flow: LoginFlow) -> InteractionKind {
        InteractionKind::Login {
            provider: self.provider.clone(),
            flow,
        }
    }

    /// Single-flight. `seen` is the generation the caller decided on: a
    /// different one now means somebody else already renewed.
    async fn refresh(&self, seen: u64) -> Result<String, AuthError> {
        let _guard = self.refreshing.lock().await;
        let (entry, generation) = self.cached();
        if generation != seen {
            return Self::usable(entry);
        }
        let tokens = match entry {
            Some(Cached::Tokens(tokens)) => tokens,
            other => return Self::usable(other),
        };
        let Some(refresh_token) = tokens.refresh.clone() else {
            return Err(self.expire("the stored credential has no refresh token".into()));
        };
        match exchange::refresh(&self.http, &self.issuer, &refresh_token).await {
            Ok(reply) => self.renewed(reply, &tokens),
            Err(AuthError::Expired(reason)) => Err(self.expire(reason)),
            Err(error) => Err(error),
        }
    }

    fn renewed(&self, reply: Value, previous: &Tokens) -> Result<String, AuthError> {
        let tokens = Tokens::from_response(&reply, unix_now()).merged(previous);
        let access = tokens.access.clone();
        self.store.write(&self.provider, tokens.entry())?;
        self.remember(Cached::Tokens(tokens));
        Ok(access)
    }

    fn persist(&self, reply: &Value) -> Result<Tokens, AuthError> {
        let tokens = Tokens::from_response(reply, unix_now());
        if tokens.access.is_empty() {
            return Err(AuthError::Invalid(
                "the token reply carries no access token".into(),
            ));
        }
        self.store.write(&self.provider, tokens.entry())?;
        self.remember(Cached::Tokens(tokens.clone()));
        Ok(tokens)
    }

    /// A credential the issuer has retired is removed, not kept around to
    /// fail again: the way back is a login, and `status()` now says so.
    fn expire(&self, reason: String) -> AuthError {
        if let Err(error) = self.store.remove(&self.provider) {
            tracing::warn!(
                provider = %self.provider,
                %error,
                "the expired credential could not be removed"
            );
        }
        self.remember(Cached::Expired(reason.clone()));
        AuthError::Expired(reason)
    }

    fn usable(entry: Option<Cached>) -> Result<String, AuthError> {
        match entry {
            Some(Cached::Key(key)) => Ok(key),
            Some(Cached::Tokens(tokens)) => Ok(tokens.access),
            Some(Cached::Expired(reason)) => Err(AuthError::Expired(reason)),
            None => Err(AuthError::SignedOut),
        }
    }

    /// An empty cache re-reads the file, so a login by another process — or
    /// by a `bingo login` run while this session was open — is picked up.
    fn cached(&self) -> (Option<Cached>, u64) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if cache.entry.is_none() {
            cache.entry = self.stored();
        }
        (cache.entry.clone(), cache.generation)
    }

    fn stored(&self) -> Option<Cached> {
        match self.store.read(&self.provider) {
            Ok(Some(Entry::Api { key })) => Some(Cached::Key(key)),
            Ok(Some(entry)) => Tokens::from_entry(&entry).map(Cached::Tokens),
            Ok(None) => None,
            // Unreadable is not a credential; it is also not a decision a
            // person can act on mid-turn, so it reads as signed out.
            Err(error) => {
                tracing::warn!(
                    provider = %self.provider,
                    %error,
                    "the credential store could not be read"
                );
                None
            }
        }
    }

    fn remember(&self, entry: Cached) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        cache.entry = Some(entry);
        cache.generation = cache.generation.wrapping_add(1);
    }

    fn forget(&self) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        cache.entry = None;
        cache.generation = cache.generation.wrapping_add(1);
    }
}

/// The provider it belongs to and where it talks; never what it holds.
impl std::fmt::Debug for TokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSource")
            .field("provider", &self.provider)
            .field("issuer", &self.issuer.base)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issuer::tests::issuer;
    use crate::percent;
    use bingo_sdk::KernelError;
    use serde_json::json;
    use std::time::Duration;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, Request, ResponseTemplate};

    const PROVIDER: &str = "codex";

    /// A prompter that records what it was asked and answers as told. `None`
    /// never answers, which is a person who has walked away — the flow wins.
    #[derive(Debug, Default)]
    struct Person {
        seen: Mutex<Vec<InteractionKind>>,
        reply: Option<Answer>,
        after: Duration,
    }

    #[async_trait::async_trait]
    impl Prompter for Person {
        async fn ask(
            &self,
            kind: InteractionKind,
            _answers: Vec<AnswerSpec>,
        ) -> Result<Answer, KernelError> {
            self.seen
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(kind);
            match &self.reply {
                Some(answer) => {
                    tokio::time::sleep(self.after).await;
                    Ok(answer.clone())
                }
                None => std::future::pending().await,
            }
        }
    }

    impl Person {
        fn watching() -> Arc<Person> {
            Arc::new(Person::default())
        }

        fn cancelling(after: Duration) -> Arc<Person> {
            Arc::new(Person {
                reply: Some(Answer::Cancel),
                after,
                ..Person::default()
            })
        }

        fn pasting(text: &str) -> Arc<Person> {
            Arc::new(Person {
                reply: Some(Answer::Text { text: text.into() }),
                ..Person::default()
            })
        }

        fn asked(&self) -> Option<InteractionKind> {
            self.seen
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .first()
                .cloned()
        }
    }

    struct Fixture {
        source: TokenSource,
        store: Arc<CredentialStore>,
        server: MockServer,
        _directory: TempDir,
    }

    async fn fixture() -> Fixture {
        let server = MockServer::start().await;
        let directory = tempfile::tempdir().expect("a temporary directory");
        let store = Arc::new(CredentialStore::new(directory.path().join("data")));
        let source = TokenSource::new(
            PROVIDER,
            issuer(&server.uri()),
            store.clone(),
            reqwest::Client::new(),
        );
        Fixture {
            source,
            store,
            server,
            _directory: directory,
        }
    }

    fn stale(refresh: &str) -> Entry {
        Entry::OAuth {
            access: "at-old".into(),
            refresh: refresh.into(),
            expires: 1,
            account_id: Some("acc_old".into()),
        }
    }

    fn field(query: &str, name: &str) -> Option<String> {
        query
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| *key == name)
            .map(|(_, value)| percent::decode(value))
    }

    fn body_of(request: &Request) -> String {
        String::from_utf8_lossy(&request.body).into_owned()
    }

    async fn requests(server: &MockServer, to: &str) -> Vec<Request> {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|request| request.url.path() == to)
            .collect()
    }

    #[tokio::test]
    async fn an_empty_store_reads_as_signed_out_and_a_login_elsewhere_is_seen() {
        let fixture = fixture().await;
        assert_eq!(fixture.source.status(), Status::SignedOut);
        assert!(matches!(
            fixture.source.access_token().await,
            Err(AuthError::SignedOut)
        ));

        // Another process signs in while this source is already built.
        fixture
            .store
            .write(PROVIDER, Entry::Api { key: "sk-1".into() })
            .expect("a write");
        assert_eq!(fixture.source.status(), Status::SignedIn { account: None });
        assert_eq!(
            fixture.source.access_token().await.expect("the key"),
            "sk-1",
            "a pasted key is the bearer as it is"
        );
    }

    #[tokio::test]
    async fn the_device_flow_polls_until_it_is_granted_and_exchanges_the_code() {
        let fixture = fixture().await;
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/usercode"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_auth_id": "dev-1",
                "user_code": "CODE-1",
                "interval": 1,
            })))
            .expect(1)
            .mount(&fixture.server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/token"))
            .respond_with(ResponseTemplate::new(403))
            .up_to_n_times(2)
            .with_priority(1)
            .mount(&fixture.server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "authorization_code": "ac-device",
                "code_verifier": "ver-device",
            })))
            .with_priority(2)
            .mount(&fixture.server)
            .await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at-new",
                "refresh_token": "rt-new",
                "expires_in": 3600,
                "account_id": "acc_1",
            })))
            .expect(1)
            .mount(&fixture.server)
            .await;

        let person = Person::watching();
        let tokens = fixture
            .source
            .login(person.clone(), LoginMethod::Device, false)
            .await
            .expect("a signed-in token set");

        assert_eq!(tokens.access, "at-new");
        assert_eq!(
            person.asked(),
            Some(InteractionKind::Login {
                provider: PROVIDER.into(),
                flow: LoginFlow::Device {
                    url: format!("{}/codex/device", fixture.server.uri()),
                    code: "CODE-1".into(),
                },
            })
        );
        assert_eq!(
            requests(&fixture.server, "/api/accounts/deviceauth/token")
                .await
                .len(),
            3,
            "two pending polls and the grant"
        );

        let exchange = requests(&fixture.server, "/oauth/token").await;
        let body = body_of(&exchange[0]);
        assert_eq!(
            field(&body, "grant_type").as_deref(),
            Some("authorization_code")
        );
        assert_eq!(field(&body, "code").as_deref(), Some("ac-device"));
        assert_eq!(field(&body, "code_verifier").as_deref(), Some("ver-device"));
        assert_eq!(field(&body, "client_id").as_deref(), Some("app_TEST"));
        assert_eq!(
            field(&body, "redirect_uri").as_deref(),
            Some(format!("{}/deviceauth/callback", fixture.server.uri()).as_str())
        );
        assert_eq!(
            fixture.store.read(PROVIDER).expect("a read"),
            Some(Entry::OAuth {
                access: "at-new".into(),
                refresh: "rt-new".into(),
                expires: tokens.expires_at.expect("an expiry"),
                account_id: Some("acc_1".into()),
            })
        );
    }

    #[tokio::test]
    async fn a_cancelled_device_login_stops_polling() {
        let fixture = fixture().await;
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/usercode"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "device_auth_id": "dev-1",
                "user_code": "CODE-1",
                "interval": 60,
            })))
            .mount(&fixture.server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/token"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&fixture.server)
            .await;

        let outcome = fixture
            .source
            .login(
                Person::cancelling(Duration::from_millis(50)),
                LoginMethod::Device,
                false,
            )
            .await;
        assert!(matches!(outcome, Err(AuthError::Cancelled)), "{outcome:?}");

        let polled = requests(&fixture.server, "/api/accounts/deviceauth/token")
            .await
            .len();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            requests(&fixture.server, "/api/accounts/deviceauth/token")
                .await
                .len(),
            polled,
            "the poll loop was dropped with the flow"
        );
        assert_eq!(fixture.source.status(), Status::SignedOut);
    }

    #[tokio::test]
    async fn the_browser_flow_checks_the_state_and_exchanges_with_its_verifier() {
        let fixture = fixture().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at-browser",
                "refresh_token": "rt-browser",
                "expires_in": 3600,
                "account_id": "acc_2",
            })))
            .expect(1)
            .mount(&fixture.server)
            .await;

        let person = Person::watching();
        let browser = tokio::spawn(redirect_like_a_browser(person.clone()));
        let tokens = fixture
            .source
            .login(person, LoginMethod::Browser, false)
            .await
            .expect("a signed-in token set");
        let challenge = browser.await.expect("the browser task");

        assert_eq!(tokens.access, "at-browser");
        let body = body_of(&requests(&fixture.server, "/oauth/token").await[0]);
        let verifier = field(&body, "code_verifier").expect("a verifier");
        assert_eq!(
            pkce::challenge(&verifier),
            challenge,
            "the exchange carries the verifier the authorize URL committed to"
        );
        assert_eq!(field(&body, "code").as_deref(), Some("ac-browser"));
        assert_eq!(
            fixture.source.status(),
            Status::SignedIn {
                account: Some("acc_2".into())
            }
        );
    }

    /// Waits for the interaction to open, reads the URL it carries, and hits
    /// the loopback callback the way a browser would. Returns the challenge
    /// the authorize URL committed to.
    async fn redirect_like_a_browser(person: Arc<Person>) -> String {
        let url = loop {
            match person.asked() {
                Some(InteractionKind::Login {
                    flow: LoginFlow::Browser { url },
                    ..
                }) => break url,
                _ => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        };
        let (_, query) = url.split_once('?').expect("a query");
        let redirect_uri = field(query, "redirect_uri").expect("a redirect uri");
        let state = field(query, "state").expect("a state");
        let response = reqwest::get(format!("{redirect_uri}?code=ac-browser&state={state}"))
            .await
            .expect("the callback answers");
        assert_eq!(response.status().as_u16(), 200);
        field(query, "code_challenge").expect("a challenge")
    }

    #[tokio::test]
    async fn a_pasted_credential_is_stored_as_a_key_and_never_refreshed() {
        let fixture = fixture().await;
        let tokens = fixture
            .source
            .login(Person::pasting("  sk-pasted  "), LoginMethod::Paste, false)
            .await
            .expect("a stored key");
        assert_eq!(tokens.access, "sk-pasted", "the text is trimmed");
        assert_eq!(
            fixture.store.read(PROVIDER).expect("a read"),
            Some(Entry::Api {
                key: "sk-pasted".into()
            })
        );
        // No mock is mounted: a refresh here would fail the test by 404.
        assert_eq!(
            fixture.source.access_token().await.expect("the key"),
            "sk-pasted"
        );
    }

    #[tokio::test]
    async fn an_empty_paste_is_refused_and_a_cancelled_one_is_cancelled() {
        let fixture = fixture().await;
        assert!(matches!(
            fixture
                .source
                .login(Person::pasting("   "), LoginMethod::Paste, false)
                .await,
            Err(AuthError::Invalid(_))
        ));
        assert!(matches!(
            fixture
                .source
                .login(
                    Person::cancelling(Duration::ZERO),
                    LoginMethod::Paste,
                    false
                )
                .await,
            Err(AuthError::Cancelled)
        ));
        assert_eq!(fixture.store.read(PROVIDER).expect("a read"), None);
    }

    #[tokio::test]
    async fn a_refresh_that_omits_the_refresh_token_keeps_the_stored_one() {
        let fixture = fixture().await;
        fixture
            .store
            .write(PROVIDER, stale("rt-old"))
            .expect("a write");
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at-new",
                "expires_in": 3600,
            })))
            .expect(1)
            .mount(&fixture.server)
            .await;

        assert_eq!(
            fixture.source.access_token().await.expect("a token"),
            "at-new"
        );
        let body = body_of(&requests(&fixture.server, "/oauth/token").await[0]);
        assert_eq!(
            serde_json::from_str::<Value>(&body).expect("a json body")["refresh_token"],
            json!("rt-old"),
            "the refresh grant is sent as json"
        );
        match fixture.store.read(PROVIDER).expect("a read") {
            Some(Entry::OAuth {
                access,
                refresh,
                account_id,
                ..
            }) => {
                assert_eq!(access, "at-new");
                assert_eq!(refresh, "rt-old", "the omitted token is the old one");
                assert_eq!(account_id.as_deref(), Some("acc_old"));
            }
            other => panic!("expected an oauth entry, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_retired_refresh_token_clears_the_entry_and_reads_as_expired() {
        let fixture = fixture().await;
        fixture
            .store
            .write(PROVIDER, stale("rt-dead"))
            .expect("a write");
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(json!({ "error": "refresh_token_expired" })),
            )
            .expect(1)
            .mount(&fixture.server)
            .await;

        assert!(matches!(
            fixture.source.access_token().await,
            Err(AuthError::Expired(_))
        ));
        assert_eq!(fixture.store.read(PROVIDER).expect("a read"), None);
        assert!(matches!(fixture.source.status(), Status::Expired { .. }));
        // The second call answers from the cache rather than the issuer, so
        // `expect(1)` above still holds.
        assert!(matches!(
            fixture.source.access_token().await,
            Err(AuthError::Expired(_))
        ));
    }

    #[tokio::test]
    async fn a_credential_with_no_refresh_token_expires_rather_than_being_sent() {
        let fixture = fixture().await;
        fixture.store.write(PROVIDER, stale("")).expect("a write");
        assert!(matches!(
            fixture.source.access_token().await,
            Err(AuthError::Expired(_))
        ));
        assert_eq!(fixture.store.read(PROVIDER).expect("a read"), None);
    }

    #[tokio::test]
    async fn eight_concurrent_callers_make_one_refresh() {
        let fixture = fixture().await;
        fixture
            .store
            .write(PROVIDER, stale("rt-old"))
            .expect("a write");
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(50))
                    .set_body_json(json!({ "access_token": "at-new", "expires_in": 3600 })),
            )
            .expect(1)
            .mount(&fixture.server)
            .await;

        let source = Arc::new(fixture.source);
        let callers: Vec<_> = (0..8)
            .map(|_| {
                let source = source.clone();
                tokio::spawn(async move { source.access_token().await })
            })
            .collect();
        for caller in callers {
            assert_eq!(
                caller.await.expect("a caller").expect("a token"),
                "at-new",
                "every caller sees the renewed token"
            );
        }
        assert_eq!(requests(&fixture.server, "/oauth/token").await.len(), 1);
    }

    #[tokio::test]
    async fn a_forced_refresh_renews_a_token_that_still_looked_fresh() {
        let fixture = fixture().await;
        fixture
            .store
            .write(
                PROVIDER,
                Entry::OAuth {
                    access: "at-old".into(),
                    refresh: "rt-old".into(),
                    expires: unix_now() + 3600,
                    account_id: None,
                },
            )
            .expect("a write");
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "access_token": "at-forced", "expires_in": 3600 })),
            )
            .expect(1)
            .mount(&fixture.server)
            .await;

        assert_eq!(
            fixture.source.access_token().await.expect("a token"),
            "at-old",
            "a fresh token is used without asking the issuer"
        );
        assert_eq!(
            fixture.source.refreshed().await.expect("a token"),
            "at-forced"
        );
    }

    #[tokio::test]
    async fn signing_out_revokes_best_effort_and_empties_the_entry() {
        let fixture = fixture().await;
        fixture
            .store
            .write(PROVIDER, stale("rt-old"))
            .expect("a write");
        assert!(matches!(fixture.source.status(), Status::SignedIn { .. }));
        Mock::given(method("POST"))
            .and(path("/oauth/revoke"))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&fixture.server)
            .await;

        fixture.source.logout().await.expect("a sign-out");
        let body = body_of(&requests(&fixture.server, "/oauth/revoke").await[0]);
        assert_eq!(
            serde_json::from_str::<Value>(&body).expect("a json body")["token"],
            json!("rt-old")
        );
        assert_eq!(fixture.store.read(PROVIDER).expect("a read"), None);
        assert_eq!(
            fixture.source.status(),
            Status::SignedOut,
            "a failed revocation still signs out locally"
        );
    }
}

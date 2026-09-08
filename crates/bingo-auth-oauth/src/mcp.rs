//! One MCP server's sign-in over time (ADR-0050 §§1–2).
//!
//! The [`TokenSource`](crate::TokenSource) shape — who am I, give me a
//! bearer, forget me — for a resource server rather than a provider. The
//! difference is what has to be found out before any of it can happen: the
//! authorization server, and a client id to be known by. Both are found once
//! and written into the one entry, because the registration and the tokens
//! are one fact about one server: a client bingo registered is usable only
//! against the issuer it was registered with, through the redirect it named.
//!
//! Nothing here logs a credential, and nothing here writes one anywhere but
//! `auth.json` (mode 0600).

use std::sync::{Arc, Mutex};

use bingo_sdk::{Answer, AnswerSpec, InteractionKind, LoginFlow, LoginMethod, Prompter};
use reqwest::header::HeaderMap;

use crate::callback::Callback;
use crate::challenge::{self, Challenge, Probe};
use crate::discover::{self, Discovered};
use crate::error::AuthError;
use crate::issuer::Issuer;
use crate::pkce;
use crate::redirect;
use crate::register;
use crate::source::Status;
use crate::store::{CredentialStore, Entry, mcp_key};
use crate::tokens::{Tokens, unix_now};

/// What bingo is to one authorization server. Half of the stored entry; the
/// tokens are the other half.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Client {
    issuer: String,
    id: String,
    secret: Option<String>,
    redirect_uri: String,
}

/// One configured MCP server's credential.
pub struct McpAuth {
    /// The configured name; the store key is `mcp:<server>`.
    server: String,
    url: String,
    /// The headers a person configured, sent on the probe as the dial sends
    /// them: a server behind a gateway may need one to answer at all.
    headers: HeaderMap,
    /// `mcpServers.<name>.oauth.clientId` — an authorization server with no
    /// dynamic registration, whose client id a person was given by hand.
    configured_client_id: Option<String>,
    store: Arc<CredentialStore>,
    http: reqwest::Client,
    /// The issuer's endpoints, resolved once per process from the name the
    /// entry keeps, so a refresh does not re-walk the well-known ladder.
    endpoints: tokio::sync::Mutex<Option<Issuer>>,
    /// What the issuer last said about a credential it retired. The entry is
    /// gone from the file by then — a dead refresh token is not worth
    /// keeping — so this is what lets `/mcp` say *expired* rather than
    /// *needs authentication* for the rest of the run.
    retired: Mutex<Option<String>>,
    refreshing: tokio::sync::Mutex<()>,
}

/// The name and the endpoint, never a header value and never a token.
impl std::fmt::Debug for McpAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpAuth")
            .field("server", &self.server)
            .field("url", &self.url)
            .finish_non_exhaustive()
    }
}

impl McpAuth {
    pub fn new(
        server: &str,
        url: &str,
        headers: HeaderMap,
        configured_client_id: Option<String>,
        store: Arc<CredentialStore>,
        http: reqwest::Client,
    ) -> Self {
        Self {
            server: server.to_string(),
            url: url.to_string(),
            headers,
            configured_client_id,
            store,
            http,
            endpoints: tokio::sync::Mutex::new(None),
            retired: Mutex::new(None),
            refreshing: tokio::sync::Mutex::new(()),
        }
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    /// Synchronous by contract, as a provider's is: a table and a dial must
    /// not disagree about whether this server is signed in to.
    pub fn status(&self) -> Status {
        if let Some(reason) = self.retired_reason() {
            return Status::Expired { reason };
        }
        match self.stored() {
            Some(_) => Status::SignedIn { account: None },
            None => Status::SignedOut,
        }
    }

    /// The bearer for the next dial: the stored token while it is fresh, a
    /// renewed one otherwise.
    pub async fn access_token(&self) -> Result<String, AuthError> {
        let tokens = self.tokens()?;
        match tokens.is_fresh(unix_now()) {
            true => Ok(tokens.access),
            false => self.refreshed(&tokens.access).await,
        }
    }

    /// What a `401` asks for: renew the token that bounced, even though its
    /// expiry said it was fine — an issuer may retire one early, and the
    /// server's refusal is the fact, not the clock.
    ///
    /// `stale` is the bearer that was refused. It is also what makes this
    /// single flight without a second rule: a caller that waited for the lock
    /// finds a different token stored and takes it, rather than renewing the
    /// renewal.
    pub async fn refreshed(&self, stale: &str) -> Result<String, AuthError> {
        let _guard = self.refreshing.lock().await;
        let tokens = self.tokens()?;
        if tokens.access != stale {
            return Ok(tokens.access);
        }
        self.renew(tokens).await
    }

    /// Sign in, and answer with the line a person reads. Re-authenticating is
    /// the same call: what it writes replaces what was there.
    pub async fn login(
        &self,
        prompter: Arc<dyn Prompter>,
        method: Option<LoginMethod>,
        open_browser: bool,
    ) -> Result<String, AuthError> {
        let paste = match method {
            Some(LoginMethod::Device) => {
                return Err(AuthError::Invalid(format!(
                    "{} has no device flow; sign in with a browser, or paste the redirect",
                    self.server
                )));
            }
            Some(LoginMethod::Paste) => true,
            _ => false,
        };
        let found = self.discover().await?;
        let loopback = redirect::bind_named(self.registered_port(&found)).await?;
        let client = self.client_for(&found, loopback.port()).await?;
        let issuer = issuer_for(&found, &client);
        let tokens = self
            .authorize(prompter, &issuer, loopback, paste, open_browser && !paste)
            .await?;
        self.store.write(&self.key(), entry_of(&client, &tokens))?;
        self.forget_retirement();
        Ok(format!("Signed in to {}.", self.server))
    }

    /// Revoke where the issuer offers it, then forget. Best effort by
    /// contract: a person who signed out is signed out here whatever the
    /// server answers.
    pub async fn logout(&self) -> Result<String, AuthError> {
        if let Some(refresh) = self.tokens().ok().and_then(|tokens| tokens.refresh)
            && let Ok(issuer) = self.issuer().await
        {
            let _ = crate::exchange::revoke(&self.http, &issuer, &refresh).await;
        }
        self.store.remove(&self.key())?;
        self.forget_retirement();
        Ok(format!("Signed out of {}.", self.server))
    }

    /// Ask the server itself what it wants, then follow it to its issuer. A
    /// server that answers without a token still gets discovered — the
    /// well-known ladder is where a `login` typed ahead of any dial starts.
    async fn discover(&self) -> Result<Discovered, AuthError> {
        let challenge = match challenge::probe(&self.http, &self.url, self.headers.clone()).await {
            Ok(Probe::Unauthorized(challenge)) => challenge,
            // Unreachable, or content with what it was given: either way the
            // ladder is the only thing left to try, and it says what it tried.
            Ok(Probe::Ok) | Err(_) => Challenge::default(),
        };
        discover::discover(&self.http, &self.url, &challenge).await
    }

    /// Who to sign in as: the client id a person configured, the one a
    /// previous login registered with this same issuer and port, or a fresh
    /// registration.
    async fn client_for(&self, found: &Discovered, port: u16) -> Result<Client, AuthError> {
        let redirect_uri = redirect::uri(port);
        let issuer = found.issuer.base.clone();
        if let Some(id) = &self.configured_client_id {
            return Ok(Client {
                issuer,
                id: id.clone(),
                secret: None,
                redirect_uri,
            });
        }
        if let Some(client) = self.registered().filter(|client| {
            client.issuer == issuer && redirect::port_of(&client.redirect_uri) == Some(port)
        }) {
            return Ok(client);
        }
        let endpoint = found.registration_endpoint.as_deref().ok_or_else(|| {
            AuthError::Invalid(format!(
                "{issuer} registers no clients of its own; put its client id in \
                 mcpServers.{}.oauth.clientId",
                self.server
            ))
        })?;
        let registered = register::register(&self.http, endpoint, &redirect::uris(port)).await?;
        Ok(Client {
            issuer,
            id: registered.client_id,
            secret: registered.client_secret,
            redirect_uri,
        })
    }

    /// The port a registration this issuer already knows was made on, so the
    /// exact redirect URI it recorded can be offered again (R-port).
    fn registered_port(&self, found: &Discovered) -> Option<u16> {
        self.registered()
            .filter(|client| client.issuer == found.issuer.base)
            .and_then(|client| redirect::port_of(&client.redirect_uri))
    }

    /// Send the person to the authorization server and redeem what comes
    /// back, whether it arrives on the loopback or in a person's own hands.
    async fn authorize(
        &self,
        prompter: Arc<dyn Prompter>,
        issuer: &Issuer,
        loopback: bingo_loopback::Loopback,
        paste: bool,
        open_browser: bool,
    ) -> Result<Tokens, AuthError> {
        let redirect_uri = redirect::uri(loopback.port());
        let verifier = pkce::verifier()?;
        let state = pkce::state()?;
        let url = issuer.authorize_url(&redirect_uri, &pkce::challenge(&verifier), &state);
        if open_browser {
            bingo_loopback::browser::open(&url);
        }
        let callback = self.code(prompter, url, loopback, &state, paste).await?;
        check(&callback, &state, &issuer.base)?;
        let reply = crate::exchange::authorization_code(
            &self.http,
            issuer,
            &callback.code,
            &redirect_uri,
            &verifier,
        )
        .await?;
        let tokens = Tokens::from_response(&reply, unix_now());
        match tokens.access.is_empty() {
            true => Err(AuthError::Invalid(
                "the token reply carries no access token".into(),
            )),
            false => Ok(tokens),
        }
    }

    /// The redirect, from whichever of the two arrives first: the browser's,
    /// or the one a person pasted into the words row.
    async fn code(
        &self,
        prompter: Arc<dyn Prompter>,
        url: String,
        loopback: bingo_loopback::Loopback,
        state: &str,
        paste: bool,
    ) -> Result<Callback, AuthError> {
        let answers = match paste {
            true => vec![AnswerSpec::Text, AnswerSpec::Cancel],
            false => vec![AnswerSpec::Cancel],
        };
        let asked = prompter.ask(
            InteractionKind::Login {
                provider: self.server.clone(),
                flow: LoginFlow::Browser { url },
            },
            answers,
        );
        tokio::select! {
            biased;
            answered = asked => pasted(answered),
            redirected = redirect::receive(loopback, state) => redirected,
        }
    }

    /// The exchange itself, under the refresh lock. The store is the one
    /// fact: what this writes is what every later reader sees.
    async fn renew(&self, tokens: Tokens) -> Result<String, AuthError> {
        let Some(refresh) = tokens.refresh.clone() else {
            return Err(self.retire("the stored token expired and nothing renews it".into()));
        };
        let issuer = self.issuer().await?;
        let reply = match crate::exchange::refresh(&self.http, &issuer, &refresh).await {
            Ok(reply) => reply,
            Err(AuthError::Expired(reason)) => return Err(self.retire(reason)),
            Err(error) => return Err(error),
        };
        let renewed = Tokens::from_response(&reply, unix_now()).merged(&tokens);
        let client = self.registered().ok_or(AuthError::SignedOut)?;
        self.store.write(&self.key(), entry_of(&client, &renewed))?;
        Ok(renewed.access)
    }

    /// The endpoints of the issuer the entry names, walked once per process.
    async fn issuer(&self) -> Result<Issuer, AuthError> {
        let client = self.registered().ok_or(AuthError::SignedOut)?;
        let mut cached = self.endpoints.lock().await;
        if let Some(issuer) = cached.as_ref().filter(|it| it.base == client.issuer) {
            return Ok(with_client(issuer.clone(), &client));
        }
        let resource = self.url.split('#').next().unwrap_or(&self.url);
        let found = discover::endpoints(&self.http, &client.issuer, resource.to_string()).await?;
        *cached = Some(found.clone());
        Ok(with_client(found, &client))
    }

    /// A credential the issuer has retired is removed rather than kept to
    /// fail again: the way back is a login, and `status()` now says so.
    fn retire(&self, reason: String) -> AuthError {
        if let Err(error) = self.store.remove(&self.key()) {
            tracing::warn!(server = %self.server, %error, "the expired credential could not be removed");
        }
        if let Ok(mut retired) = self.retired.lock() {
            *retired = Some(reason.clone());
        }
        AuthError::Expired(reason)
    }

    fn forget_retirement(&self) {
        if let Ok(mut retired) = self.retired.lock() {
            *retired = None;
        }
    }

    fn retired_reason(&self) -> Option<String> {
        self.retired.lock().ok().and_then(|retired| retired.clone())
    }

    fn tokens(&self) -> Result<Tokens, AuthError> {
        self.stored()
            .as_ref()
            .and_then(Tokens::from_entry)
            .ok_or(AuthError::SignedOut)
    }

    fn registered(&self) -> Option<Client> {
        match self.stored()? {
            Entry::McpOAuth {
                issuer,
                client_id,
                client_secret,
                redirect_uri,
                ..
            } => Some(Client {
                issuer,
                id: client_id,
                secret: client_secret,
                redirect_uri,
            }),
            _ => None,
        }
    }

    /// An unreadable store is not a credential; it is also not a decision a
    /// person can act on mid-dial, so it reads as signed out.
    fn stored(&self) -> Option<Entry> {
        match self.store.read(&self.key()) {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(server = %self.server, %error, "the credential store could not be read");
                None
            }
        }
    }

    fn key(&self) -> String {
        mcp_key(&self.server)
    }
}

/// The issuer as this login will use it: discovered endpoints, the client we
/// are, and the scope the resource asked for.
fn issuer_for(found: &Discovered, client: &Client) -> Issuer {
    Issuer {
        client_id: client.id.clone(),
        scope: found.scopes_supported.clone().unwrap_or_default(),
        ..found.issuer.clone()
    }
}

fn with_client(issuer: Issuer, client: &Client) -> Issuer {
    Issuer {
        client_id: client.id.clone(),
        ..issuer
    }
}

fn entry_of(client: &Client, tokens: &Tokens) -> Entry {
    Entry::McpOAuth {
        issuer: client.issuer.clone(),
        client_id: client.id.clone(),
        client_secret: client.secret.clone(),
        redirect_uri: client.redirect_uri.clone(),
        access: tokens.access.clone(),
        refresh: tokens.refresh.clone(),
        expires: tokens.expires_at.unwrap_or_default(),
        scope: None,
    }
}

fn pasted(answered: Result<Answer, bingo_sdk::KernelError>) -> Result<Callback, AuthError> {
    match answered {
        Ok(Answer::Text { text }) => crate::callback::pasted(&text),
        Ok(_) => Err(AuthError::Cancelled),
        Err(refused) => Err(AuthError::Invalid(refused.message)),
    }
}

/// What the redirect must prove before its code is redeemed: the nonce this
/// flow minted (RFC 6749 §10.12) and, when the server named itself, that it
/// is the server the flow was started against (RFC 9207).
fn check(callback: &Callback, state: &str, issuer: &str) -> Result<(), AuthError> {
    if !callback.state.is_empty() && callback.state != state {
        return Err(AuthError::Invalid(
            "the callback state does not match".into(),
        ));
    }
    match &callback.iss {
        Some(named) if named.trim_end_matches('/') != issuer.trim_end_matches('/') => Err(
            AuthError::Invalid(format!("the code was minted by {named}, not by {issuer}")),
        ),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::CredentialStore;
    use serde_json::json;
    use tempfile::TempDir;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SERVER: &str = "binlesson";

    /// A person who never answers: the flow is what completes, as it does
    /// when a browser finishes the sign-in on its own.
    #[derive(Debug)]
    struct Watching;

    #[async_trait::async_trait]
    impl Prompter for Watching {
        async fn ask(
            &self,
            _kind: InteractionKind,
            _answers: Vec<AnswerSpec>,
        ) -> Result<Answer, bingo_sdk::KernelError> {
            std::future::pending().await
        }
    }

    /// A person who pastes the redirect they were sent to, once the URL has
    /// reached them: the `--paste` path, with no socket in it.
    #[derive(Debug)]
    struct Pasting(Mutex<Option<String>>);

    #[async_trait::async_trait]
    impl Prompter for Pasting {
        async fn ask(
            &self,
            kind: InteractionKind,
            _answers: Vec<AnswerSpec>,
        ) -> Result<Answer, bingo_sdk::KernelError> {
            let InteractionKind::Login {
                flow: LoginFlow::Browser { url },
                ..
            } = kind
            else {
                panic!("an mcp login is a browser flow");
            };
            let state = url
                .rsplit_once("state=")
                .map(|(_, state)| state.to_string())
                .unwrap_or_default();
            let pasted = self
                .0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
                .unwrap_or_else(|| {
                    format!("http://localhost:1/auth/callback?code=ac-1&state={state}")
                });
            Ok(Answer::Text { text: pasted })
        }
    }

    fn store(home: &TempDir) -> Arc<CredentialStore> {
        Arc::new(CredentialStore::new(home.path().join("data")))
    }

    fn auth(server: &MockServer, home: &TempDir) -> McpAuth {
        McpAuth::new(
            SERVER,
            &format!("{}/mcp", server.uri()),
            HeaderMap::new(),
            None,
            store(home),
            reqwest::Client::new(),
        )
    }

    /// A whole authorization server: the challenge, both ladders, the
    /// registration, the token endpoint and the revocation.
    async fn authorization_server(server: &MockServer) {
        let uri = server.uri();
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                format!(
                    r#"Bearer resource_metadata="{uri}/.well-known/oauth-protected-resource/mcp", scope="mcp:tools""#
                )
                .as_str(),
            ))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "resource": format!("{uri}/mcp"),
                "authorization_servers": [uri],
            })))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "issuer": uri,
                "authorization_endpoint": format!("{uri}/authorize"),
                "token_endpoint": format!("{uri}/token"),
                "registration_endpoint": format!("{uri}/register"),
                "revocation_endpoint": format!("{uri}/revoke"),
                "code_challenge_methods_supported": ["S256"],
            })))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/register"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "client_id": "cl_1" })))
            .mount(server)
            .await;
        Mock::given(method("POST"))
            .and(path("/revoke"))
            .respond_with(ResponseTemplate::new(200))
            .mount(server)
            .await;
    }

    async fn token_endpoint(server: &MockServer, access: &str, refresh: &str) {
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": access,
                "refresh_token": refresh,
                "token_type": "Bearer",
                "expires_in": 3600,
            })))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn a_pasted_redirect_registers_signs_in_and_stores_the_one_entry() {
        let server = MockServer::start().await;
        authorization_server(&server).await;
        token_endpoint(&server, "at_1", "rt_1").await;
        let home = tempfile::tempdir().expect("a home");
        let auth = auth(&server, &home);
        assert_eq!(auth.status(), Status::SignedOut);

        let receipt = auth
            .login(
                Arc::new(Pasting(Mutex::new(None))),
                Some(LoginMethod::Paste),
                false,
            )
            .await
            .expect("a sign-in");
        assert_eq!(receipt, "Signed in to binlesson.");
        assert_eq!(auth.status(), Status::SignedIn { account: None });
        assert_eq!(auth.access_token().await.expect("a bearer"), "at_1");

        let Some(Entry::McpOAuth {
            issuer,
            client_id,
            refresh,
            redirect_uri,
            ..
        }) = auth.stored()
        else {
            panic!("one mcpOauth entry under mcp:binlesson");
        };
        assert_eq!(issuer, server.uri());
        assert_eq!(client_id, "cl_1");
        assert_eq!(refresh.as_deref(), Some("rt_1"));
        assert!(
            redirect_uri.starts_with("http://localhost:"),
            "{redirect_uri}"
        );
    }

    /// RFC 8707 and RFC 7636: the audience and the verifier reach the token
    /// endpoint, which is what a resource server checks the bearer against.
    #[tokio::test]
    async fn the_token_request_carries_the_resource_and_the_verifier() {
        let server = MockServer::start().await;
        authorization_server(&server).await;
        let uri = server.uri();
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier="))
            .and(body_string_contains(
                percent_encoded(&format!("{uri}/mcp")).as_str(),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "access_token": "at_1", "expires_in": 60 })),
            )
            .mount(&server)
            .await;
        let home = tempfile::tempdir().expect("a home");
        auth(&server, &home)
            .login(
                Arc::new(Pasting(Mutex::new(None))),
                Some(LoginMethod::Paste),
                false,
            )
            .await
            .expect("a sign-in");
    }

    fn percent_encoded(value: &str) -> String {
        format!("resource={}", crate::percent::encode(value))
    }

    #[tokio::test]
    async fn a_stale_token_is_renewed_once_however_many_callers_ask() {
        let server = MockServer::start().await;
        authorization_server(&server).await;
        token_endpoint(&server, "at_1", "rt_1").await;
        let home = tempfile::tempdir().expect("a home");
        let auth = Arc::new(auth(&server, &home));
        auth.login(
            Arc::new(Pasting(Mutex::new(None))),
            Some(LoginMethod::Paste),
            false,
        )
        .await
        .expect("a sign-in");

        // Age the stored token past the refresh lead, as a day would.
        let Some(Entry::McpOAuth {
            issuer,
            client_id,
            redirect_uri,
            ..
        }) = auth.stored()
        else {
            panic!("an entry");
        };
        auth.store
            .write(
                &auth.key(),
                Entry::McpOAuth {
                    issuer,
                    client_id,
                    client_secret: None,
                    redirect_uri,
                    access: "at_stale".into(),
                    refresh: Some("rt_1".into()),
                    expires: unix_now() - 10,
                    scope: None,
                },
            )
            .expect("a write");

        let mut asking = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let auth = Arc::clone(&auth);
            asking.spawn(async move { auth.access_token().await });
        }
        while let Some(answered) = asking.join_next().await {
            assert_eq!(
                answered.expect("the task").expect("a bearer"),
                "at_1",
                "every caller reads the one renewed token"
            );
        }
        let exchanges = server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|request| request.url.path() == "/token")
            .count();
        assert_eq!(exchanges, 2, "one for the sign-in, one for the renewal");
    }

    #[tokio::test]
    async fn a_refresh_token_the_issuer_retired_leaves_the_entry_gone_and_expired() {
        let server = MockServer::start().await;
        authorization_server(&server).await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": "at_1", "refresh_token": "rt_1", "expires_in": -1,
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(
                ResponseTemplate::new(400).set_body_string(r#"{"error":"refresh_token_expired"}"#),
            )
            .mount(&server)
            .await;
        let home = tempfile::tempdir().expect("a home");
        let auth = auth(&server, &home);
        auth.login(
            Arc::new(Pasting(Mutex::new(None))),
            Some(LoginMethod::Paste),
            false,
        )
        .await
        .expect("a sign-in");
        assert!(matches!(
            auth.access_token().await,
            Err(AuthError::Expired(_))
        ));
        assert!(auth.stored().is_none(), "a dead credential is not kept");
        assert!(matches!(auth.status(), Status::Expired { .. }));
    }

    #[tokio::test]
    async fn signing_out_revokes_and_forgets() {
        let server = MockServer::start().await;
        authorization_server(&server).await;
        token_endpoint(&server, "at_1", "rt_1").await;
        let home = tempfile::tempdir().expect("a home");
        let auth = auth(&server, &home);
        auth.login(
            Arc::new(Pasting(Mutex::new(None))),
            Some(LoginMethod::Paste),
            false,
        )
        .await
        .expect("a sign-in");
        assert_eq!(
            auth.logout().await.expect("a sign-out"),
            "Signed out of binlesson."
        );
        assert_eq!(auth.status(), Status::SignedOut);
        let revocations = server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|request| request.url.path() == "/revoke")
            .count();
        assert_eq!(revocations, 1);
        auth.logout().await.expect("signing out twice is a no-op");
    }

    #[tokio::test]
    async fn a_forged_state_or_a_foreign_issuer_is_refused_before_the_code_is_redeemed() {
        let server = MockServer::start().await;
        authorization_server(&server).await;
        token_endpoint(&server, "at_1", "rt_1").await;
        for pasted in [
            "http://localhost:1/auth/callback?code=ac-1&state=forged".to_string(),
            "http://localhost:1/auth/callback?code=ac-1&iss=https%3A%2F%2Fimpostor.example".into(),
        ] {
            let home = tempfile::tempdir().expect("a home");
            let refused = auth(&server, &home)
                .login(
                    Arc::new(Pasting(Mutex::new(Some(pasted.clone())))),
                    Some(LoginMethod::Paste),
                    false,
                )
                .await
                .expect_err("refused");
            assert!(matches!(refused, AuthError::Invalid(_)), "{pasted}");
        }
    }

    #[tokio::test]
    async fn a_configured_client_id_is_used_and_nothing_is_registered() {
        let server = MockServer::start().await;
        authorization_server(&server).await;
        token_endpoint(&server, "at_1", "rt_1").await;
        let home = tempfile::tempdir().expect("a home");
        let auth = McpAuth::new(
            SERVER,
            &format!("{}/mcp", server.uri()),
            HeaderMap::new(),
            Some("cl_by_hand".into()),
            store(&home),
            reqwest::Client::new(),
        );
        auth.login(
            Arc::new(Pasting(Mutex::new(None))),
            Some(LoginMethod::Paste),
            false,
        )
        .await
        .expect("a sign-in");
        assert_eq!(auth.registered().expect("a client").id, "cl_by_hand");
        let registrations = server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|request| request.url.path() == "/register")
            .count();
        assert_eq!(registrations, 0);
    }

    #[tokio::test]
    async fn an_mcp_server_has_no_device_flow_and_says_so() {
        let server = MockServer::start().await;
        let home = tempfile::tempdir().expect("a home");
        let refused = auth(&server, &home)
            .login(Arc::new(Watching), Some(LoginMethod::Device), false)
            .await
            .expect_err("no device flow");
        assert!(refused.to_string().contains("device flow"), "{refused}");
    }

    #[tokio::test]
    async fn a_server_that_registers_nobody_and_was_given_no_client_id_says_which_key() {
        let server = MockServer::start().await;
        let uri = server.uri();
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-protected-resource/mcp"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "authorization_servers": [uri.clone()] })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/.well-known/oauth-authorization-server"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "issuer": uri,
                "authorization_endpoint": format!("{uri}/authorize"),
                "token_endpoint": format!("{uri}/token"),
                "code_challenge_methods_supported": ["S256"],
            })))
            .mount(&server)
            .await;
        let home = tempfile::tempdir().expect("a home");
        let refused = auth(&server, &home)
            .login(Arc::new(Watching), None, false)
            .await
            .expect_err("nowhere to register");
        assert!(
            refused
                .to_string()
                .contains("mcpServers.binlesson.oauth.clientId"),
            "{refused}"
        );
    }

    #[test]
    fn nothing_of_the_credential_reaches_a_debug_line() {
        let home = tempfile::tempdir().expect("a home");
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::AUTHORIZATION,
            reqwest::header::HeaderValue::from_static("Bearer ghp_live"),
        );
        let auth = McpAuth::new(
            SERVER,
            "https://mcp.example.com/mcp",
            headers,
            Some("cl_secret_looking".into()),
            store(&home),
            reqwest::Client::new(),
        );
        let printed = format!("{auth:?}");
        assert!(printed.contains("binlesson"), "{printed}");
        assert!(printed.contains("https://mcp.example.com/mcp"), "{printed}");
        assert!(!printed.contains("ghp_live"), "{printed}");
    }
}

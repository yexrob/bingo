//! An HTTP server that wants a bearer, and what the manager does about it
//! (ADR-0050 §3).
//!
//! The server here is a scripted streamable-HTTP endpoint rather than the
//! stdio example: a `401` and a `WWW-Authenticate` are what this milestone is
//! about, and only an HTTP server can send one. It plays the resource server
//! and its own authorization server, so a renewal is a real exchange.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bingo_auth_oauth::{CredentialStore, Entry, mcp_key};
use bingo_mcp::{Manager, McpSource, Server, Status};
use bingo_sdk::{
    CancellationToken, Env, ItemBody, ItemId, KernelError, SessionId, Tool, ToolContext, ToolHost,
    ToolSource, TurnId,
};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

// ------------------------------------------------------------- the endpoint

/// A streamable-HTTP MCP server that accepts one bearer at a time and
/// refuses every other with the `401` the specification asks for.
struct Endpoint {
    /// The bearer it accepts now. Changing it mid-test is a token the server
    /// has retired under a live connection.
    accepts: Mutex<String>,
    /// Every `Authorization` it was offered, in order.
    offered: Mutex<Vec<String>>,
    calls: AtomicUsize,
}

/// The endpoint as wiremock holds it: a handle, because the test keeps one
/// too and changes what the server accepts while a connection is live.
#[derive(Clone)]
struct Scripted(Arc<Endpoint>);

impl Endpoint {
    fn new(accepts: &str) -> Arc<Self> {
        Arc::new(Self {
            accepts: Mutex::new(format!("Bearer {accepts}")),
            offered: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        })
    }

    fn now_accepts(&self, bearer: &str) {
        *self.accepts.lock().unwrap() = format!("Bearer {bearer}");
    }

    fn offered(&self) -> Vec<String> {
        self.offered.lock().unwrap().clone()
    }
}

impl Respond for Scripted {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let Scripted(endpoint) = self;
        let offered = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        endpoint.offered.lock().unwrap().push(offered.clone());
        if offered != *endpoint.accepts.lock().unwrap() {
            return ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                r#"Bearer resource_metadata="http://[::1]/.well-known/oauth-protected-resource""#,
            );
        }
        let Ok(message) = serde_json::from_slice::<Value>(&request.body) else {
            return ResponseTemplate::new(400);
        };
        let Some(id) = message.get("id") else {
            // A notification: the specification's own answer is 202.
            return ResponseTemplate::new(202);
        };
        endpoint.calls.fetch_add(1, Ordering::Relaxed);
        ResponseTemplate::new(200)
            .insert_header("mcp-session-id", "ses_endpoint")
            .set_body_json(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result(&message),
            }))
    }
}

/// What each method this test needs answers with.
fn result(message: &Value) -> Value {
    match message.get("method").and_then(Value::as_str) {
        Some("initialize") => json!({
            "protocolVersion": message["params"]["protocolVersion"],
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "scripted", "version": "0.1.0" },
        }),
        Some("tools/list") => json!({
            "tools": [{
                "name": "echo",
                "description": "Say it back.\nThe second line is not a summary.",
                "inputSchema": { "type": "object" },
            }],
        }),
        _ => json!({ "content": [{ "type": "text", "text": "said" }] }),
    }
}

/// The endpoint, mounted, plus the token endpoint its renewals go to.
async fn endpoint(accepts: &str, renews_to: &str) -> (MockServer, Arc<Endpoint>) {
    let server = MockServer::start().await;
    let endpoint = Endpoint::new(accepts);
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(Scripted(Arc::clone(&endpoint)))
        .mount(&server)
        .await;
    // rmcp opens the server-to-client stream on a GET; a server that offers
    // none says so, and the client goes on without it.
    Mock::given(method("GET"))
        .and(path("/mcp"))
        .respond_with(ResponseTemplate::new(405))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": server.uri(),
            "authorization_endpoint": format!("{}/authorize", server.uri()),
            "token_endpoint": format!("{}/token", server.uri()),
            "code_challenge_methods_supported": ["S256"],
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": renews_to,
            "refresh_token": "rt_2",
            "expires_in": 3600,
        })))
        .mount(&server)
        .await;
    (server, endpoint)
}

// ---------------------------------------------------------------- the plugin

fn http(url: String) -> Server {
    Server::Http {
        url,
        headers: BTreeMap::new(),
        oauth: None,
    }
}

fn manager(server: &MockServer) -> (Arc<Manager>, tempfile::TempDir) {
    let data = tempfile::tempdir().expect("a temporary data directory");
    let configured =
        BTreeMap::from([("remote".to_string(), http(format!("{}/mcp", server.uri())))]);
    let manager = Arc::new(Manager::new(configured, &[], data.path().to_path_buf()));
    (manager, data)
}

/// A sign-in this run already has, written the way `login` writes it.
fn signed_in(data: &tempfile::TempDir, issuer: &str, access: &str) {
    CredentialStore::new(data.path().to_path_buf())
        .write(
            &mcp_key("remote"),
            Entry::McpOAuth {
                issuer: issuer.to_string(),
                client_id: "cl_1".into(),
                client_secret: None,
                redirect_uri: "http://localhost:1455/auth/callback".into(),
                access: access.to_string(),
                refresh: Some("rt_1".into()),
                expires: unix_now() + 3600,
                scope: None,
            },
        )
        .expect("a stored sign-in");
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

async fn state(manager: &Arc<Manager>) -> Status {
    manager
        .lines()
        .await
        .into_iter()
        .next()
        .expect("one configured server")
        .status
}

/// Poll until the server reaches a state, or give up: dialling is
/// asynchronous, and a test that asks about it waits for the answer rather
/// than for a clock.
async fn settles(manager: &Arc<Manager>, wanted: impl Fn(&Status) -> bool) -> Status {
    for _ in 0..600 {
        let status = state(manager).await;
        if wanted(&status) {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the server never settled: {:?}", state(manager).await);
}

// -------------------------------------------------------------- the scenarios

#[tokio::test]
async fn a_server_that_wants_a_bearer_nobody_has_needs_authentication() {
    let (server, endpoint) = endpoint("at_1", "at_2").await;
    let (manager, _data) = manager(&server);
    manager.dial_enabled().await;

    let Status::NeedsAuth { why } = state(&manager).await else {
        panic!("a 401 with no stored sign-in: {:?}", state(&manager).await);
    };
    assert!(!why.contains("at_"), "no credential in the reason: {why}");
    let line = manager.lines().await.remove(0);
    assert_eq!(line.auth, Some(bingo_auth_oauth::Status::SignedOut));
    assert!(
        endpoint.offered().iter().all(String::is_empty),
        "nothing was signed in, so nothing was offered"
    );
    assert!(
        manager.tools().await.is_empty(),
        "a server nobody signed in to offers nothing"
    );
}

#[tokio::test]
async fn a_stored_sign_in_is_the_bearer_the_dial_carries() {
    let (server, endpoint) = endpoint("at_1", "at_2").await;
    let (manager, data) = manager(&server);
    signed_in(&data, &server.uri(), "at_1");
    manager.dial_enabled().await;

    assert_eq!(state(&manager).await, Status::Connected { tools: 1 });
    assert!(
        endpoint.offered().contains(&"Bearer at_1".to_string()),
        "the stored token reached the wire: {:?}",
        endpoint.offered()
    );
    let line = manager.lines().await.remove(0);
    assert_eq!(
        line.auth,
        Some(bingo_auth_oauth::Status::SignedIn { account: None })
    );
    assert_eq!(manager.tools().await.len(), 1);
}

/// ADR-0050 §3: a call refused mid-session renews and dials again, once, on
/// a task of its own — the call itself answers the model straight away.
#[tokio::test]
async fn a_call_refused_mid_session_renews_and_dials_again() {
    let (server, endpoint) = endpoint("at_1", "at_2").await;
    let (manager, data) = manager(&server);
    signed_in(&data, &server.uri(), "at_1");
    manager.dial_enabled().await;
    assert_eq!(state(&manager).await, Status::Connected { tools: 1 });

    // The server retires the token this connection was dialled with.
    endpoint.now_accepts("at_2");
    let tool = tools(&manager).await.remove(0);
    let refused = tool
        .call(json!({}), &tool_context())
        .await
        .expect_err("the server refuses the call it was signed in for");
    assert!(!format!("{refused}").contains("at_1"), "{refused}");

    settles(&manager, |status| {
        matches!(status, Status::Connected { .. })
    })
    .await;
    assert!(
        endpoint.offered().contains(&"Bearer at_2".to_string()),
        "the renewed token reached the wire: {:?}",
        endpoint.offered()
    );
    let stored = CredentialStore::new(data.path().to_path_buf())
        .read(&mcp_key("remote"))
        .expect("a read")
        .expect("a stored sign-in");
    let Entry::McpOAuth { access, .. } = stored else {
        panic!("an mcpOauth entry");
    };
    assert_eq!(access, "at_2", "the renewal was written back");
}

/// A server whose renewal fails too has nothing left to try: the person is
/// told, once, in the one word `/mcp` shows.
#[tokio::test]
async fn a_renewal_that_fails_leaves_the_server_needing_authentication() {
    let server = MockServer::start().await;
    let endpoint = Endpoint::new("at_never");
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(Scripted(Arc::clone(&endpoint)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let (manager, data) = manager(&server);
    signed_in(&data, &server.uri(), "at_1");
    manager.dial_enabled().await;

    assert!(
        matches!(state(&manager).await, Status::NeedsAuth { .. }),
        "{:?}",
        state(&manager).await
    );
    assert!(endpoint.offered().contains(&"Bearer at_1".to_string()));
}

/// ADR-0050 §3: a person's own `Authorization` is their answer to who this
/// is. A `401` against it is a settings problem, and reads as one.
#[tokio::test]
async fn a_header_a_person_wrote_themselves_fails_rather_than_needing_a_sign_in() {
    let (server, _endpoint) = endpoint("at_1", "at_2").await;
    let data = tempfile::tempdir().expect("a temporary data directory");
    let configured = BTreeMap::from([(
        "remote".to_string(),
        Server::Http {
            url: format!("{}/mcp", server.uri()),
            headers: BTreeMap::from([("Authorization".to_string(), "Bearer mine".to_string())]),
            oauth: None,
        },
    )]);
    let manager = Arc::new(Manager::new(configured, &[], data.path().to_path_buf()));
    manager.dial_enabled().await;

    let Status::Failed { why } = state(&manager).await else {
        panic!("a person's own header: {:?}", state(&manager).await);
    };
    assert!(!why.contains("mine"), "no credential in the reason: {why}");
    assert!(manager.auth("remote").is_none());
    assert_eq!(manager.lines().await.remove(0).auth, None);
}

/// `/mcp tools <server>`: what one connected server offers, first line only.
#[tokio::test]
async fn a_connected_server_lists_what_it_offers() {
    let (server, _endpoint) = endpoint("at_1", "at_2").await;
    let (manager, data) = manager(&server);
    signed_in(&data, &server.uri(), "at_1");
    manager.dial_enabled().await;

    assert_eq!(
        manager.tools_of("remote").await,
        Some(vec![("echo".to_string(), "Say it back.".to_string())]),
        "a table holds the first line and not the whole description"
    );
    assert_eq!(manager.tools_of("nothing").await, None);
}

// ------------------------------------------------------------------- support

async fn tools(manager: &Arc<Manager>) -> Vec<Arc<dyn Tool>> {
    McpSource::new(Arc::clone(manager)).tools().await
}

/// A call that records nothing and asks nobody.
#[derive(Debug)]
struct NullHost;

#[async_trait::async_trait]
impl bingo_sdk::Prompter for NullHost {
    async fn ask(
        &self,
        _kind: bingo_sdk::InteractionKind,
        _answers: Vec<bingo_sdk::AnswerSpec>,
    ) -> Result<bingo_sdk::Answer, KernelError> {
        Err(KernelError::new(
            bingo_sdk::ErrorCode::Internal,
            "nobody is at this session",
        ))
    }
}

#[async_trait::async_trait]
impl ToolHost for NullHost {
    fn progress(&self, _item: &ItemId, _tail: String) {}

    async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
        Ok(ItemId::from_raw("itm_test"))
    }
}

fn tool_context() -> ToolContext {
    ToolContext {
        call_id: "call_1".into(),
        session: SessionId::from_raw("ses_test"),
        turn: TurnId::from_raw("trn_test"),
        item: ItemId::from_raw("itm_test"),
        cwd: PathBuf::from("/work"),
        cancel: CancellationToken::new(),
        env: Arc::new(Env::rooted("/tmp")),
        host: bingo_sdk::testing::NoHost::handle(),
        call: Arc::new(NullHost),
    }
}

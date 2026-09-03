//! What a real MCP client gets from the tool bridge (ADR-0036 §3).
//!
//! The client is `rmcp`'s own, on the other end of the real rendezvous — a
//! unix socket or a named pipe, dialled and handshaken exactly as the proxy
//! dials it — so what these tests assert is the protocol and not this crate's
//! idea of it. The doors below are a double: what the kernel does with a call
//! is worker Q's, and the seam is the whole of what this side may assume.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use bingo_provider_acp::bridge::doors::{Doors, Refused};
use bingo_provider_acp::bridge::{Address, Bridge, Token, handshake, socket};
use bingo_sdk::{Env, ToolCall, ToolOutput, ToolSpec};
use rmcp::ServiceExt;
use rmcp::handler::client::ClientHandler;
use rmcp::model::{CallToolRequestParams, CallToolResult, JsonObject};
use rmcp::service::{NotificationContext, RoleClient, RunningService};
use serde_json::json;
use tokio::sync::{Mutex, Notify};

/// How long any one wait may take before a scenario is called stalled. CI is
/// slower than a developer's box, so this is generous rather than tight.
const LIMIT: Duration = Duration::from_secs(20);

// ------------------------------------------------------------- the double

/// What the doors answer a call with.
#[derive(Clone)]
enum Answer {
    Output(ToolOutput),
    /// The call never ran: no turn in flight, a gate that said no, an
    /// interrupt.
    Refuse(String),
}

/// A pair of doors that hold an offer and a script, and remember what came
/// through them.
struct Fake {
    offer: Mutex<Vec<ToolSpec>>,
    answer: Mutex<Answer>,
    seen: Mutex<Vec<ToolCall>>,
}

impl Fake {
    fn with(offer: Vec<ToolSpec>, answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            offer: Mutex::new(offer),
            answer: Mutex::new(answer),
            seen: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl Doors for Fake {
    async fn offer(&self) -> Vec<ToolSpec> {
        self.offer.lock().await.clone()
    }

    async fn call(&self, call: ToolCall) -> Result<ToolOutput, Refused> {
        self.seen.lock().await.push(call);
        match self.answer.lock().await.clone() {
            Answer::Output(output) => Ok(output),
            Answer::Refuse(why) => Err(Refused::new(why)),
        }
    }
}

/// The names are invented on purpose. The bridge knows no tool by name — the
/// offer is whatever the doors hold — and a fixture built from a real bingo
/// tool would not show that.
fn spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.to_string(),
        description: format!("What {name} does."),
        input_schema: json!({
            "type": "object",
            "properties": { "text": { "type": "string" } }
        }),
        meta: serde_json::Map::new(),
    }
}

// ------------------------------------------------------------- the client

/// A client that says when it heard `tools/list_changed`.
#[derive(Clone, Default)]
struct Listening {
    heard: Arc<Notify>,
}

impl ClientHandler for Listening {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        self.heard.notify_one();
    }
}

/// One bridge on an address of its own, and the doors behind one token.
struct Rendezvous {
    _home: tempfile::TempDir,
    bridge: Bridge,
    address: Address,
}

impl Rendezvous {
    fn open() -> Self {
        let home = tempfile::tempdir().expect("a temporary home");
        // The address is derived exactly as a run derives it, so what these
        // tests bind is what a run binds — length and all.
        let address = Address::of_run(&Env::rooted(home.path()), std::process::id());
        let bridge = Bridge::at(address.clone()).expect("it listens");
        Self {
            _home: home,
            bridge,
            address,
        }
    }
}

/// Dial, say which conversation this is, and speak MCP. One attempt: a
/// refusal must be a failure here, not a retry.
async fn connect<H: ClientHandler>(
    address: &Address,
    token: &str,
    handler: H,
) -> Result<RunningService<RoleClient, H>, String> {
    let mut stream = socket::dial(address).await.map_err(|e| e.to_string())?;
    handshake::write(&mut stream, token)
        .await
        .map_err(|e| e.to_string())?;
    handler.serve(stream).await.map_err(|e| e.to_string())
}

/// The same, for a stream the far side has to finish letting go of first.
/// Bounded by a deadline rather than by a sleep of a guessed length.
async fn reconnect<H: ClientHandler + Clone>(
    address: &Address,
    token: &str,
    handler: H,
) -> RunningService<RoleClient, H> {
    let deadline = Instant::now() + LIMIT;
    loop {
        match connect(address, token, handler.clone()).await {
            Ok(client) => return client,
            Err(why) if Instant::now() >= deadline => {
                panic!("nothing answered in {LIMIT:?}: {why}")
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

fn arguments(value: serde_json::Value) -> JsonObject {
    match value {
        serde_json::Value::Object(map) => map,
        other => panic!("arguments are an object, not {other}"),
    }
}

fn text_of(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect()
}

// -------------------------------------------------------------- the tests

#[tokio::test]
async fn what_the_doors_offer_is_what_tools_list_shows() {
    let rendezvous = Rendezvous::open();
    let doors = Fake::with(
        vec![spec("Shout"), spec("Wave")],
        Answer::Output(ToolOutput::text("unused")),
    );
    let token = rendezvous.bridge.admit(doors).expect("a token");

    let client = connect(&rendezvous.address, token.as_str(), ())
        .await
        .expect("the bridge answers");
    let offered = client.list_all_tools().await.expect("a list");
    assert_eq!(
        offered
            .iter()
            .map(|t| t.name.to_string())
            .collect::<Vec<_>>(),
        ["Shout", "Wave"],
        "in the order the doors held them"
    );
    assert_eq!(offered[0].description.as_deref(), Some("What Shout does."));
    assert_eq!(offered[0].input_schema["type"], json!("object"));
    let _ = client.cancel().await;
}

#[tokio::test]
async fn a_call_reaches_the_doors_and_its_answer_comes_back() {
    let rendezvous = Rendezvous::open();
    let doors = Fake::with(
        vec![spec("Shout")],
        Answer::Output(ToolOutput::text("posted into #review")),
    );
    let token = rendezvous.bridge.admit(doors.clone()).expect("a token");

    let client = connect(&rendezvous.address, token.as_str(), ())
        .await
        .expect("the bridge answers");
    let answered = client
        .call_tool(
            CallToolRequestParams::new("Shout").with_arguments(arguments(json!({ "text": "hi" }))),
        )
        .await
        .expect("a call is answered");
    assert_eq!(text_of(&answered), "posted into #review");
    assert_eq!(answered.is_error, Some(false));

    let seen = doors.seen.lock().await;
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].name, "Shout");
    assert_eq!(seen[0].input, json!({ "text": "hi" }));
    assert!(
        !seen[0].call_id.is_empty(),
        "the bridge mints the id the call is journaled under"
    );
    drop(seen);
    let _ = client.cancel().await;
}

/// ADR-0036 §2: a call that never ran is still an answer. A protocol error
/// would end the conversation; an error *result* leaves the agent able to say
/// something else instead.
#[tokio::test]
async fn a_call_the_doors_refused_is_an_error_result_not_a_protocol_error() {
    let rendezvous = Rendezvous::open();
    let doors = Fake::with(
        vec![spec("Shout")],
        Answer::Refuse("no turn is in flight".into()),
    );
    let token = rendezvous.bridge.admit(doors).expect("a token");

    let client = connect(&rendezvous.address, token.as_str(), ())
        .await
        .expect("the bridge answers");
    let answered = client
        .call_tool(CallToolRequestParams::new("Shout"))
        .await
        .expect("a refusal is an answer, not a transport failure");
    assert_eq!(answered.is_error, Some(true));
    assert_eq!(text_of(&answered), "no turn is in flight");

    // And the conversation goes on: the client can still be spoken to.
    assert_eq!(client.list_all_tools().await.expect("a list").len(), 1);
    let _ = client.cancel().await;
}

/// `CatalogChanged` becomes MCP's `tools/list_changed` (ADR-0036 §1), and the
/// list the client asks for afterwards is the new one.
#[tokio::test]
async fn a_changed_offer_reaches_a_live_client() {
    let rendezvous = Rendezvous::open();
    let doors = Fake::with(
        vec![spec("Shout")],
        Answer::Output(ToolOutput::text("unused")),
    );
    let token = rendezvous.bridge.admit(doors.clone()).expect("a token");

    let listening = Listening::default();
    let heard = listening.heard.clone();
    let client = connect(&rendezvous.address, token.as_str(), listening)
        .await
        .expect("the bridge answers");
    assert_eq!(client.list_all_tools().await.expect("a list").len(), 1);

    doors.offer.lock().await.push(spec("Wave"));
    rendezvous.bridge.offer_changed();

    tokio::time::timeout(LIMIT, heard.notified())
        .await
        .expect("the notification arrives");
    assert_eq!(
        client
            .list_all_tools()
            .await
            .expect("a list")
            .iter()
            .map(|t| t.name.to_string())
            .collect::<Vec<_>>(),
        ["Shout", "Wave"],
        "asking again after the word is what list_changed is for"
    );
    let _ = client.cancel().await;
}

/// The token is the address (ADR-0036 §3): a stream that cannot say which
/// conversation it is gets none.
#[tokio::test]
async fn a_token_nobody_minted_never_gets_a_conversation() {
    let rendezvous = Rendezvous::open();
    rendezvous
        .bridge
        .admit(Fake::with(vec![spec("Shout")], Answer::Refuse("x".into())))
        .expect("a token");

    let refused = connect(
        &rendezvous.address,
        Token::mint().expect("a token").as_str(),
        (),
    )
    .await
    .expect_err("a token this bridge never minted");
    assert!(!refused.is_empty(), "the stream is closed, not answered");
}

/// One conversation is one agent: a second stream on a token that is already
/// being served is refused while the first lives.
#[tokio::test]
async fn a_second_concurrent_stream_on_one_token_is_refused() {
    let rendezvous = Rendezvous::open();
    let doors = Fake::with(
        vec![spec("Shout")],
        Answer::Output(ToolOutput::text("unused")),
    );
    let token = rendezvous.bridge.admit(doors).expect("a token");

    let first = connect(&rendezvous.address, token.as_str(), ())
        .await
        .expect("the bridge answers");
    connect(&rendezvous.address, token.as_str(), ())
        .await
        .expect_err("the token is already being served");
    assert_eq!(
        first.list_all_tools().await.expect("a list").len(),
        1,
        "and the first conversation is untouched"
    );
    let _ = first.cancel().await;
}

/// An agent's MCP client may respawn a proxy that died and dial again
/// mid-session. A token that could only ever be used once would leave that
/// session mute for the rest of its life.
#[tokio::test]
async fn a_token_may_be_dialled_again_once_its_stream_has_closed() {
    let rendezvous = Rendezvous::open();
    let doors = Fake::with(
        vec![spec("Shout")],
        Answer::Output(ToolOutput::text("still here")),
    );
    let token = rendezvous.bridge.admit(doors).expect("a token");

    let first = connect(&rendezvous.address, token.as_str(), ())
        .await
        .expect("the bridge answers");
    let _ = first.cancel().await;

    let again = reconnect(&rendezvous.address, token.as_str(), ()).await;
    let answered = again
        .call_tool(CallToolRequestParams::new("Shout"))
        .await
        .expect("the same doors are still behind it");
    assert_eq!(text_of(&answered), "still here");
    let _ = again.cancel().await;
}

/// A session that ended has no tools to offer, and its token must stop being
/// an address.
#[tokio::test]
async fn a_dismissed_conversation_is_no_longer_an_address() {
    let rendezvous = Rendezvous::open();
    let doors = Fake::with(
        vec![spec("Shout")],
        Answer::Output(ToolOutput::text("unused")),
    );
    let token = rendezvous.bridge.admit(doors).expect("a token");
    rendezvous.bridge.dismiss(&token);

    connect(&rendezvous.address, token.as_str(), ())
        .await
        .expect_err("the conversation is over");
}

/// The bridge lives as long as the instance that opened it: dropping it takes
/// the listener, the way dropping a `Link` takes an adapter's process group.
#[tokio::test]
async fn a_dropped_bridge_stops_listening() {
    let home = tempfile::tempdir().expect("a temporary home");
    let address = Address::of_run(&Env::rooted(home.path()), std::process::id());
    let bridge = Bridge::at(address.clone()).expect("it listens");
    let token = bridge
        .admit(Fake::with(vec![spec("Shout")], Answer::Refuse("x".into())))
        .expect("a token");
    connect(&address, token.as_str(), ())
        .await
        .expect("it answers while it lives");
    drop(bridge);

    let deadline = Instant::now() + LIMIT;
    loop {
        if socket::dial(&address).await.is_err() {
            return;
        }
        assert!(Instant::now() < deadline, "the bridge is still listening");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

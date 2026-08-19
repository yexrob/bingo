//! The transport's own contract tests: real frames in, real frames out.
//!
//! The fixture suite in `protocol/fixtures_tests.rs` proves that every type
//! serializes to the JSON the contract publishes. These prove the other half —
//! that a line written to the transport produces the line the contract says it
//! produces — by driving the whole loop over a pipe: framing, negotiation,
//! session lifecycle, ordering, and the refusals.
//!
//! What needs a model turn is not here and is not pretended: text, tools,
//! permissions, retries, and steering reach the wire when B7 attaches the
//! engine. Delta coalescing is tested as the function it is, on frames built by
//! hand, for the same reason.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

use super::*;
use crate::app::event::{
    AppEvent, AppEventPayload, EventMeta, ItemDelta, SessionClosed, SessionUpdated,
};
use crate::app::ids::{ConversationId, ItemId, SessionId, TurnId};

const PIPE: usize = 1 << 20;

/// A home of its own, so no test reads or writes the developer's.
struct Root(std::path::PathBuf);

impl Root {
    fn new(label: &str) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "bingo-stdio-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).unwrap_or_else(|error| panic!("{error}"));
        Self(path)
    }

    fn boot(&self) -> Bootstrap {
        Bootstrap {
            home: self.0.clone(),
            user_dir: self.0.join("config"),
            cwd: self.0.clone(),
        }
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One client on the other end of the pipe.
struct Client {
    to: Option<DuplexStream>,
    from: BufReader<DuplexStream>,
    server: tokio::task::JoinHandle<Result<(), AppServerError>>,
    /// Notifications seen while waiting for a response, in the order they came.
    seen: Vec<Value>,
}

impl Client {
    fn open(boot: Bootstrap) -> Self {
        Self::with_pipes(boot, PIPE, PIPE)
    }

    fn with_pipes(boot: Bootstrap, inbound: usize, outbound: usize) -> Self {
        let (to, server_in) = tokio::io::duplex(inbound);
        let (server_out, from) = tokio::io::duplex(outbound);
        let server = tokio::spawn(run(server_in, server_out, boot));
        Self {
            to: Some(to),
            from: BufReader::new(from),
            server,
            seen: Vec::new(),
        }
    }

    async fn write(&mut self, line: &str) {
        let mut framed = line.as_bytes().to_vec();
        framed.push(b'\n');
        self.write_bytes(&framed).await;
    }

    async fn write_bytes(&mut self, bytes: &[u8]) {
        let to = self
            .to
            .as_mut()
            .unwrap_or_else(|| panic!("the client still has its half of the pipe"));
        to.write_all(bytes)
            .await
            .unwrap_or_else(|error| panic!("{error}"));
        to.flush().await.unwrap_or_else(|error| panic!("{error}"));
    }

    /// The next frame, or nothing because the server stopped writing.
    async fn frame(&mut self) -> Option<Value> {
        let mut line = String::new();
        match self.from.read_line(&mut line).await {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(
                serde_json::from_str(line.trim_end()).unwrap_or_else(|error| {
                    panic!("every line the server writes is one JSON frame: {error}: {line}")
                }),
            ),
        }
    }

    /// Send one request and read until its reply, remembering what came first.
    async fn call(&mut self, id: Value, method: &str, params: Value) -> Value {
        let frame = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        self.write(&frame.to_string()).await;
        self.reply(id).await
    }

    /// Read until the reply to this id.
    async fn reply(&mut self, id: Value) -> Value {
        loop {
            let Some(frame) = self.frame().await else {
                panic!("the server closed before answering {id}");
            };
            if frame.get("id") == Some(&id) {
                return frame;
            }
            self.seen.push(frame);
        }
    }

    /// Initialize and say so, which is what a controlling client does.
    async fn handshake(&mut self) -> Value {
        let result = self.call(json!(1), "initialize", initialize_params()).await;
        self.write(&json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}).to_string())
            .await;
        result
    }

    /// Close stdin and wait for the process the transport is standing in for.
    async fn finish(mut self) -> Result<(), AppServerError> {
        drop(self.to.take());
        // Everything still on the wire is still the client's; reading it out is
        // what proves nothing was dropped on the way to the exit.
        while let Some(frame) = self.frame().await {
            self.seen.push(frame);
        }
        self.server
            .await
            .unwrap_or_else(|error| panic!("the transport task panicked: {error}"))
    }
}

fn initialize_params() -> Value {
    json!({
        "protocol": {"major": 1, "minMinor": 0, "maxMinor": 0},
        "client": {"name": "bingo-test", "version": "0.1.0"},
        "capabilities": {"interactionResponse": true}
    })
}

fn error_code(frame: &Value) -> &str {
    frame
        .get("error")
        .and_then(|error| error.get("data"))
        .and_then(|data| data.get("bingoCode"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected an application error, got {frame}"))
}

fn number(frame: &Value) -> i64 {
    frame
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("expected an error code, got {frame}"))
}

// ---------------------------------------------------------------------------
// Initialization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_handshake_agrees_on_a_version_and_says_what_it_can_do() {
    let root = Root::new("handshake");
    let mut client = Client::open(root.boot());
    let frame = client
        .call(json!(1), "initialize", initialize_params())
        .await;
    let result = &frame["result"];
    assert_eq!(result["protocol"], json!({"major": 1, "minor": 0}));
    assert_eq!(result["server"]["name"], json!("bingo"));
    assert_eq!(
        result["server"]["version"],
        json!(env!("CARGO_PKG_VERSION"))
    );
    assert!(
        result["server"]["epoch"]
            .as_str()
            .is_some_and(|epoch| epoch.starts_with("epoch_")),
        "the epoch every identifier belongs to is announced: {result}"
    );
    assert_eq!(
        result["limits"],
        json!({"maxClientFrameBytes": 1_048_576, "maxServerFrameBytes": 8_388_608})
    );
    assert_eq!(result["capabilities"]["reasoning"], json!(true));
    assert!(client.finish().await.is_ok());
}

/// The refusal matrix: every way initialization is not agreed to.
#[tokio::test]
async fn initialization_fails_rather_than_pretending_to_agree() {
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "major",
            json!({
                "protocol": {"major": 2, "minMinor": 0, "maxMinor": 0},
                "client": {"name": "c", "version": "0"},
                "capabilities": {"interactionResponse": true}
            }),
            "PROTOCOL_UNSUPPORTED",
        ),
        (
            "minor",
            json!({
                "protocol": {"major": 1, "minMinor": 7, "maxMinor": 9},
                "client": {"name": "c", "version": "0"},
                "capabilities": {"interactionResponse": true}
            }),
            "PROTOCOL_UNSUPPORTED",
        ),
        (
            "window",
            json!({
                "protocol": {"major": 1, "minMinor": 4, "maxMinor": 1},
                "client": {"name": "c", "version": "0"},
                "capabilities": {"interactionResponse": true}
            }),
            "BAD_ARGUMENT",
        ),
        (
            "prompts",
            json!({
                "protocol": {"major": 1, "minMinor": 0, "maxMinor": 0},
                "client": {"name": "c", "version": "0"},
                "capabilities": {"interactionResponse": false}
            }),
            "CAPABILITY_REQUIRED",
        ),
        (
            "experimental",
            json!({
                "protocol": {"major": 1, "minMinor": 0, "maxMinor": 0},
                "client": {"name": "c", "version": "0"},
                "capabilities": {"interactionResponse": true, "experimental": ["telepathy"]}
            }),
            "CAPABILITY_REQUIRED",
        ),
    ];
    for (label, params, expected) in cases {
        let root = Root::new(&format!("init-{label}"));
        let mut client = Client::open(root.boot());
        let frame = client.call(json!(1), "initialize", params).await;
        assert_eq!(error_code(&frame), expected, "{label}: {frame}");
        assert!(frame.get("result").is_none(), "{label}: {frame}");
        // The two the contract calls non-recoverable end the connection: a
        // client cannot usefully retry either one on it.
        let fatal = matches!(expected, "PROTOCOL_UNSUPPORTED" | "CAPABILITY_REQUIRED");
        match client.finish().await {
            Err(AppServerError::Initialization { kind }) => {
                assert!(fatal, "{label}: this one should have been recoverable");
                assert_eq!(kind.bingo_code(), expected, "{label}");
            }
            other => assert!(!fatal, "{label}: expected the connection to end: {other:?}"),
        }
    }
}

#[tokio::test]
async fn initializing_twice_is_refused() {
    let root = Root::new("init-twice");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    let frame = client
        .call(json!(2), "initialize", initialize_params())
        .await;
    assert_eq!(error_code(&frame), "ALREADY_INITIALIZED");
    let _ = client.finish().await;
}

/// `initialize` completes before anything else is served, and `initialized` is
/// what completes it.
#[tokio::test]
async fn nothing_is_served_before_the_client_says_it_is_ready() {
    let root = Root::new("not-initialized");
    let mut client = Client::open(root.boot());
    let early = client
        .call(json!(1), "catalog/read", json!({"catalog": "providers"}))
        .await;
    assert_eq!(error_code(&early), "NOT_INITIALIZED");

    let _ = client
        .call(json!(2), "initialize", initialize_params())
        .await;
    let between = client
        .call(json!(3), "catalog/read", json!({"catalog": "providers"}))
        .await;
    assert_eq!(
        error_code(&between),
        "NOT_INITIALIZED",
        "the handshake is not finished until `initialized` arrives"
    );

    client
        .write(&json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}).to_string())
        .await;
    let after = client
        .call(json!(4), "catalog/read", json!({"catalog": "providers"}))
        .await;
    assert!(after.get("result").is_some(), "{after}");
    assert!(
        client.seen.is_empty(),
        "no notification may arrive before a snapshot has been read: {:?}",
        client.seen
    );
    let _ = client.finish().await;
}

// ---------------------------------------------------------------------------
// Framing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_malformed_line_is_a_parse_error_and_changes_nothing() {
    let root = Root::new("parse");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    client.write("{not json").await;
    let frame = client
        .frame()
        .await
        .unwrap_or_else(|| panic!("a parse error is still an answer"));
    assert_eq!(frame["id"], Value::Null, "{frame}");
    assert_eq!(number(&frame), -32700);
    // The connection is untouched by it.
    let after = client
        .call(json!(2), "catalog/read", json!({"catalog": "providers"}))
        .await;
    assert!(after.get("result").is_some(), "{after}");
    assert!(client.finish().await.is_ok());
}

#[tokio::test]
async fn a_frame_that_is_not_a_request_is_refused_without_guessing() {
    let root = Root::new("invalid");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    for (line, id) in [
        (json!([1, 2, 3]).to_string(), Value::Null),
        (
            json!({"jsonrpc": "1.0", "id": 5, "method": "shutdown", "params": {}}).to_string(),
            json!(5),
        ),
        (json!({"jsonrpc": "2.0", "id": 6}).to_string(), json!(6)),
        (
            json!({"jsonrpc": "2.0", "id": {"a": 1}, "method": "shutdown"}).to_string(),
            Value::Null,
        ),
    ] {
        client.write(&line).await;
        let frame = client
            .frame()
            .await
            .unwrap_or_else(|| panic!("expected a refusal for {line}"));
        assert_eq!(frame["id"], id, "{frame}");
        assert_eq!(number(&frame), -32600, "{frame}");
    }
    let _ = client.finish().await;
}

#[tokio::test]
async fn an_unknown_method_and_unreadable_params_are_told_apart() {
    let root = Root::new("methods");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    let unknown = client.call(json!(2), "session/teleport", json!({})).await;
    assert_eq!(number(&unknown), -32601, "{unknown}");
    let unreadable = client
        .call(json!(3), "catalog/read", json!({"catalog": "unicorns"}))
        .await;
    assert_eq!(error_code(&unreadable), "BAD_ARGUMENT");
    assert_eq!(number(&unreadable), -32602, "{unreadable}");
    let _ = client.finish().await;
}

/// A notification is answered with nothing at all, including one this build does
/// not know.
#[tokio::test]
async fn a_notification_is_never_answered() {
    let root = Root::new("notification");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    client
        .write(&json!({"jsonrpc": "2.0", "method": "clientYawned", "params": {}}).to_string())
        .await;
    let after = client
        .call(json!(2), "catalog/read", json!({"catalog": "providers"}))
        .await;
    assert!(after.get("result").is_some(), "{after}");
    assert!(client.seen.is_empty(), "{:?}", client.seen);
    let _ = client.finish().await;
}

#[tokio::test]
async fn a_request_id_already_in_flight_is_refused() {
    let root = Root::new("duplicate");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    // Two frames with one id, in a single write so both are on the wire before
    // the server reads either.
    //
    // Whether the second is *read* while the first is still in flight is the
    // server's own ordering, not the client's: its loop prefers the core's reply
    // over the next client line, so a core that answers quickly enough frees the
    // id before the duplicate is looked at and both calls succeed. That is
    // correct, and it is not what this test is about — so the pairing is retried
    // with a fresh id until the window the guard lives in actually opens.
    let mut refused = 0;
    for attempt in 0..16 {
        let id = json!(100 + attempt);
        let call = json!({"jsonrpc": "2.0", "id": id, "method": "catalog/read", "params": {"catalog": "skills"}});
        client.write(&format!("{call}\n{call}")).await;
        let first = client.reply(id.clone()).await;
        let second = client.reply(id.clone()).await;
        refused = [&first, &second]
            .into_iter()
            .filter(|frame| frame.get("error").is_some())
            .count();
        assert!(
            refused < 2,
            "a duplicate refuses the second, never both: {first} / {second}"
        );
        if refused == 1 {
            // The id is free again once it has been answered.
            let again = client
                .call(id, "catalog/read", json!({"catalog": "skills"}))
                .await;
            assert!(again.get("result").is_some(), "{again}");
            break;
        }
    }
    assert_eq!(refused, 1, "no round ever put two frames in flight at once");
    let _ = client.finish().await;
}

/// Within a major version, a field this build has never heard of is additive.
#[tokio::test]
async fn unknown_additive_fields_are_accepted() {
    let root = Root::new("additive");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    client
        .write(
            &json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "catalog/read",
                "params": {"catalog": "providers", "sortedBy": "a future minor"},
                "traceparent": "00-0af7-00f0-01"
            })
            .to_string(),
        )
        .await;
    let frame = client.reply(json!(2)).await;
    assert!(frame.get("result").is_some(), "{frame}");
    let _ = client.finish().await;
}

#[tokio::test]
async fn a_line_past_the_client_ceiling_closes_the_transport() {
    let root = Root::new("oversized");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    let huge = "x".repeat(MAX_CLIENT_FRAME_BYTES + 1);
    client
        .write(&json!({"jsonrpc": "2.0", "id": 2, "method": "session/rename", "params": {"name": huge}}).to_string())
        .await;
    match client.finish().await {
        Err(AppServerError::FrameTooLarge { limit }) => {
            assert_eq!(limit, MAX_CLIENT_FRAME_BYTES);
        }
        other => panic!("expected the transport to close: {other:?}"),
    }
}

#[tokio::test]
async fn input_that_is_not_utf8_closes_the_transport() {
    let root = Root::new("utf8");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    client.write_bytes(&[0xff, 0xfe, b'\n']).await;
    match client.finish().await {
        Err(AppServerError::Framing { .. }) => {}
        other => panic!("expected the transport to close: {other:?}"),
    }
}

#[tokio::test]
async fn eof_ends_the_connection_cleanly() {
    let root = Root::new("eof");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    let _ = client.call(json!(2), "session/start", json!({})).await;
    let outcome = client.finish().await;
    assert!(outcome.is_ok(), "{outcome:?}");
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_session_starts_reads_closes_and_leaves_the_catalogs_answering() {
    let root = Root::new("lifecycle");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;

    let started = client.call(json!(2), "session/start", json!({})).await;
    let snapshot = &started["result"]["snapshot"];
    let session_id = snapshot["session"]["id"].clone();
    assert_eq!(snapshot["session"]["state"], json!("active"));
    assert_eq!(snapshot["session"]["resumed"], json!(false));
    let cursor = snapshot["eventCursor"].as_u64().unwrap_or_default();

    // A read of the session is the same shape, and the conversation the session
    // opens with is main.
    let read = client.call(json!(3), "session/read", json!({})).await;
    assert_eq!(read["result"]["snapshot"]["session"]["id"], session_id);
    let conversations = client.call(json!(4), "conversation/list", json!({})).await;
    let main = conversations["result"]["conversations"]["items"][0]["id"].clone();
    assert_eq!(
        conversations["result"]["conversations"]["items"][0]["kind"],
        json!({"type": "main"})
    );

    // Reading a conversation never marks it read; only markRead does.
    let revision = conversations["result"]["conversations"]["items"][0]["revision"]
        .as_u64()
        .unwrap_or_default();
    let marked = client
        .call(
            json!(5),
            "conversation/markRead",
            json!({"conversationId": main, "expectedRevision": revision}),
        )
        .await;
    assert_eq!(marked["result"]["conversation"]["unread"], json!(0));

    let closed = client.call(json!(6), "session/close", json!({})).await;
    assert_eq!(closed["result"]["sessionId"], session_id);
    let gone = client.call(json!(7), "session/read", json!({})).await;
    assert_eq!(error_code(&gone), "NO_ACTIVE_SESSION");
    // The catalogs still answer: they never needed a session.
    let catalog = client
        .call(json!(8), "catalog/read", json!({"catalog": "providers"}))
        .await;
    assert!(catalog.get("result").is_some(), "{catalog}");

    // The session said it closed, and it said so after the snapshot that cut it.
    let closing = client
        .seen
        .iter()
        .find(|frame| frame["method"] == json!("session/closed"))
        .unwrap_or_else(|| panic!("the close is announced: {:?}", client.seen));
    assert_eq!(closing["params"]["sessionId"], session_id);
    assert!(
        closing["params"]["event"]["seq"]
            .as_u64()
            .unwrap_or_default()
            > cursor,
        "every event after a snapshot has a greater seq than its cursor"
    );
    assert!(client.finish().await.is_ok());
}

#[tokio::test]
async fn a_session_is_resumed_by_the_name_it_keeps_across_epochs() {
    let root = Root::new("resume");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    let started = client.call(json!(2), "session/start", json!({})).await;
    let first = started["result"]["snapshot"]["session"].clone();
    let stem = first["title"].as_str().unwrap_or_default().to_string();
    // A started session is one the client can name again: `session/list` shows
    // it, and the locator it shows is what `session/resume` takes.
    let listed = client.call(json!(3), "session/list", json!({})).await;
    assert!(
        listed["result"]["sessions"]["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|entry| entry["locator"]
                == json!({"type": "stem", "stem": stem})
                && entry["open"] == json!(true))),
        "the open session is listed and says it is open: {listed}"
    );

    let resumed = client
        .call(
            json!(4),
            "session/resume",
            json!({"locator": {"type": "stem", "stem": stem}}),
        )
        .await;
    let session = &resumed["result"]["snapshot"]["session"];
    assert_eq!(session["resumed"], json!(true), "{resumed}");
    assert_ne!(
        session["epoch"], first["epoch"],
        "replacing the actor mints a new epoch, and the old identifiers die with it"
    );
    let missing = client
        .call(
            json!(5),
            "session/resume",
            json!({"locator": {"type": "stem", "stem": "no-such-session"}}),
        )
        .await;
    assert_eq!(error_code(&missing), "SESSION_NOT_FOUND");
    let _ = client.finish().await;
}

#[tokio::test]
async fn deleting_the_open_session_is_refused_and_deleting_another_is_not() {
    let root = Root::new("delete");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    let started = client.call(json!(2), "session/start", json!({})).await;
    let locator = started["result"]["snapshot"]["session"]["locator"].clone();
    let refused = client
        .call(json!(3), "session/delete", json!({"locator": locator}))
        .await;
    assert_eq!(error_code(&refused), "BAD_ARGUMENT");
    let missing = client
        .call(
            json!(4),
            "session/delete",
            json!({"locator": {"type": "stem", "stem": "not-a-session"}}),
        )
        .await;
    assert_eq!(error_code(&missing), "SESSION_NOT_FOUND");
    let _ = client.finish().await;
}

/// The reply is written before the events it caused (spec invariant #3).
#[tokio::test]
async fn a_response_is_written_before_the_events_it_caused() {
    let root = Root::new("ordering");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    let started = client.call(json!(2), "session/start", json!({})).await;
    let cursor = started["result"]["snapshot"]["eventCursor"]
        .as_u64()
        .unwrap_or_default();
    let main = started["result"]["snapshot"]["conversations"]["active"][0]["id"].clone();

    let executed = client
        .call(
            json!(3),
            "action/execute",
            json!({
                "originConversationId": main,
                "action": {"type": "themeSet", "theme": "dark"}
            }),
        )
        .await;
    assert_eq!(
        executed["result"]["disposition"]["result"]["status"],
        json!("applied"),
        "{executed}"
    );
    assert!(
        client.seen.is_empty(),
        "the reply comes first: {:?}",
        client.seen
    );
    // And the event it caused follows it.
    let changed = client.call(json!(4), "config/read", json!({})).await;
    assert_eq!(changed["result"]["config"]["theme"], json!("dark"));
    let event = client
        .seen
        .iter()
        .find(|frame| frame["method"] == json!("config/changed"))
        .unwrap_or_else(|| panic!("the change is announced: {:?}", client.seen));
    assert!(event["params"]["event"]["seq"].as_u64().unwrap_or_default() > cursor);
    let _ = client.finish().await;
}

/// Every method travels the transport, and answers with either its result or an
/// error it declares. Nothing here is a stub: the frames are real on both sides.
#[tokio::test]
async fn every_method_travels_the_transport_and_answers_within_its_contract() {
    let root = Root::new("every-method");
    let mut client = Client::open(root.boot());
    let _ = client.handshake().await;
    let started = client.call(json!(2), "session/start", json!({})).await;
    let snapshot = &started["result"]["snapshot"];
    let main = snapshot["conversations"]["active"][0]["id"].clone();
    let revision = snapshot["conversations"]["active"][0]["revision"]
        .as_u64()
        .unwrap_or_default();
    assert!(
        main.is_string(),
        "a started session opens with main: {snapshot}"
    );

    let calls: Vec<(RequestMethod, Value)> = vec![
        (RequestMethod::SessionList, json!({})),
        (RequestMethod::SessionRead, json!({})),
        (RequestMethod::ConversationList, json!({})),
        (
            RequestMethod::ConversationRead,
            json!({"conversationId": main}),
        ),
        (
            RequestMethod::ConversationMarkRead,
            json!({"conversationId": main, "expectedRevision": revision}),
        ),
        (
            RequestMethod::ConversationSubmit,
            json!({
                "conversationId": main,
                "input": {"type": "composer", "mode": "normal", "text": "/help", "attachments": []}
            }),
        ),
        (
            RequestMethod::TurnInterrupt,
            json!({"conversationId": main, "turnId": "turn_never"}),
        ),
        (RequestMethod::QueueRead, json!({"conversationId": main})),
        (
            RequestMethod::QueueReclaimTail,
            json!({"conversationId": main}),
        ),
        (
            RequestMethod::InteractionRespond,
            json!({
                "interactionId": "int_never",
                "activation": "pointer",
                "decision": {"type": "deny"}
            }),
        ),
        (RequestMethod::ActionList, json!({})),
        (
            RequestMethod::ActionExecute,
            json!({
                "originConversationId": main,
                "action": {"type": "themeSet", "theme": "light"}
            }),
        ),
        (RequestMethod::ConfigRead, json!({})),
        (RequestMethod::CatalogRead, json!({"catalog": "models"})),
        (RequestMethod::ResourceRead, json!({"resource": "agents"})),
        (
            RequestMethod::AssetRegisterPath,
            json!({"path": root.0.join("nothing.png")}),
        ),
        (
            RequestMethod::AssetReadChunk,
            json!({"assetId": "asset_never", "offset": 0, "length": 16}),
        ),
        (
            RequestMethod::SessionDelete,
            json!({"locator": {"type": "stem", "stem": "not-a-session"}}),
        ),
    ];
    let mut id = 10i64;
    for (method, params) in calls {
        id += 1;
        let frame = client.call(json!(id), method.as_str(), params).await;
        if frame.get("result").is_some() {
            continue;
        }
        let code = error_code(&frame);
        assert!(
            method
                .declared_errors()
                .iter()
                .any(|declared| declared.bingo_code() == code),
            "{}: {code} is not one of the errors it declares",
            method.as_str()
        );
    }
    // The transport's own four, and the two that end the connection, are covered
    // by their own tests above.
    let _ = client.finish().await;
}

/// A method that needs a session and a method that does not are decided by the
/// errors each one declares, so the transport's table cannot drift from the
/// published manifest.
#[test]
fn the_session_free_methods_are_the_ones_that_declare_no_missing_session() {
    for method in RequestMethod::ALL {
        // The transport answers these four itself; a session is not what they
        // are about.
        if matches!(
            method,
            RequestMethod::Initialize
                | RequestMethod::Shutdown
                | RequestMethod::SessionStart
                | RequestMethod::SessionResume
        ) {
            continue;
        }
        let declares = method
            .declared_errors()
            .contains(&ProtocolErrorKind::NoActiveSession);
        assert_eq!(
            needs_session(*method),
            declares,
            "{} disagrees with its own manifest",
            method.as_str()
        );
    }
}

// ---------------------------------------------------------------------------
// Backpressure and coalescing
// ---------------------------------------------------------------------------

fn meta(seq: u64) -> EventMeta {
    EventMeta {
        seq,
        ts: 1_760_000_000_000,
        session_id: SessionId::new("sess_1"),
        caused_by: None,
        coalesced_from: None,
    }
}

fn delta(seq: u64, item: &str, delta_seq: u64, text: &str) -> Wire {
    Wire::Notification(Box::new(ServerNotification::from(AppEvent {
        meta: meta(seq),
        payload: AppEventPayload::ItemTextDelta(ItemDelta {
            conversation_id: ConversationId::new("conv_main"),
            turn_id: Some(TurnId::new("turn_9")),
            item_id: ItemId::new(item),
            delta_seq,
            delta: text.to_string(),
        }),
    })))
}

fn lifecycle(seq: u64) -> Wire {
    Wire::Notification(Box::new(ServerNotification::from(AppEvent {
        meta: meta(seq),
        payload: AppEventPayload::SessionClosed(SessionClosed {
            session_id: SessionId::new("sess_1"),
            reason: SessionCloseReason::Requested,
        }),
    })))
}

#[test]
fn adjacent_appends_for_one_item_become_one_frame_that_says_so() {
    let mut batch = vec![
        delta(5, "item_1", 1, "I will "),
        delta(6, "item_1", 2, "run "),
        delta(7, "item_1", 3, "the tests"),
    ];
    coalesce(&mut batch);
    assert_eq!(batch.len(), 1);
    let Wire::Notification(frame) = &batch[0] else {
        panic!("a delta stays a notification");
    };
    let ServerNotification::ItemTextDelta(params) = frame.as_ref() else {
        panic!("a text delta stays a text delta");
    };
    assert_eq!(params.body.delta, "I will run the tests");
    assert_eq!(params.body.delta_seq, 3, "the run's last append numbers it");
    assert_eq!(params.event.seq, 7, "the run's last sequence number");
    assert_eq!(
        params.event.coalesced_from,
        Some(5),
        "and the one it started at, so the stream is still gapless"
    );
}

#[test]
fn a_lifecycle_event_is_never_merged_or_dropped() {
    let mut batch = vec![
        delta(5, "item_1", 1, "one"),
        lifecycle(6),
        delta(7, "item_1", 2, "two"),
        delta(8, "item_2", 1, "other item"),
        lifecycle(9),
    ];
    coalesce(&mut batch);
    assert_eq!(
        batch.len(),
        5,
        "nothing here is adjacent to something it may merge with"
    );
    let kept: Vec<u64> = batch
        .iter()
        .map(|frame| match frame {
            Wire::Notification(notification) => match notification.as_ref() {
                ServerNotification::ItemTextDelta(params) => params.event.seq,
                ServerNotification::SessionClosed(params) => params.event.seq,
                other => panic!("unexpected frame: {other:?}"),
            },
            other => panic!("unexpected frame: {other:?}"),
        })
        .collect();
    assert_eq!(kept, vec![5, 6, 7, 8, 9]);
}

#[test]
fn a_response_is_never_merged_into_a_notification() {
    let mut batch = vec![
        delta(5, "item_1", 1, "one"),
        Wire::Response(Box::new(ResponseFrame::result(
            7,
            ResponseResult::Shutdown(ShutdownResult {
                interrupted_turns: 0,
                denied_interactions: 0,
            }),
        ))),
        delta(6, "item_1", 2, "two"),
    ];
    coalesce(&mut batch);
    assert_eq!(batch.len(), 3);
}

/// Reasoning and text are two streams. Merging one into the other would put
/// thinking into the answer.
#[test]
fn reasoning_and_text_are_never_merged_together() {
    let reasoning = Wire::Notification(Box::new(ServerNotification::from(AppEvent {
        meta: meta(6),
        payload: AppEventPayload::ItemReasoningDelta(ItemDelta {
            conversation_id: ConversationId::new("conv_main"),
            turn_id: Some(TurnId::new("turn_9")),
            item_id: ItemId::new("item_1"),
            delta_seq: 1,
            delta: "thinking".to_string(),
        }),
    })));
    let mut batch = vec![delta(5, "item_1", 1, "answer"), reasoning];
    coalesce(&mut batch);
    assert_eq!(batch.len(), 2);
}

/// A frame past the server ceiling is never written. A response becomes the
/// error that says so; a notification cannot, because dropping a lifecycle event
/// is exactly what the protocol forbids, so it closes the transport instead.
#[test]
fn a_frame_past_the_server_ceiling_is_refused_rather_than_written() {
    let huge = "x".repeat(MAX_SERVER_FRAME_BYTES);
    let response = Wire::Response(Box::new(ResponseFrame::result(
        7,
        ResponseResult::AssetReadChunk(
            crate::app_server::protocol::requests::AssetReadChunkResult {
                data: huge.clone(),
                next_offset: 0,
                eof: true,
            },
        ),
    )));
    let line = encode(&response).unwrap_or_else(|error| panic!("{error}"));
    assert!(line.len() < MAX_SERVER_FRAME_BYTES);
    assert!(
        line.contains("FRAME_TOO_LARGE"),
        "{}",
        &line[..80.min(line.len())]
    );

    let notification = Wire::Notification(Box::new(ServerNotification::from(AppEvent {
        meta: meta(5),
        payload: AppEventPayload::SessionUpdated(SessionUpdated {
            session: crate::app::snapshot::SessionSummary {
                id: SessionId::new("sess_1"),
                epoch: crate::app::ids::EpochId::new("epoch_1"),
                title: huge,
                state: crate::app::snapshot::SessionState::Active,
                cwd: "/repo".into(),
                locator: crate::app::snapshot::SessionLocator::Latest,
                provider: "default".to_string(),
                model: "sonnet".to_string(),
                thinking: crate::app::snapshot::ThinkingLevel::Off,
                permission_mode: crate::app::snapshot::PermissionMode::Default,
                created_at: 0,
                updated_at: 0,
                resumed: false,
            },
        }),
    })));
    assert!(encode(&notification).is_err());
}

/// A client that stops reading loses the transport, not the stream: the write
/// deadline runs out, the notice is best-effort, and the connection ends.
#[tokio::test(start_paused = true)]
async fn a_client_that_stops_reading_loses_the_transport() {
    let root = Root::new("slow");
    // An outbound pipe too small for one frame, and a client that never drains
    // it: the writer blocks on the first response there is.
    let mut client = Client::with_pipes(root.boot(), PIPE, 8);
    client
        .write(
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": initialize_params()
            })
            .to_string(),
        )
        .await;
    let outcome = client
        .server
        .await
        .unwrap_or_else(|error| panic!("the transport task panicked: {error}"));
    assert!(
        matches!(outcome, Err(AppServerError::ClientTooSlow)),
        "{outcome:?}"
    );
}

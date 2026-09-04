//! The wire, black box: a scripted `HostApi` behind `serve`, driven once by raw
//! JSON-RPC lines and once by `RemoteKernel`. Nothing here reaches inside the
//! server; what the tests assert is what a GUI would see.

// An integration test is not `cfg(test)`; the test-only lint relief is spelled
// out, the way `crates/bingo/tests/cli/main.rs` spells it out.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod host;

use std::time::Duration;

use bingo_sdk::{
    Activation, Answer, Attachment, CatalogKind, Delivery, ErrorCode, Exit, HostApi, HostHandle,
    Image, Input, IntentId, InteractionId, InterruptScope, KernelError, OpenOptions, Origin,
};
use bingo_surface_rpc::codec::{
    self, INVALID_PARAMS, INVALID_REQUEST, Id, KERNEL_ERROR, METHOD_NOT_FOUND, Message,
    PARSE_ERROR, Request, Response, RpcError,
};
use bingo_surface_rpc::methods::{METHODS, PROTOCOL, name};
use bingo_surface_rpc::{RemoteKernel, serve};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};

use host::{TestHost, child_id, fresh_state, last_seq, script, selector, session_id, who};
use tokio::io::{DuplexStream, ReadHalf, WriteHalf};
use tokio::task::JoinHandle;
use tokio_util::codec::{FramedRead, FramedWrite};

// ---- the raw wire --------------------------------------------------------

/// A client that writes lines and reads them back, so the assertions are about
/// bytes rather than about the server's insides.
struct Wire {
    reader: FramedRead<ReadHalf<DuplexStream>, tokio_util::codec::LinesCodec>,
    writer: FramedWrite<WriteHalf<DuplexStream>, tokio_util::codec::LinesCodec>,
    served: JoinHandle<Result<Exit, KernelError>>,
    next: i64,
}

impl Wire {
    fn start(host: HostHandle) -> Wire {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (server_reader, server_writer) = tokio::io::split(server);
        let (client_reader, client_writer) = tokio::io::split(client);
        Wire {
            reader: FramedRead::new(client_reader, codec::lines()),
            writer: FramedWrite::new(client_writer, codec::lines()),
            served: tokio::spawn(serve(host, server_reader, server_writer)),
            next: 1,
        }
    }

    async fn started(host: HostHandle) -> Wire {
        let mut wire = Wire::start(host);
        wire.call(
            name::INITIALIZE,
            json!({ "client": who(), "protocol": PROTOCOL }),
        )
        .await
        .expect("the handshake succeeds");
        wire
    }

    /// A started wire with the scripted session already open.
    async fn opened(host: HostHandle) -> Wire {
        let mut wire = Wire::started(host).await;
        wire.call(name::SESSION_OPEN, json!({ "selector": selector() }))
            .await
            .expect("the session opens");
        wire
    }

    async fn line(&mut self, line: &str) {
        self.writer
            .send(line.to_owned())
            .await
            .expect("the server is listening");
    }

    async fn ask(&mut self, method: &str, params: Value) -> Id {
        let id = Id::Number(self.next);
        self.next += 1;
        let request = Message::Request(Request::new(id.clone(), method, params));
        let line = serde_json::to_string(&request).expect("a request serialises");
        self.line(&line).await;
        id
    }

    /// Bounded, so a wrong expectation fails the test instead of hanging it.
    async fn recv(&mut self) -> Message {
        let line = tokio::time::timeout(Duration::from_secs(5), self.reader.next())
            .await
            .expect("the server answers within five seconds")
            .expect("the server answers")
            .expect("the line is readable");
        serde_json::from_str(&line).expect("the server speaks json-rpc")
    }

    /// Ask, then read past any notification until the matching reply.
    async fn call(&mut self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.ask(method, params).await;
        loop {
            if let Message::Response(response) = self.recv().await
                && response.id.as_ref() == Some(&id)
            {
                return match response.outcome {
                    codec::Outcome::Result(value) => Ok(value),
                    codec::Outcome::Error(error) => Err(error),
                };
            }
        }
    }

    async fn finish(mut self) -> Exit {
        self.call(name::SHUTDOWN, json!({}))
            .await
            .expect("shutdown is answered");
        self.served
            .await
            .expect("the server task did not panic")
            .expect("the server ended cleanly")
    }
}

fn error_of(response: &Message) -> &RpcError {
    match response {
        Message::Response(Response {
            outcome: codec::Outcome::Error(error),
            ..
        }) => error,
        other => panic!("expected an error, got {other:?}"),
    }
}

// ---- the handshake -------------------------------------------------------

#[tokio::test]
async fn initialize_answers_the_table_it_dispatches_from() {
    let (host, _) = TestHost::with(script());
    let mut wire = Wire::start(host);
    let result = wire
        .call(
            name::INITIALIZE,
            json!({ "client": who(), "protocol": PROTOCOL }),
        )
        .await
        .expect("the handshake succeeds");
    assert_eq!(result["protocol"], json!(PROTOCOL));
    assert_eq!(result["name"], json!("bingo"));
    let methods = result["capabilities"]["methods"]
        .as_array()
        .expect("the capabilities list the methods");
    assert_eq!(methods.len(), METHODS.len());
    assert_eq!(wire.finish().await, Exit { code: 0 });
}

#[tokio::test]
async fn every_other_method_before_initialize_is_not_initialized() {
    let (host, _) = TestHost::with(script());
    let mut wire = Wire::start(host);
    for &(method, ..) in METHODS {
        if method == name::INITIALIZE {
            continue;
        }
        let error = wire
            .call(method, json!({}))
            .await
            .expect_err("nothing is served before the handshake");
        assert_eq!(error.code, KERNEL_ERROR, "{method}");
        assert_eq!(
            error.data,
            Some(json!({ "code": "NOT_INITIALIZED" })),
            "{method}"
        );
    }
}

#[tokio::test]
async fn initializing_twice_is_an_invalid_request() {
    let (host, _) = TestHost::with(script());
    let mut wire = Wire::started(host).await;
    let error = wire
        .call(
            name::INITIALIZE,
            json!({ "client": who(), "protocol": PROTOCOL }),
        )
        .await
        .expect_err("one handshake per connection");
    assert_eq!(error.code, INVALID_REQUEST);
}

// ---- the methods ---------------------------------------------------------

#[tokio::test]
async fn every_method_in_the_table_is_dispatched() {
    let (host, _) = TestHost::with(script());
    let mut wire = Wire::opened(host).await;
    for &(method, ..) in METHODS {
        if method == name::SHUTDOWN || method == name::INITIALIZE {
            continue;
        }
        let answer = wire.call(method, params_for(method)).await;
        let code = answer.err().map(|error| error.code);
        assert_ne!(code, Some(METHOD_NOT_FOUND), "{method} is not dispatched");
        assert_ne!(code, Some(INVALID_PARAMS), "{method} rejected its params");
    }
    assert_eq!(wire.finish().await, Exit { code: 0 });
}

/// One well formed params object per method, so the round trip is real.
fn params_for(method: &str) -> Value {
    let session = json!(session_id());
    let intent = json!(IntentId::from_raw("req_1"));
    match method {
        name::SESSION_LIST => json!({ "filter": {} }),
        name::SESSION_OPEN => json!({ "selector": selector() }),
        name::SESSION_CLOSE | name::SESSION_DELETE => json!({ "session": session }),
        name::SESSION_HISTORY => json!({ "session": session, "page": { "limit": 10 } }),
        name::SESSION_EVENTS => json!({ "session": session, "since": 0 }),
        name::SESSION_SUBMIT => json!({
            "session": session,
            "intent": intent,
            "input": Input::text("hi", Origin::surface("test")),
        }),
        name::SESSION_INTERRUPT => json!({
            "session": session,
            "intent": intent,
            "scope": InterruptScope::Head,
        }),
        name::SESSION_ANSWER => json!({
            "session": session,
            "intent": intent,
            "interaction": InteractionId::from_raw("int_1"),
            "answer": Answer::AllowOnce,
            "activation": Activation::Programmatic,
        }),
        name::SESSION_DELIVER => json!({
            "session": session,
            "intent": intent,
            "input": Input::text("from a peer", Origin::surface("test")),
            "delivery": "wake",
        }),
        name::SESSION_EXTEND => json!({
            "session": session,
            "plugin": "bingo.test",
            "kind": "things",
            "payload": [1, 2, 3],
        }),
        name::SESSION_SIGNAL => json!({
            "session": session,
            "plugin": "bingo.test",
            "kind": "progress",
            "payload": { "kind": "progress", "value": 1, "total": 3 },
        }),
        name::CATALOG_READ => json!({ "kind": CatalogKind::Providers }),
        _ => json!({}),
    }
}

/// `session/deliver`, `session/extend` and `session/signal` are `HostApi`
/// one-to-one (ADR-0011, ADR-0013): what a client sends is what the kernel is handed.
#[tokio::test]
async fn a_delivery_an_extension_and_a_signal_reach_the_kernel_verbatim() {
    let (host, session) = TestHost::with(script());
    let mut wire = Wire::opened(host).await;
    let result = wire
        .call(name::SESSION_DELIVER, params_for(name::SESSION_DELIVER))
        .await
        .expect("a delivery is accepted");
    assert_eq!(result, json!({}));
    wire.call(name::SESSION_EXTEND, params_for(name::SESSION_EXTEND))
        .await
        .expect("an extension is accepted");
    wire.call(name::SESSION_SIGNAL, params_for(name::SESSION_SIGNAL))
        .await
        .expect("a signal is accepted");
    assert_eq!(wire.finish().await, Exit { code: 0 });
    let signalled = session.signalled.lock().unwrap();
    assert_eq!(
        signalled.as_slice(),
        [(
            "bingo.test".to_string(),
            "progress".to_string(),
            json!({ "kind": "progress", "value": 1, "total": 3 })
        )]
    );

    let delivered = session.delivered.lock().unwrap();
    assert_eq!(delivered.len(), 1);
    let (intent, input, delivery) = &delivered[0];
    assert_eq!(intent, &IntentId::from_raw("req_1"));
    assert_eq!(*delivery, Delivery::Wake);
    assert!(matches!(input, Input::Text { text, .. } if text == "from a peer"));
    let extended = session.extended.lock().unwrap();
    assert_eq!(
        extended.as_slice(),
        [(
            "bingo.test".to_string(),
            "things".to_string(),
            json!([1, 2, 3])
        )]
    );
}

/// `Image` is on the wire by serde alone (ADR-0040 §4): a submit carrying one
/// reaches the kernel as the `Input::Text` a surface built, unaltered.
#[tokio::test]
async fn a_submitted_image_reaches_the_kernel_beside_the_text() {
    let (host, session) = TestHost::with(script());
    let mut wire = Wire::opened(host).await;
    let image = Image::from_bytes("image/png", b"a tiny picture").expect("a small image");
    let input = Input::Text {
        text: "look".into(),
        images: vec![image.clone()],
        origin: Origin::surface("test"),
        delivery: Delivery::Wake,
    };
    wire.call(
        name::SESSION_SUBMIT,
        json!({
            "session": json!(session_id()),
            "intent": json!(IntentId::from_raw("req_1")),
            "input": input,
        }),
    )
    .await
    .expect("a submit with an image is accepted");
    assert_eq!(wire.finish().await, Exit { code: 0 });
    let submits = session.submits();
    assert_eq!(submits.len(), 1);
    assert_eq!(
        submits[0].1,
        Input::Text {
            text: "look".into(),
            images: vec![image],
            origin: Origin::surface("test"),
            delivery: Delivery::Wake,
        }
    );
}

#[tokio::test]
async fn a_write_reaches_the_actor_and_answers_nothing() {
    let (host, session) = TestHost::with(script());
    let mut wire = Wire::opened(host).await;
    let intent = IntentId::from_raw("req_write");
    let result = wire
        .call(name::SESSION_SUBMIT, params_for(name::SESSION_SUBMIT))
        .await
        .expect("a submit is accepted");
    assert_eq!(result, json!({}), "a write returns nothing (ADR-0007)");
    wire.call(name::SESSION_INTERRUPT, params_for(name::SESSION_INTERRUPT))
        .await
        .expect("an interrupt is accepted");
    wire.call(name::SESSION_ANSWER, params_for(name::SESSION_ANSWER))
        .await
        .expect("an answer is accepted");
    assert_eq!(wire.finish().await, Exit { code: 0 });
    assert_eq!(session.submits().len(), 1);
    assert_ne!(
        session.submits()[0].0,
        intent,
        "the client mints the intent"
    );
    assert_eq!(
        session
            .interrupts
            .lock()
            .expect("the recorder is not poisoned")
            .len(),
        1
    );
    assert_eq!(
        session
            .answers
            .lock()
            .expect("the recorder is not poisoned")
            .len(),
        1
    );
}

#[tokio::test]
async fn history_and_catalog_answer_sdk_types() {
    let (host, session) = TestHost::with(script());
    let mut wire = Wire::opened(host).await;
    let chunk = wire
        .call(name::SESSION_HISTORY, params_for(name::SESSION_HISTORY))
        .await
        .expect("history is paged");
    assert_eq!(chunk["generation"], json!(3));
    let catalog = wire
        .call(name::CATALOG_READ, params_for(name::CATALOG_READ))
        .await
        .expect("the catalogue is read");
    assert_eq!(catalog["kind"], json!("providers"));
    assert_eq!(catalog["entries"][0]["id"], json!("fake"));
    let sessions = wire
        .call(name::SESSION_LIST, json!({ "filter": {} }))
        .await
        .expect("the sessions are listed");
    assert_eq!(sessions["sessions"][0]["id"], json!("ses_1"));
    assert_eq!(wire.finish().await, Exit { code: 0 });
    assert_eq!(
        session
            .pages
            .lock()
            .expect("the recorder is not poisoned")
            .len(),
        1
    );
}

// ---- ordering and resync -------------------------------------------------

#[tokio::test]
async fn the_open_reply_precedes_the_first_event() {
    let (host, _) = TestHost::with(script());
    let mut wire = Wire::started(host).await;
    let id = wire
        .ask(name::SESSION_OPEN, json!({ "selector": selector() }))
        .await;
    let Message::Response(response) = wire.recv().await else {
        panic!("the snapshot is written before any frame of that session");
    };
    assert_eq!(response.id, Some(id));
    let codec::Outcome::Result(result) = response.outcome else {
        panic!("the session opens");
    };
    assert_eq!(result["session"], json!("ses_1"));
    assert_eq!(result["snapshot"]["seq"], json!(0));

    let mut seqs = Vec::new();
    for _ in 0..script().len() {
        let Message::Notification(notification) = wire.recv().await else {
            panic!("the frames follow the snapshot");
        };
        assert_eq!(notification.method, name::EVENT);
        seqs.push(notification.params["seq"].clone());
    }
    assert_eq!(seqs, vec![json!(1), json!(2), json!(3), json!(4)]);
}

#[tokio::test]
async fn a_lagged_frame_travels_like_any_other() {
    let (host, _) = TestHost::with(script());
    let mut wire = Wire::opened(host).await;
    let mut lagged = None;
    for _ in 0..script().len() {
        if let Message::Notification(notification) = wire.recv().await
            && notification.params["event"]["type"] == json!("lagged")
        {
            lagged = Some(notification.params["event"].clone());
        }
    }
    assert_eq!(
        lagged,
        Some(json!({ "type": "lagged", "from": 2, "to": 3 }))
    );
}

#[tokio::test]
async fn events_since_resends_from_that_seq() {
    let (host, _) = TestHost::with(script());
    let mut wire = Wire::opened(host).await;
    // Drain the frames the open forwarder sent.
    for _ in 0..script().len() {
        wire.recv().await;
    }
    wire.call(
        name::SESSION_EVENTS,
        json!({ "session": session_id(), "since": 1 }),
    )
    .await
    .expect("the resync is accepted");
    let mut seqs = Vec::new();
    for _ in 0..2 {
        let Message::Notification(notification) = wire.recv().await else {
            panic!("the replay is a stream of frames");
        };
        seqs.push(notification.params["seq"].clone());
    }
    // The journal replay is durable only, so the `Lagged` marker is not resent.
    assert_eq!(seqs, vec![json!(2), json!(4)]);
}

#[tokio::test]
async fn the_gateway_is_subscribed_once() {
    let (host, _) = TestHost::with(Vec::new());
    let mut wire = Wire::started(host).await;
    wire.call(name::GATEWAY_SUBSCRIBE, json!({}))
        .await
        .expect("the gateway is subscribed");
    let Message::Notification(notification) = wire.recv().await else {
        panic!("a gateway event follows the reply");
    };
    assert_eq!(notification.method, name::GATEWAY_EVENT);
    assert_eq!(notification.params["type"], json!("catalogChanged"));
    // A second subscribe is answered but starts no second forwarder.
    wire.call(name::GATEWAY_SUBSCRIBE, json!({}))
        .await
        .expect("subscribing twice is not an error");
    assert_eq!(wire.finish().await, Exit { code: 0 });
}

// ---- the errors ----------------------------------------------------------

#[tokio::test]
async fn a_line_that_is_not_json_is_a_parse_error() {
    let (host, _) = TestHost::with(script());
    let mut wire = Wire::started(host).await;
    wire.line("{not json").await;
    let response = wire.recv().await;
    assert_eq!(error_of(&response).code, PARSE_ERROR);
    let Message::Response(Response { id, .. }) = &response else {
        panic!("a parse error is a response");
    };
    assert_eq!(*id, None, "an unparsable line carries no id");
    // The server goes on.
    assert_eq!(wire.finish().await, Exit { code: 0 });
}

#[tokio::test]
async fn a_line_that_is_not_json_rpc_is_an_invalid_request() {
    let (host, _) = TestHost::with(script());
    let mut wire = Wire::started(host).await;
    wire.line(r#"{"jsonrpc":"1.0","id":9,"method":"shutdown"}"#)
        .await;
    let response = wire.recv().await;
    assert_eq!(error_of(&response).code, INVALID_REQUEST);
    assert_eq!(wire.finish().await, Exit { code: 0 });
}

#[tokio::test]
async fn an_unknown_method_is_not_found() {
    let (host, _) = TestHost::with(script());
    let mut wire = Wire::started(host).await;
    let error = wire
        .call("session/teleport", json!({}))
        .await
        .expect_err("no such method");
    assert_eq!(error.code, METHOD_NOT_FOUND);
    assert!(error.message.contains("session/teleport"));
    assert_eq!(wire.finish().await, Exit { code: 0 });
}

#[tokio::test]
async fn params_that_do_not_fit_are_invalid_params() {
    let (host, _) = TestHost::with(script());
    let mut wire = Wire::started(host).await;
    let error = wire
        .call(name::SESSION_OPEN, json!({ "selector": "not a selector" }))
        .await
        .expect_err("a string is not a selector");
    assert_eq!(error.code, INVALID_PARAMS);
    assert!(!error.message.is_empty(), "the serde message travels");
    assert_eq!(wire.finish().await, Exit { code: 0 });
}

#[tokio::test]
async fn a_kernel_error_carries_its_stable_code() {
    let host = TestHost::refusing(KernelError::new(ErrorCode::Storage, "the disk is full"));
    let mut wire = Wire::started(host).await;
    let error = wire
        .call(name::SESSION_LIST, json!({ "filter": {} }))
        .await
        .expect_err("the host refused");
    assert_eq!(error.code, KERNEL_ERROR);
    assert_eq!(error.message, "the disk is full");
    assert_eq!(error.data, Some(json!({ "code": "STORAGE" })));
}

#[tokio::test]
async fn a_write_to_a_session_that_is_not_open_is_not_found() {
    let (host, _) = TestHost::with(script());
    let mut wire = Wire::started(host).await;
    let error = wire
        .call(name::SESSION_SUBMIT, params_for(name::SESSION_SUBMIT))
        .await
        .expect_err("nothing is open");
    assert_eq!(error.code, KERNEL_ERROR);
    assert_eq!(error.data, Some(json!({ "code": "SESSION_NOT_FOUND" })));
    assert_eq!(wire.finish().await, Exit { code: 0 });
}

#[tokio::test]
async fn closing_stops_the_forwarder() {
    let (host, _) = TestHost::with(script());
    let mut wire = Wire::opened(host).await;
    wire.call(name::SESSION_CLOSE, json!({ "session": session_id() }))
        .await
        .expect("the session closes");
    let error = wire
        .call(name::SESSION_SUBMIT, params_for(name::SESSION_SUBMIT))
        .await
        .expect_err("the session is no longer open here");
    assert_eq!(error.data, Some(json!({ "code": "SESSION_NOT_FOUND" })));
    assert_eq!(wire.finish().await, Exit { code: 0 });
}

// ---- the remote kernel ---------------------------------------------------

/// The whole point of the wire: a client that folds the frames it is sent ends
/// up with the state the kernel would have handed it directly.
#[tokio::test]
async fn a_remote_kernel_folds_to_the_state_the_host_scripted() {
    let (host, session) = TestHost::with(script());
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (server_reader, server_writer) = tokio::io::split(server);
    let (client_reader, client_writer) = tokio::io::split(client);
    let served = tokio::spawn(serve(host, server_reader, server_writer));
    let kernel = RemoteKernel::connect(client_reader, client_writer);

    let hello = kernel.initialize(who()).await.expect("the handshake");
    assert_eq!(hello.protocol, PROTOCOL);

    let Attachment {
        mut snapshot,
        mut events,
        handle,
        ..
    } = kernel
        .open(selector(), who(), OpenOptions::default())
        .await
        .expect("the session");
    handle.submit(
        IntentId::from_raw("req_remote"),
        Input::text("hi", Origin::surface("test")),
    );

    while snapshot.seq < last_seq()
        && let Some(frame) = events.next().await
    {
        snapshot.apply(&frame);
    }

    let mut want = fresh_state();
    for frame in script() {
        want.apply(&frame);
    }
    assert_eq!(snapshot, want, "the client's fold is the kernel's fold");

    kernel.shutdown().await.expect("shutdown is answered");
    assert_eq!(
        served.await.expect("the server task did not panic"),
        Ok(Exit { code: 0 })
    );
    let submits = session.submits();
    assert_eq!(submits.len(), 1);
    assert_eq!(submits[0].0, IntentId::from_raw("req_remote"));
}

/// A tree attachment's frames are routed by the root they were opened through
/// (ADR-0010 §3): a child's frame reaches the same stream, stamped with its own
/// session, and the wire says which root it belongs to.
#[tokio::test]
async fn a_tree_attachment_routes_a_childs_frames_to_the_root_stream() {
    let (host, _) = TestHost::with(script());
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (server_reader, server_writer) = tokio::io::split(server);
    let (client_reader, client_writer) = tokio::io::split(client);
    let served = tokio::spawn(serve(host, server_reader, server_writer));
    let kernel = RemoteKernel::connect(client_reader, client_writer);
    kernel.initialize(who()).await.expect("the handshake");

    let Attachment { mut events, .. } = kernel
        .open(selector(), who(), OpenOptions::with_children())
        .await
        .expect("the session");
    let mut sessions = Vec::new();
    while let Some(frame) = events.next().await {
        sessions.push(frame.session.clone());
        if frame.session == child_id() {
            break;
        }
    }
    assert_eq!(
        sessions.last(),
        Some(&child_id()),
        "the child's head arrived"
    );
    assert_eq!(
        sessions.iter().filter(|s| **s == session_id()).count(),
        script().len(),
        "after every frame of the root"
    );

    kernel.shutdown().await.expect("shutdown is answered");
    served.await.expect("the server task did not panic").ok();
}

/// `HostApi::catalog` cannot await, so the remote one blocks its worker; that
/// is only sound on a multi-threaded runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_remote_catalog_answers_on_a_multi_threaded_runtime() {
    let (host, _) = TestHost::with(Vec::new());
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (server_reader, server_writer) = tokio::io::split(server);
    let (client_reader, client_writer) = tokio::io::split(client);
    let served = tokio::spawn(serve(host, server_reader, server_writer));
    let kernel = RemoteKernel::connect(client_reader, client_writer);
    kernel.initialize(who()).await.expect("the handshake");

    let asked = tokio::task::spawn_blocking({
        let kernel = kernel.clone();
        move || {
            tokio::runtime::Handle::current()
                .block_on(async { kernel.catalog(CatalogKind::Tools).await })
        }
    })
    .await
    .expect("the blocking task did not panic")
    .expect("the catalogue is read");
    assert_eq!(asked.entries.len(), 1);

    kernel.shutdown().await.expect("shutdown is answered");
    assert_eq!(
        served.await.expect("the server task did not panic"),
        Ok(Exit { code: 0 })
    );
}

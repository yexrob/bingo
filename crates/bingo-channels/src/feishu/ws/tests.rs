use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::adapter::Arrival;
use crate::conversation::Conversation;
use crate::feishu::frame::{Frame, Method, encode, header, kind};

const ME: &str = "ou_bot";

/// One event frame, as the peer sends it.
fn event(id: &str) -> Vec<u8> {
    let payload = json!({
        "schema": "2.0",
        "header": { "event_id": id, "event_type": "im.message.receive_v1" },
        "event": {
            "sender": { "sender_id": { "open_id": "ou_person" } },
            "message": {
                "message_id": "om_1",
                "chat_id": "oc_1",
                "chat_type": "p2p",
                "message_type": "text",
                "content": r#"{"text":"hello"}"#,
                "mentions": [],
            },
        },
    });
    let mut frame = Frame {
        seq_id: 1,
        method: Method::Data,
        payload: payload.to_string().into_bytes(),
        ..Frame::default()
    };
    frame.set_header(header::TYPE, kind::EVENT);
    frame.set_header(header::MESSAGE_ID, id);
    encode(&frame)
}

/// A peer that accepts, says one thing, and hangs up — for as long as
/// anybody keeps dialling it. Answers with the number of accepts so far.
async fn flaky_peer() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("an address");
    let accepts = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&accepts);
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let n = counted.fetch_add(1, Ordering::Relaxed);
            tokio::spawn(async move {
                let Ok(mut peer) = tokio_tungstenite::accept_async(socket).await else {
                    return;
                };
                let _ = peer
                    .send(Message::Binary(event(&format!("evt_{n}")).into()))
                    .await;
                // And then the socket dies, which is what a laptop lid does.
                let _ = peer.close(None).await;
            });
        }
    });
    (address, accepts)
}

/// A peer that refuses the upgrade the way Feishu refuses a bad app.
async fn refusing_peer(status: &'static str, autherr: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
    let address = listener.local_addr().expect("an address");
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let response = format!(
                "HTTP/1.1 {status} Refused\r\nHandshake-Status: {status}\r\n\
                 Handshake-Msg: no\r\nHandshake-Autherrcode: {autherr}\r\n\
                 Content-Length: 0\r\n\r\n"
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
        }
    });
    address
}

/// The bootstrap endpoint, pointing at a socket of our own.
async fn endpoint(address: SocketAddr) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(super::super::bootstrap::ENDPOINT))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0,
            "data": {
                "URL": format!("ws://{address}/callback/ws"),
                // A short nonce so the ladder's first rung is quick; the
                // ladder itself is what the test is about, not the wait.
                "ClientConfig": { "ReconnectNonce": 1, "PingInterval": 120 },
            },
        })))
        .mount(&server)
        .await;
    server
}

fn listening(server: &MockServer) -> (Api, mpsc::Receiver<Arrival>, Inbox) {
    let api = Api::new(server.uri(), "cli_a", "secret");
    let (post, arrivals) = mpsc::channel(8);
    let inbox = Inbox::new("feishu", post);
    (api, arrivals, inbox)
}

#[tokio::test]
async fn a_killed_socket_is_dialled_again_and_the_events_keep_arriving() {
    let (address, accepts) = flaky_peer().await;
    let server = endpoint(address).await;
    let (api, mut arrivals, inbox) = listening(&server);
    let cancel = bingo_sdk::CancellationToken::new();
    let stopping = cancel.clone();
    let listener = tokio::spawn(async move {
        listen(&api, "secret", ME, &inbox, &stopping)
            .await
            .expect("a clean stop");
    });

    for expected in 0..2 {
        let arrival = tokio::time::timeout(Duration::from_secs(10), arrivals.recv())
            .await
            .unwrap_or_else(|_| panic!("no event after reconnect {expected}"))
            .expect("an arrival");
        assert!(matches!(
            arrival.event,
            Incoming::Message { ref conversation, .. } if *conversation == Conversation::direct("oc_1")
        ));
    }
    assert!(
        accepts.load(Ordering::Relaxed) >= 2,
        "the ladder dialled again after the socket died"
    );
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), listener)
        .await
        .expect("the listener stops when it is cancelled")
        .expect("the task");
}

#[tokio::test]
async fn a_forbidden_handshake_stops_the_ladder_rather_than_hammering_it() {
    let address = refusing_peer("403", "0").await;
    let server = endpoint(address).await;
    let (api, _arrivals, inbox) = listening(&server);
    let cancel = bingo_sdk::CancellationToken::new();
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        listen(&api, "secret", ME, &inbox, &cancel),
    )
    .await
    .expect("the ladder gives up rather than retrying for ever")
    .expect_err("a refusal");
    assert!(matches!(error, ChannelError::Refused(_)), "{error}");
    assert!(error.to_string().contains("forbidden"), "{error}");
}

#[tokio::test]
async fn the_connection_limit_is_fatal_however_many_times_it_is_tried() {
    let address = refusing_peer("514", "1000040350").await;
    let server = endpoint(address).await;
    let (api, _arrivals, inbox) = listening(&server);
    let cancel = bingo_sdk::CancellationToken::new();
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        listen(&api, "secret", ME, &inbox, &cancel),
    )
    .await
    .expect("the ladder gives up")
    .expect_err("a refusal");
    assert!(
        error.to_string().contains("as many long connections"),
        "{error}"
    );
}

#[tokio::test]
async fn a_cancelled_listener_stops_without_dialling_again() {
    let (address, accepts) = flaky_peer().await;
    let server = endpoint(address).await;
    let (api, _arrivals, inbox) = listening(&server);
    let cancel = bingo_sdk::CancellationToken::new();
    cancel.cancel();
    listen(&api, "secret", ME, &inbox, &cancel)
        .await
        .expect("a clean stop");
    assert_eq!(accepts.load(Ordering::Relaxed), 0);
}

#[test]
fn a_pong_hot_updates_the_intervals_the_loop_keeps() {
    let mut seen = Seen::default();
    let mut inbound = Inbound::new(ME, &mut seen);
    let mut config = ClientConfig::default();
    let mut pong = Frame {
        method: Method::Control,
        payload: json!({ "PingInterval": 15 }).to_string().into_bytes(),
        ..Frame::default()
    };
    pong.set_header(header::TYPE, kind::PONG);
    let step = inbound.absorb(pong, &mut config);
    assert!(step.reply.is_none() && step.deliver.is_none());
    assert_eq!(config.ping_interval, std::time::Duration::from_secs(15));
    assert_eq!(config.read_deadline(), std::time::Duration::from_secs(35));
}

#[test]
fn a_ping_is_answered_with_a_pong_and_a_card_frame_is_dropped() {
    let mut seen = Seen::default();
    let mut inbound = Inbound::new(ME, &mut seen);
    let mut config = ClientConfig::default();
    let control = |what: &str| {
        let mut frame = Frame {
            seq_id: 3,
            method: Method::Control,
            ..Frame::default()
        };
        frame.set_header(header::TYPE, what);
        frame
    };
    let step = inbound.absorb(control(kind::PING), &mut config);
    let reply = step.reply.expect("a pong");
    assert_eq!(reply.kind(), Some(kind::PONG));
    assert_eq!(reply.seq_id, 3);

    let step = inbound.absorb(control(kind::CARD), &mut config);
    assert!(step.reply.is_none(), "a stale card frame is noise");
}

#[test]
fn an_event_is_acked_once_and_a_redelivery_is_acked_but_not_repeated() {
    let mut seen = Seen::default();
    let mut inbound = Inbound::new(ME, &mut seen);
    let mut config = ClientConfig::default();
    let frame = || frame::decode(&event("evt_1")).expect("a frame");

    let first = inbound.absorb(frame(), &mut config);
    assert!(first.deliver.is_some());
    assert!(
        first
            .reply
            .expect("an ack")
            .header(header::BIZ_RT)
            .is_some(),
        "every event is acked within three seconds"
    );

    let again = inbound.absorb(frame(), &mut config);
    assert!(
        again.reply.is_some(),
        "a redelivery is still acked, or the peer keeps trying"
    );
    assert!(again.deliver.is_none(), "but it is handled once");
}

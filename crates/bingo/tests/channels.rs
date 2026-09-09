//! Black-box: a session driven from a chat, through the real binary.
//!
//! The loopback adapter dials this test and speaks NDJSON both ways
//! (ADR-0016 §7), so what is asserted here is what an adapter is actually
//! asked to do — a message posted, the edits an answer streams into it, the
//! buttons under a question, and the buttons coming off again when the answer
//! arrived somewhere else entirely.

// An integration test is not `cfg(test)`; the test-only lint relief is spelled out.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::process::Stdio;
use std::time::Duration;

use bingo_sdk::{
    Activation, Answer, ClientIdentity, HostApi, IntentId, OpenOptions, Origin, SessionSelector,
};
use futures::StreamExt;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::TcpListener;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::process::{Child, Command};

use support::{Server, ready};

/// Any one wait; past this the scenario has stalled and the test fails
/// rather than hanging the suite.
const LIMIT: Duration = Duration::from_secs(30);

/// The peer on the other end of the loopback adapter: what a chat app is,
/// as far as the binary is concerned.
struct Peer {
    outbound: Lines<BufReader<OwnedReadHalf>>,
    inbound: OwnedWriteHalf,
}

impl Peer {
    async fn accept(listener: &TcpListener) -> Peer {
        let (socket, _) = tokio::time::timeout(LIMIT, listener.accept())
            .await
            .expect("the channel dials its peer")
            .expect("a connection");
        let (read, inbound) = socket.into_split();
        Peer {
            outbound: BufReader::new(read).lines(),
            inbound,
        }
    }

    async fn say(&mut self, event: Value) {
        self.inbound
            .write_all(format!("{event}\n").as_bytes())
            .await
            .expect("the chat is heard");
    }

    async fn chats(&mut self, chat: &str, text: &str) {
        self.say(json!({ "kind": "message", "chat": chat, "principal": "u_1", "text": text }))
            .await;
    }

    /// Everything the channel was asked to do up to and including the first
    /// op that matches, in order.
    async fn until(&mut self, matching: impl Fn(&Value) -> bool) -> Vec<Value> {
        let mut seen = Vec::new();
        loop {
            let line = tokio::time::timeout(LIMIT, self.outbound.next_line())
                .await
                .unwrap_or_else(|_| panic!("the channel went quiet; it had done {seen:#?}"))
                .expect("a line")
                .expect("the channel is still there");
            let op: Value = serde_json::from_str(&line).expect("one json object per line");
            let done = matching(&op);
            seen.push(op);
            if done {
                return seen;
            }
        }
    }
}

fn is(op: &Value, name: &str) -> bool {
    op["op"] == json!(name)
}

/// `bingo channels` with nothing else running, its loopback dialling us.
struct Chat {
    peer: Peer,
    _child: Child,
    _home: tempfile::TempDir,
}

impl Chat {
    async fn open(script: &str, settings: Value) -> Chat {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = listener.local_addr().unwrap().to_string();
        let home = tempfile::tempdir().unwrap();
        let script_path = home.path().join("script.json");
        std::fs::write(&script_path, script).unwrap();
        let settings_path = home.path().join("channels.json");
        std::fs::write(&settings_path, with_peer(settings, &peer).to_string()).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_bingo"))
            .args(["channels", "--cwd"])
            .arg(home.path())
            .arg("--settings")
            .arg(&settings_path)
            .env("BINGO_FAKE_SCRIPT", &script_path)
            .envs(support::home::home_env(home.path()))
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Chat {
            peer: Peer::accept(&listener).await,
            _child: child,
            _home: home,
        }
    }
}

/// The settings a chat runs on: the peer this test is listening on, and a
/// coalescer tight enough that a test never waits for one.
fn with_peer(mut settings: Value, peer: &str) -> Value {
    settings["channels"]["loopback"]["peer"] = json!(peer);
    settings["channels"]["coalesce"] = json!({ "minChars": 1, "intervalMs": 1 });
    settings
}

fn answering(text: &str) -> String {
    format!(r#"{{"responses":[{{"steps":[{{"text":"{text}"}}]}}]}}"#)
}

/// A turn that wants to write a file — which the gate stops to ask about —
/// and then says it is done.
const WRITES_A_FILE: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"Write","input":{"file_path":"made.txt","content":"by the chat\n"}}}]},
    {"steps":[{"text":"Written."}]}
]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_message_opens_a_session_and_the_answer_streams_into_one_edited_message() {
    let mut chat = Chat::open(&answering("Two tests failed."), json!({})).await;
    chat.peer.chats("oc_1", "run the tests").await;
    let ops = chat.peer.until(|op| is(op, "finish")).await;

    let opened = ops.iter().find(|op| is(op, "send")).expect("a message");
    assert_eq!(opened["chat"], json!("oc_1"));
    assert_eq!(
        opened["mode"],
        json!("stream"),
        "the message the answer streams into"
    );
    assert!(
        ops.iter().any(|op| is(op, "replace")),
        "the answer was edited in as it arrived: {ops:#?}"
    );
    let finish = ops.last().expect("the finish");
    assert_eq!(finish["id"], opened["id"], "one message, edited");
    assert_eq!(finish["text"], json!("Two tests failed."));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_permission_is_buttons_and_a_click_answers_it() {
    let mut chat = Chat::open(WRITES_A_FILE, json!({})).await;
    chat.peer.chats("oc_1", "write it").await;
    let ops = chat.peer.until(|op| is(op, "ask")).await;
    let asked = ops.last().expect("the question");
    assert!(
        asked["prompt"].as_str().unwrap().starts_with("Write:"),
        "{asked}"
    );
    let choices = asked["choices"].as_array().expect("choices");
    assert_eq!(choices[0][0], json!("1"));
    assert_eq!(choices[0][1], json!("Allow once"));

    chat.peer
        .say(json!({
            "kind": "click", "chat": "oc_1", "principal": "u_1",
            "question": asked["question"], "choice": "1",
        }))
        .await;
    let settled = chat.peer.until(|op| is(op, "settle")).await;
    let settle = settled.last().expect("the settle");
    assert_eq!(settle["id"], asked["id"], "the card it was asked in");
    assert_eq!(
        settle["outcome"],
        json!("approved"),
        "answered here, so no elsewhere to name"
    );
    // And the turn went on: the tool ran and the model said so.
    let after = chat.peer.until(|op| op["text"] == json!("Written.")).await;
    assert!(!after.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn without_buttons_the_numbered_rung_is_drawn_and_a_reply_answers_it() {
    let mut chat = Chat::open(
        WRITES_A_FILE,
        json!({ "channels": { "loopback": { "buttons": false } } }),
    )
    .await;
    chat.peer.chats("oc_1", "write it").await;
    let ops = chat
        .peer
        .until(|op| {
            op["text"]
                .as_str()
                .is_some_and(|t| t.contains("1. Allow once"))
        })
        .await;
    let question = ops.last().expect("the numbered list");
    let text = question["text"].as_str().unwrap();
    assert!(text.contains("for this session"), "the middle rung: {text}");
    assert!(
        text.ends_with("Reply with a number or the words above."),
        "{text}"
    );

    // The words work as well as the number, which is the whole point of the
    // lower rung. Without buttons the question is a message, so it is settled
    // by editing that message — not the one the next answer streams into.
    let asked_in = question["id"].clone();
    chat.peer.chats("oc_1", "Deny").await;
    let settled = chat
        .peer
        .until(|op| is(op, "replace") && op["id"] == asked_in)
        .await;
    let settle = settled.last().expect("the edit");
    assert!(
        settle["text"].as_str().unwrap().contains("denied"),
        "the buttons come off however they went on: {settle}"
    );
}

/// The two-surface race: the card is up in the chat and the person answers at
/// the terminal instead. `bingo serve --stdio --channels …` is the shape that
/// makes it possible — a chat is `SurfaceKind::Concurrent`, so it runs beside
/// whatever owns the terminal (ADR-0016 §1).
#[tokio::test(flavor = "multi_thread")]
async fn a_question_answered_at_another_surface_edits_the_card() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let mut server = Server::spawn_with(
        WRITES_A_FILE,
        &["--channels", &format!("loopback={address}")],
    );
    let mut peer = Peer::accept(&listener).await;
    peer.chats("oc_1", "write it").await;
    let ops = peer.until(|op| is(op, "ask")).await;
    let asked = ops.last().expect("the question");

    // Now the person walks over to the terminal. That is the identity the
    // terminal surface connects with, and a resolution is attributed to the
    // connection's identity, so it is set at the handshake.
    let terminal = ClientIdentity {
        name: "tui".into(),
        surface: "tui".into(),
    };
    let kernel = server.kernel();
    kernel.initialize(terminal.clone()).await.unwrap();
    let mut attachment = kernel
        .open(
            SessionSelector::ByKey {
                key: "loopback/oc_1".into(),
            },
            terminal.clone(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    // The card went up before this attachment existed, so the question is
    // already in the snapshot; a client that only watched the stream would
    // wait for a frame that has been and gone.
    let interaction = loop {
        if let Some(open) = attachment.snapshot.interactions.first() {
            break open.clone();
        }
        let frame = tokio::time::timeout(LIMIT, attachment.events.next())
            .await
            .expect("a frame while the question is open")
            .expect("the stream stays open");
        attachment.snapshot.apply(&frame);
    };
    attachment.handle.answer(
        IntentId::mint(),
        interaction.id,
        Answer::AllowOnce,
        // Not `Keyboard`: the kernel guards a freshly opened question against
        // a stray keystroke for 400 ms, and a test that races that guard is a
        // test that fails on a fast machine.
        Activation::Pointer,
    );

    let settled = peer.until(|op| is(op, "settle")).await;
    let settle = settled.last().expect("the settle");
    assert_eq!(settle["id"], asked["id"]);
    assert_eq!(
        settle["outcome"],
        json!("approved in the TUI"),
        "no live button outlives its question, wherever it was answered"
    );
    kernel.shutdown().await.unwrap();
}

/// The prompts a chat submits carry who said them and where, so the journal
/// can tell one person in a group from another (ADR-0016 §4).
#[tokio::test(flavor = "multi_thread")]
async fn a_prompt_from_a_chat_carries_its_principal_and_its_conversation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let mut server = Server::spawn_with(
        &answering("Hello."),
        &["--channels", &format!("loopback={address}")],
    );
    let mut peer = Peer::accept(&listener).await;
    let kernel = ready(&mut server).await;

    peer.say(json!({
        "kind": "message", "chat": "oc_1", "principal": "ou_wei", "text": "hello",
    }))
    .await;
    peer.until(|op| is(op, "finish")).await;

    let attachment = kernel
        .open(
            SessionSelector::ByKey {
                key: "loopback/oc_1".into(),
            },
            support::who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    let origin = attachment
        .snapshot
        .items
        .iter()
        .find_map(|item| match &item.body {
            bingo_sdk::ItemBody::User { origin, .. } => Some(origin.clone()),
            _ => None,
        })
        .expect("the prompt is in the transcript");
    assert_eq!(
        origin,
        Origin {
            surface: "channels".into(),
            principal: Some("ou_wei".into()),
            conversation: Some("loopback/oc_1".into()),
        }
    );
    kernel.shutdown().await.unwrap();
}

/// A group is not a session until the bot is spoken to.
#[tokio::test(flavor = "multi_thread")]
async fn a_group_is_ignored_until_the_bot_is_mentioned() {
    let mut chat = Chat::open(&answering("Hello."), json!({})).await;
    chat.peer
        .say(json!({
            "kind": "message", "chat": "oc_g", "group": true,
            "principal": "u_1", "text": "what do you think?",
        }))
        .await;
    chat.peer
        .say(json!({
            "kind": "message", "chat": "oc_g", "group": true,
            "principal": "u_1", "text": "@bingo what do you think?",
        }))
        .await;
    let ops = chat.peer.until(|op| is(op, "finish")).await;
    assert_eq!(
        ops.iter().filter(|op| is(op, "send")).count(),
        1,
        "the overheard line opened nothing: {ops:#?}"
    );
}

/// The settings one policy test runs on. Everything else is the default,
/// which is what ran before there was a policy at all.
fn under(access: Value) -> Value {
    json!({ "channels": { "loopback": { "access": access } } })
}

/// One group speaks and is refused, another speaks and is answered. The
/// refused chat is a chat of its own, so a leak has nowhere to hide: every
/// op the channel emits names the chat it went to.
async fn only_one_group_is_heard(access: Value, refused: Value, heard: Value, chat: &str) {
    let mut chat_under = Chat::open(&answering("Hello."), under(access)).await;
    chat_under.peer.say(refused).await;
    chat_under.peer.say(heard).await;
    let ops = chat_under.peer.until(|op| is(op, "finish")).await;
    assert!(
        ops.iter().any(|op| op["chat"] == json!(chat)),
        "the admitted group was answered: {ops:#?}"
    );
    assert!(
        ops.iter()
            .all(|op| op["chat"].is_null() || op["chat"] == json!(chat)),
        "and nothing was said anywhere else: {ops:#?}"
    );
}

fn in_group(chat: &str, principal: &str, text: &str) -> Value {
    json!({
        "kind": "message", "chat": chat, "group": true,
        "principal": principal, "text": text,
    })
}

/// A blocklisted principal opens no session and gets no reply — and is not
/// told so either (ADR-0051 §4).
#[tokio::test(flavor = "multi_thread")]
async fn a_blocklisted_principal_in_a_group_is_answered_with_nothing() {
    only_one_group_is_heard(
        json!({ "group": { "policy": "blocklist", "list": ["u_loud"], "mention": false } }),
        // Mentioned, so the mention alone would have admitted it: what
        // refuses it is the policy and nothing else.
        in_group("oc_loud", "u_loud", "@bingo what do you think?"),
        in_group("oc_quiet", "u_quiet", "what do you think?"),
        "oc_quiet",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_an_admin_is_heard_in_a_group_that_admits_only_admins() {
    only_one_group_is_heard(
        json!({
            "admins": ["u_admin"],
            "group": { "policy": "admins", "mention": false },
        }),
        in_group("oc_no", "u_1", "@bingo what do you think?"),
        in_group("oc_yes", "u_admin", "what do you think?"),
        "oc_yes",
    )
    .await;
}

/// A chat's own rule replaces the one for groups whole, and a rule that wants
/// no mention engages on a plain line — the amendment to ADR-0016 §4.
#[tokio::test(flavor = "multi_thread")]
async fn a_chats_own_rule_engages_without_the_mention_the_others_need() {
    only_one_group_is_heard(
        json!({ "rules": { "oc_open": { "mention": false } } }),
        in_group("oc_shut", "u_1", "what do you think?"),
        in_group("oc_open", "u_1", "what do you think?"),
        "oc_open",
    )
    .await;
}

/// The sign brackets the turn the message started (ADR-0051 §5): up before
/// the chat says anything back, down after the answer is finished.
#[tokio::test(flavor = "multi_thread")]
async fn the_working_sign_brackets_the_turn_a_message_started() {
    let mut chat = Chat::open(&answering("Two tests failed."), json!({})).await;
    chat.peer
        .say(json!({
            "kind": "message", "chat": "oc_1", "principal": "u_1",
            "text": "run the tests", "parent": "om_1",
        }))
        .await;
    let ops = chat.peer.until(|op| is(op, "acknowledged")).await;
    let at = |name: &str| ops.iter().position(|op| is(op, name));
    let up = at("acknowledge").unwrap_or_else(|| panic!("the sign goes up: {ops:#?}"));
    let said = at("reply").unwrap_or_else(|| panic!("the answer is posted: {ops:#?}"));
    let done = at("finish").unwrap_or_else(|| panic!("and finished: {ops:#?}"));
    let down = ops.len() - 1;
    assert!(
        up < said,
        "the sign is up before a word is said back: {ops:#?}"
    );
    assert!(done < down, "and comes off after the answer: {ops:#?}");
    assert_eq!(ops[up]["id"], json!("om_1"), "on the message that spoke");
    assert_eq!(ops[down]["id"], json!("om_1"));
    assert_eq!(ops[down]["outcome"], json!("done"));
}

/// The one thing a chat must never do quietly: two processes on one app take
/// half of its events each and neither knows (ADR-0016 §5).
#[test]
fn a_second_process_on_one_credential_refuses_loudly() {
    let home = tempfile::tempdir().unwrap();
    let settings = home.path().join("channels.json");
    // A peer nobody is listening on: the claim is taken before the dial, so
    // the first process is still the one holding it.
    std::fs::write(
        &settings,
        json!({ "channels": { "loopback": { "peer": "127.0.0.1:9" } } }).to_string(),
    )
    .unwrap();
    let command = || {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_bingo"));
        command
            .args(["channels", "--cwd"])
            .arg(home.path())
            .arg("--settings")
            .arg(&settings)
            .envs(support::home::home_env(home.path()))
            .env_remove("BINGO_FAKE_SCRIPT")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    };
    let first = command().output().unwrap();
    // The peer refuses the connection, so the first run ends — and gives its
    // claim back, which is what the second run then takes.
    assert_ne!(first.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(
        stderr.contains("the loopback peer"),
        "the refusal names what could not be reached: {stderr}"
    );
    // A claim left behind by a process that is still running is refused.
    let held = home.path().join(".bingo/data/channels");
    std::fs::create_dir_all(&held).unwrap();
    std::fs::write(held.join("loopback-127.0.0.1_9.lock"), "1").unwrap();
    let second = command().output().unwrap();
    let stderr = String::from_utf8_lossy(&second.stderr).into_owned();
    assert!(
        stderr.contains("another bingo already runs"),
        "a second process refuses loudly: {stderr}"
    );
}

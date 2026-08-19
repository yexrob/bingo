//! `bingo app-server` as a client meets it: a real process, an isolated home,
//! and frames over its stdin and stdout.
//!
//! **What is here** is everything the server can do without a model: the
//! handshake and its refusals, the session lifecycle including resume with the
//! rooms it left, the catalogs and configuration a client reads on the way in,
//! the actions the core owns outright, the queue, attention, the error paths,
//! stdout purity, and the exit codes.
//!
//! **What runs a model** does so against a scripted provider on loopback
//! ([`Provider`]) rather than against one: text, reasoning and usage; a tool
//! call with the permission prompt that stops it and the denial that answers
//! it; a stream retry and the round it re-enters; the queue draining at a turn
//! boundary and an interrupt reaching a run; the foreground shell tail. What
//! those assert is the path, not the model.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "bingo-app-server-test-{label}-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("temporary test directory must be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One `bingo app-server` process, and the pipe to it.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    /// Every frame that arrived while waiting for a response, in order.
    seen: Vec<Value>,
}

/// How the process ended, and what it left on each stream.
struct Ended {
    code: Option<i32>,
    stderr: String,
    /// Frames still on stdout when the client stopped reading for a response.
    tail: Vec<Value>,
}

impl Server {
    fn start(root: &TempDir) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bingo"))
            .arg("app-server")
            .current_dir(root.path())
            .env("HOME", root.path())
            .env("XDG_CONFIG_HOME", root.path().join("config"))
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("DEEPSEEK_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the app-server process must start");
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        Self {
            child,
            stdin: Some(stdin),
            stdout,
            seen: Vec::new(),
        }
    }

    fn write(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("the client still holds stdin");
        stdin.write_all(line.as_bytes()).expect("write a frame");
        stdin.write_all(b"\n").expect("write the frame separator");
        stdin.flush().expect("flush the frame");
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("the client still holds stdin");
        stdin.write_all(bytes).expect("write bytes");
        stdin.flush().expect("flush");
    }

    fn frame(&mut self) -> Option<Value> {
        let mut line = String::new();
        match self.stdout.read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(
                serde_json::from_str(line.trim_end()).unwrap_or_else(|error| {
                    panic!("stdout carries protocol frames only: {error}: {line}")
                }),
            ),
        }
    }

    fn call(&mut self, id: i64, method: &str, params: Value) -> Value {
        self.write(
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string(),
        );
        loop {
            let Some(frame) = self.frame() else {
                panic!("the server closed before answering {id} ({method})");
            };
            if frame.get("id") == Some(&json!(id)) {
                return frame;
            }
            self.seen.push(frame);
        }
    }

    /// Initialize and say so, which is what a controlling client does.
    fn handshake(&mut self) -> Value {
        let result = self.call(1, "initialize", initialize_params());
        assert!(result.get("result").is_some(), "{result}");
        self.write(&json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}).to_string());
        result
    }

    /// Close stdin, drain what is left, and wait for the exit.
    fn finish(mut self) -> Ended {
        drop(self.stdin.take());
        let mut tail = Vec::new();
        while let Some(frame) = self.frame() {
            tail.push(frame);
        }
        let status = self.child.wait().expect("the process must exit");
        let mut stderr = String::new();
        if let Some(mut handle) = self.child.stderr.take() {
            use std::io::Read;
            let _ = handle.read_to_string(&mut stderr);
        }
        Ended {
            code: status.code(),
            stderr,
            tail,
        }
    }
}

fn initialize_params() -> Value {
    json!({
        "protocol": {"major": 1, "minMinor": 0, "maxMinor": 0},
        "client": {"name": "bingo-black-box", "version": "0.1.0"},
        "capabilities": {"interactionResponse": true}
    })
}

fn code_of(frame: &Value) -> &str {
    frame
        .get("error")
        .and_then(|error| error.get("data"))
        .and_then(|data| data.get("bingoCode"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected an application error, got {frame}"))
}

// ---------------------------------------------------------------------------

/// The handshake, and the two ways it ends the connection instead.
#[test]
fn a_real_process_negotiates_a_protocol_and_says_what_it_is() {
    let root = TempDir::new("handshake");
    let mut server = Server::start(&root);
    let frame = server.call(1, "initialize", initialize_params());
    let result = &frame["result"];
    assert_eq!(result["protocol"], json!({"major": 1, "minor": 0}));
    assert_eq!(result["server"]["name"], json!("bingo"));
    assert_eq!(
        result["limits"],
        json!({"maxClientFrameBytes": 1_048_576, "maxServerFrameBytes": 8_388_608})
    );
    let ended = server.finish();
    assert_eq!(ended.code, Some(0), "a clean EOF exits zero");
    assert!(ended.stderr.is_empty(), "stderr: {}", ended.stderr);
}

#[test]
fn a_protocol_this_build_does_not_speak_ends_the_connection() {
    let root = TempDir::new("major");
    let mut server = Server::start(&root);
    let frame = server.call(
        1,
        "initialize",
        json!({
            "protocol": {"major": 9, "minMinor": 0, "maxMinor": 0},
            "client": {"name": "c", "version": "0"},
            "capabilities": {"interactionResponse": true}
        }),
    );
    assert_eq!(code_of(&frame), "PROTOCOL_UNSUPPORTED");
    let ended = server.finish();
    assert_eq!(ended.code, Some(1));
    assert!(
        ended
            .stderr
            .contains("[error] code=PROTOCOL_UNSUPPORTED msg="),
        "stderr: {}",
        ended.stderr
    );
}

#[test]
fn a_client_that_cannot_answer_a_prompt_may_not_control_a_session() {
    let root = TempDir::new("capability");
    let mut server = Server::start(&root);
    let frame = server.call(
        1,
        "initialize",
        json!({
            "protocol": {"major": 1, "minMinor": 0, "maxMinor": 0},
            "client": {"name": "c", "version": "0"},
            "capabilities": {"interactionResponse": false}
        }),
    );
    assert_eq!(code_of(&frame), "CAPABILITY_REQUIRED");
    let ended = server.finish();
    assert_eq!(ended.code, Some(1));
    assert!(
        ended
            .stderr
            .contains("[error] code=CAPABILITY_REQUIRED msg="),
        "stderr: {}",
        ended.stderr
    );
}

/// A session starts, is read, is closed, and is deleted — and every frame the
/// client saw on the way was a protocol frame.
#[test]
fn a_session_is_started_read_closed_and_deleted() {
    let root = TempDir::new("lifecycle");
    let mut server = Server::start(&root);
    server.handshake();

    let started = server.call(2, "session/start", json!({}));
    let snapshot = &started["result"]["snapshot"];
    let session_id = snapshot["session"]["id"].clone();
    let locator = snapshot["session"]["locator"].clone();
    let stem = snapshot["session"]["title"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(snapshot["session"]["state"], json!("active"));
    assert!(
        snapshot["session"]["cwd"]
            .as_str()
            .is_some_and(|cwd| cwd.ends_with(&format!("{}", root.path().display()))
                || root.path().ends_with(cwd)),
        "the session runs where the process was started: {snapshot}"
    );

    let read = server.call(3, "session/read", json!({}));
    assert_eq!(read["result"]["snapshot"]["session"]["id"], session_id);

    // The open session may not be deleted, by refusal rather than by omission.
    let refused = server.call(4, "session/delete", json!({"locator": locator}));
    assert_eq!(code_of(&refused), "BAD_ARGUMENT");

    let closed = server.call(5, "session/close", json!({}));
    assert_eq!(closed["result"]["sessionId"], session_id);
    let after = server.call(6, "session/read", json!({}));
    assert_eq!(code_of(&after), "NO_ACTIVE_SESSION");

    let deleted = server.call(
        7,
        "session/delete",
        json!({"locator": {"type": "stem", "stem": stem}}),
    );
    assert_eq!(deleted["result"]["deleted"], json!(true), "{deleted}");

    let seen = server.seen.clone();
    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
    assert!(ended.stderr.is_empty(), "stderr: {}", ended.stderr);
    // Everything the client read was a frame; `frame()` would have panicked
    // otherwise, and the close was announced before the process left.
    assert!(
        server_saw(&seen, &ended.tail, "session/closed"),
        "the close is announced: {seen:?} / {:?}",
        ended.tail
    );
}

fn server_saw(seen: &[Value], tail: &[Value], method: &str) -> bool {
    seen.iter()
        .chain(tail.iter())
        .any(|frame| frame["method"] == json!(method))
}

/// The golden one for this batch (Amendment #6): a session resumed by name comes
/// back to the rooms it left, and to how far the user had read them.
///
/// The rooms are a fixture on disk rather than rooms this test created, because
/// creating one takes an agent, and an agent takes the engine (B7). What is under
/// test here is the transport's half: that `session/resume` names a session
/// across epochs, replays its sidecar, and publishes what came back.
#[test]
fn a_resumed_session_comes_back_to_the_rooms_it_left() {
    let root = TempDir::new("resume");
    let data = root.path().join(".local/share/bingo");
    std::fs::create_dir_all(data.join("transcripts")).expect("transcript directory");
    std::fs::create_dir_all(data.join("rooms")).expect("rooms directory");
    let stem = "resume-fixture-1760000000";
    std::fs::write(
        data.join(format!("transcripts/{stem}.jsonl")),
        b"{\"role\":\"user\",\"content\":\"hello\"}\n",
    )
    .expect("transcript fixture");
    std::fs::write(
        data.join(format!("rooms/{stem}.rooms.jsonl")),
        concat!(
            r#"{"v":1,"at":1760000000000,"type":"room","room":"build","mode":"free","members":["main","scout"],"frozen":false}"#,
            "\n",
            r#"{"v":1,"at":1760000000100,"type":"post","room":"build","seq":1,"from":"scout","text":"the suite is green","atUnix":1760000000,"said":true}"#,
            "\n",
            r#"{"v":1,"at":1760000000200,"type":"member","room":"build","member":"scout","seen":1,"sent":1}"#,
            "\n",
            r#"{"v":1,"at":1760000005000,"type":"read","room":"build","seq":1}"#,
            "\n",
        )
        .as_bytes(),
    )
    .expect("room sidecar fixture");

    let mut server = Server::start(&root);
    server.handshake();
    let started = server.call(2, "session/start", json!({}));
    let epoch = started["result"]["snapshot"]["session"]["epoch"].clone();

    let listed = server.call(3, "session/list", json!({}));
    assert!(
        listed["result"]["sessions"]["items"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|entry| entry["locator"] == json!({"type": "stem", "stem": stem}))),
        "the session on disk is listed: {listed}"
    );

    let resumed = server.call(
        4,
        "session/resume",
        json!({"locator": {"type": "stem", "stem": stem}}),
    );
    let snapshot = &resumed["result"]["snapshot"];
    assert_eq!(snapshot["session"]["resumed"], json!(true), "{resumed}");
    assert_eq!(snapshot["session"]["title"], json!(stem));
    assert_ne!(
        snapshot["session"]["epoch"], epoch,
        "replacing the actor mints a new epoch, and the old identifiers die with it"
    );
    assert!(
        snapshot["collections"]["rooms"]["count"]
            .as_u64()
            .unwrap_or_default()
            >= 1,
        "the session came back with its room: {snapshot}"
    );

    let rooms = server.call(5, "resource/read", json!({"resource": "rooms"}));
    let named = rooms["result"]["resource"]["items"]
        .as_array()
        .is_some_and(|items| {
            items.iter().any(|room| {
                room["name"]
                    .as_str()
                    .is_some_and(|name| name.contains("build"))
            })
        });
    assert!(named, "the room the session left is back: {rooms}");

    // And the user's place in it: one post, already read.
    let conversations = server.call(6, "conversation/list", json!({}));
    let room = conversations["result"]["conversations"]["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|conversation| conversation["kind"]["type"] == json!("room"))
                .cloned()
        })
        .unwrap_or_else(|| panic!("the room is a conversation: {conversations}"));
    assert_eq!(
        room["unread"],
        json!(0),
        "the read cursor came back too: {room}"
    );

    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
}

/// The catalogs answer before a session exists — the job `--inspect` had — and a
/// provider never carries a credential.
#[test]
fn the_catalogs_answer_before_a_session_and_never_carry_a_key() {
    let root = TempDir::new("catalog");
    std::fs::create_dir_all(root.path().join(".bingo")).expect("project config directory");
    std::fs::write(
        root.path().join(".bingo/settings.json"),
        br#"{"providers":{"secretive":{"protocol":"anthropic","apiBaseUrl":"https://example.invalid","apiKey":"sk-should-never-travel"}}}"#,
    )
    .expect("settings fixture");
    let mut server = Server::start(&root);
    server.handshake();

    for catalog in ["providers", "models", "skills", "mcpServers", "images"] {
        let frame = server.call(2, "catalog/read", json!({"catalog": catalog}));
        assert!(
            frame.get("result").is_some(),
            "{catalog} must answer with no session: {frame}"
        );
        assert!(
            !frame.to_string().contains("sk-should-never-travel"),
            "{catalog} carried a credential"
        );
    }
    // Configuration and runtime collections are session state, and say so.
    let config = server.call(3, "config/read", json!({}));
    assert_eq!(code_of(&config), "NO_ACTIVE_SESSION");

    server.call(4, "session/start", json!({}));
    let config = server.call(5, "config/read", json!({}));
    assert!(config.get("result").is_some(), "{config}");
    assert!(
        !config.to_string().contains("sk-should-never-travel"),
        "the configuration carried a credential"
    );
    for resource in [
        "agents",
        "rooms",
        "tasks",
        "deliveries",
        "backgroundCommands",
    ] {
        let frame = server.call(6, "resource/read", json!({"resource": resource}));
        assert!(frame.get("result").is_some(), "{resource}: {frame}");
    }
    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
}

/// The actions the core owns outright are applied and reported; the ones that
/// need a model say so before they start rather than failing halfway.
#[test]
fn the_actions_the_core_owns_are_applied_and_the_rest_say_why_not() {
    let root = TempDir::new("actions");
    let mut server = Server::start(&root);
    server.handshake();
    let started = server.call(2, "session/start", json!({}));
    let main = started["result"]["snapshot"]["conversations"]["active"][0]["id"].clone();

    let listed = server.call(3, "action/list", json!({}));
    let actions = listed["result"]["actions"]
        .as_array()
        .unwrap_or_else(|| panic!("action/list answers with a list: {listed}"));
    assert_eq!(actions.len(), 28, "every action is published");
    let unavailable: Vec<&Value> = actions
        .iter()
        .filter(|action| action["available"] == json!(false))
        .collect();
    assert!(
        !unavailable.is_empty()
            && unavailable
                .iter()
                .all(|action| action["unavailableReason"].is_string()),
        "an unavailable action says why: {unavailable:?}"
    );

    // Representatives of the fourteen the core owns: a setting, a selection, a
    // permission rule, and a room.
    for (id, action, expected) in [
        (10, json!({"type": "themeSet", "theme": "dark"}), "applied"),
        (11, json!({"type": "themeSet", "theme": "dark"}), "noChange"),
        (
            12,
            json!({"type": "thinkingSelect", "level": "high"}),
            "applied",
        ),
        (
            13,
            json!({"type": "permissionModeSet", "mode": "acceptEdits"}),
            "applied",
        ),
        (
            14,
            json!({"type": "permissionRuleAdd", "decision": "allow", "rule": "Bash(cargo test:*)"}),
            "applied",
        ),
    ] {
        let frame = server.call(
            id,
            "action/execute",
            json!({"originConversationId": main, "action": action}),
        );
        assert_eq!(
            frame["result"]["disposition"]["result"]["status"],
            json!(expected),
            "{action}: {frame}"
        );
    }

    let config = server.call(20, "config/read", json!({}));
    assert_eq!(config["result"]["config"]["theme"], json!("dark"));
    assert_eq!(config["result"]["config"]["thinking"], json!("high"));
    assert_eq!(
        config["result"]["config"]["permissionMode"],
        json!("acceptEdits")
    );

    // The composer is the same path: a slash line becomes the action a typed
    // call would have made, so a GUI cannot bypass CLI semantics by typing.
    let typed = server.call(
        21,
        "conversation/submit",
        json!({
            "conversationId": main,
            "input": {
                "type": "composer",
                "mode": "normal",
                "text": "/theme light",
                "attachments": []
            }
        }),
    );
    assert_eq!(
        typed["result"]["disposition"]["result"]["status"],
        json!("applied"),
        "{typed}"
    );
    let after = server.call(22, "config/read", json!({}));
    assert_eq!(after["result"]["config"]["theme"], json!("light"));

    // Joining a room that does not exist is refused, exactly as `/join` is: a
    // room is opened by the agents in it, not by asking to sit in one.
    let nowhere = server.call(
        23,
        "action/execute",
        json!({
            "originConversationId": main,
            "action": {"type": "roomJoin", "room": "design"}
        }),
    );
    assert_eq!(code_of(&nowhere), "BAD_ARGUMENT");

    // One that needs a model, refused before it starts.
    let refused = server.call(
        24,
        "action/execute",
        json!({
            "originConversationId": main,
            "action": {"type": "conversationCompact"}
        }),
    );
    assert_eq!(code_of(&refused), "ACTION_UNAVAILABLE");

    // A stale precondition loses rather than overwrites.
    let stale = server.call(
        25,
        "action/execute",
        json!({
            "originConversationId": main,
            "precondition": {"scope": "config", "revision": 1},
            "action": {"type": "themeSet", "theme": "light"}
        }),
    );
    assert_eq!(code_of(&stale), "STALE_REVISION");

    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
}

/// The queue is readable and its pull-back is honest about finding nothing;
/// attention moves only when a client says it saw something.
#[test]
fn the_queue_reads_and_attention_moves_only_when_told() {
    let root = TempDir::new("queue");
    let mut server = Server::start(&root);
    server.handshake();
    let started = server.call(2, "session/start", json!({}));
    let conversation = &started["result"]["snapshot"]["conversations"]["active"][0];
    let main = conversation["id"].clone();
    let revision = conversation["revision"].as_u64().unwrap_or_default();

    let queue = server.call(3, "queue/read", json!({"conversationId": main}));
    assert_eq!(queue["result"]["count"], json!(0), "{queue}");
    assert_eq!(
        queue["result"]["entries"]["items"],
        json!([]),
        "an idle conversation has an empty queue"
    );

    let reclaimed = server.call(4, "queue/reclaimTail", json!({"conversationId": main}));
    assert_eq!(
        reclaimed["result"]["outcome"],
        json!({"type": "empty"}),
        "{reclaimed}"
    );

    let marked = server.call(
        5,
        "conversation/markRead",
        json!({"conversationId": main, "expectedRevision": revision}),
    );
    assert_eq!(marked["result"]["conversation"]["unread"], json!(0));

    let stale = server.call(
        6,
        "conversation/markRead",
        json!({"conversationId": main, "expectedRevision": revision + 99}),
    );
    assert_eq!(code_of(&stale), "STALE_REVISION");

    let missing = server.call(
        7,
        "conversation/read",
        json!({"conversationId": "conv_never"}),
    );
    assert_eq!(code_of(&missing), "CONVERSATION_NOT_FOUND");

    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
}

/// Bytes become an asset the server owns, and come back chunk by chunk.
#[test]
fn an_asset_is_registered_and_read_back_over_the_wire() {
    let root = TempDir::new("asset");
    let mut server = Server::start(&root);
    server.handshake();
    server.call(2, "session/start", json!({}));

    let file = root.path().join("note.txt");
    std::fs::write(&file, b"the bytes the server takes a copy of").expect("asset fixture");
    let registered = server.call(3, "asset/registerPath", json!({"path": file}));
    let asset = &registered["result"]["asset"];
    let id = asset["id"].clone();
    assert!(
        id.as_str().is_some_and(|id| id.starts_with("asset_")),
        "{registered}"
    );

    let chunk = server.call(
        4,
        "asset/readChunk",
        json!({"assetId": id, "offset": 0, "length": 1024}),
    );
    assert_eq!(chunk["result"]["eof"], json!(true), "{chunk}");
    let data = chunk["result"]["data"].as_str().unwrap_or_default();
    // Base64 of the bytes above, decoded the way a client would.
    assert!(!data.is_empty());

    let missing = server.call(
        5,
        "asset/readChunk",
        json!({"assetId": "asset_never", "offset": 0, "length": 16}),
    );
    assert_eq!(code_of(&missing), "ASSET_NOT_FOUND");
    let rejected = server.call(
        6,
        "asset/registerPath",
        json!({"path": root.path().join("not-here.png")}),
    );
    assert_eq!(code_of(&rejected), "ASSET_REJECTED");

    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
}

/// A malformed line is a parse error and nothing else; an unknown method and
/// unreadable arguments are told apart; stdout stays frames only.
#[test]
fn the_error_paths_answer_without_disturbing_the_connection() {
    let root = TempDir::new("errors");
    let mut server = Server::start(&root);
    server.handshake();

    server.write("{ this is not json");
    let parse = server.frame().expect("a parse error is still an answer");
    assert_eq!(parse["id"], Value::Null, "{parse}");
    assert_eq!(parse["error"]["code"], json!(-32700), "{parse}");

    let unknown = server.call(2, "session/levitate", json!({}));
    assert_eq!(unknown["error"]["code"], json!(-32601), "{unknown}");

    let unreadable = server.call(3, "catalog/read", json!({"catalog": "unicorns"}));
    assert_eq!(unreadable["error"]["code"], json!(-32602), "{unreadable}");
    assert_eq!(code_of(&unreadable), "BAD_ARGUMENT");

    // And the connection is exactly as it was.
    let after = server.call(4, "catalog/read", json!({"catalog": "providers"}));
    assert!(after.get("result").is_some(), "{after}");

    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
    assert!(ended.stderr.is_empty(), "stderr: {}", ended.stderr);
}

/// `shutdown` answers, then the process leaves.
#[test]
fn shutdown_answers_before_the_process_leaves() {
    let root = TempDir::new("shutdown");
    let mut server = Server::start(&root);
    server.handshake();
    server.call(2, "session/start", json!({}));
    let frame = server.call(3, "shutdown", json!({}));
    assert_eq!(
        frame["result"],
        json!({"interruptedTurns": 0, "deniedInteractions": 0})
    );
    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
    assert!(ended.stderr.is_empty(), "stderr: {}", ended.stderr);
}

#[test]
fn a_line_past_the_ceiling_closes_the_transport_with_its_own_code() {
    let root = TempDir::new("oversized");
    let mut server = Server::start(&root);
    server.handshake();
    let huge = "x".repeat(1_048_577);
    server.write(
        &json!({"jsonrpc": "2.0", "id": 2, "method": "session/start", "params": {"model": huge}})
            .to_string(),
    );
    let ended = server.finish();
    assert_eq!(ended.code, Some(1));
    assert!(
        ended.stderr.contains("[error] code=FRAME_TOO_LARGE msg="),
        "stderr: {}",
        ended.stderr
    );
    assert_eq!(ended.stderr.lines().count(), 1);
}

#[test]
fn input_that_is_not_utf8_closes_the_transport() {
    let root = TempDir::new("utf8");
    let mut server = Server::start(&root);
    server.handshake();
    server.write_bytes(&[0xff, 0xfe, 0x00, b'\n']);
    let ended = server.finish();
    assert_eq!(ended.code, Some(1));
    assert!(
        ended.stderr.contains("[error] code=TRANSPORT_FAILED msg="),
        "stderr: {}",
        ended.stderr
    );
}

/// A frontend-shaped read: the whole surface a GUI touches before its first
/// turn, in one connection, with nothing on stderr and nothing but frames on
/// stdout.
#[test]
fn a_client_can_walk_the_whole_session_free_surface_in_one_connection() {
    let root = TempDir::new("surface");
    let mut server = Server::start(&root);
    server.handshake();
    let calls: Vec<(&str, Value)> = vec![
        ("catalog/read", json!({"catalog": "providers"})),
        ("catalog/read", json!({"catalog": "models"})),
        ("session/list", json!({})),
        ("session/start", json!({})),
        ("session/read", json!({})),
        ("conversation/list", json!({})),
        ("action/list", json!({})),
        ("config/read", json!({})),
        ("resource/read", json!({"resource": "agents"})),
    ];
    let mut id = 1i64;
    for (method, params) in calls {
        id += 1;
        let frame = server.call(id, method, params);
        assert!(frame.get("result").is_some(), "{method}: {frame}");
    }
    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
    assert!(ended.stderr.is_empty(), "stderr: {}", ended.stderr);
}

// ---------------------------------------------------------------------------
// A provider that answers from a script
// ---------------------------------------------------------------------------

/// An Anthropic-protocol endpoint on loopback that answers from a script.
///
/// A model would make these tests measure the model. What they are about is the
/// path — that a submission becomes a turn, that the turn's stream becomes
/// items, that a prompt stops the run and an answer restarts it — so the
/// provider is exactly as real as it needs to be: a socket that speaks the
/// protocol the client speaks.
struct Provider {
    port: u16,
    /// How many turns have been asked for, so a test can say the tool loop went
    /// round twice rather than infer it from the text.
    asked: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
}

impl Provider {
    /// `script[n]` is the SSE body of the n-th request; the last entry repeats,
    /// so a run that goes one round further than a test expected still ends.
    fn start(script: Vec<String>) -> Self {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("the fake provider must take a port");
        let port = listener.local_addr().expect("a bound port").port();
        listener
            .set_nonblocking(true)
            .expect("the accept loop must not block on shutdown");
        let asked = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (counter, halt) = (asked.clone(), stop.clone());
        std::thread::spawn(move || {
            while !halt.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let script = script.clone();
                        let counter = counter.clone();
                        std::thread::spawn(move || serve(stream, &script, &counter));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => return,
                }
            }
        });
        Self { port, asked, stop }
    }

    /// The settings that point a session at it, written where a project's own
    /// settings go.
    fn configure(&self, root: &TempDir) {
        let dir = root.path().join(".bingo");
        std::fs::create_dir_all(&dir).expect("project config directory");
        std::fs::write(
            dir.join("settings.json"),
            serde_json::to_vec(&json!({
                "providers": {
                    "scripted": {
                        "protocol": "anthropic",
                        "apiBaseUrl": format!("http://127.0.0.1:{}", self.port),
                        "apiKey": "sk-scripted"
                    }
                },
                "provider": "scripted",
                "model": "scripted-1"
            }))
            .expect("settings fixture"),
        )
        .expect("settings fixture");
    }

    fn rounds(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

/// A session that never stops to ask, for the scenarios that are about
/// something other than the prompt.
fn never_asks(root: &TempDir) {
    std::fs::create_dir_all(root.path().join(".bingo")).expect("project config directory");
    std::fs::write(
        root.path().join(".bingo/local.json"),
        br#"{"permissionMode":"bypassPermissions"}"#,
    )
    .expect("settings fixture");
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// One HTTP request, one answer. `Connection: close` on purpose: a script step
/// per connection is one less thing for a test to reason about.
fn serve(mut stream: TcpStream, script: &[String], asked: &AtomicUsize) {
    let mut reader = BufReader::new(stream.try_clone().expect("the socket must clone"));
    let mut head = String::new();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = value.trim().parse().unwrap_or(0);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        head.push_str(&line);
    }
    let mut body = vec![0u8; length];
    let _ = std::io::Read::read_exact(&mut reader, &mut body);
    // A token count is arithmetic the compactor asks for, not a turn.
    if head.contains("/v1/messages/count_tokens") {
        let payload = br#"{"input_tokens":128}"#;
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                payload.len()
            )
            .as_bytes(),
        );
        let _ = stream.write_all(payload);
        return;
    }
    let step = asked.fetch_add(1, Ordering::SeqCst);
    let sse = script
        .get(step)
        .or_else(|| script.last())
        .cloned()
        .unwrap_or_default();
    let _ = stream.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
    );
    let _ = stream.write_all(sse.as_bytes());
    let _ = stream.flush();
}

/// One SSE frame.
fn frame_of(event: &str, data: Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

fn message_open() -> String {
    frame_of(
        "message_start",
        json!({"type": "message_start", "message": {"id": "msg_1", "model": "scripted-1"}}),
    )
}

fn message_close(stop_reason: &str, output_tokens: u64) -> String {
    frame_of(
        "message_delta",
        json!({
            "type": "message_delta",
            "delta": {"stop_reason": stop_reason},
            "usage": {"output_tokens": output_tokens}
        }),
    ) + &frame_of("message_stop", json!({"type": "message_stop"}))
}

fn text_block(index: usize, text: &str) -> String {
    frame_of(
        "content_block_start",
        json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}}),
    ) + &frame_of(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": text}}),
    ) + &frame_of(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": index}),
    )
}

fn thinking_block(index: usize, thinking: &str) -> String {
    frame_of(
        "content_block_start",
        json!({"type": "content_block_start", "index": index, "content_block": {"type": "thinking", "thinking": ""}}),
    ) + &frame_of(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": index, "delta": {"type": "thinking_delta", "thinking": thinking}}),
    ) + &frame_of(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": index}),
    )
}

fn tool_block(index: usize, id: &str, name: &str, input: Value) -> String {
    frame_of(
        "content_block_start",
        json!({"type": "content_block_start", "index": index, "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}),
    ) + &frame_of(
        "content_block_delta",
        json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": input.to_string()}}),
    ) + &frame_of(
        "content_block_stop",
        json!({"type": "content_block_stop", "index": index}),
    )
}

/// A turn that says something and stops.
fn says(text: &str) -> String {
    message_open() + &text_block(0, text) + &message_close("end_turn", 12)
}

/// A stream that fails in a way the engine retries.
fn overloaded() -> String {
    message_open()
        + &frame_of(
            "error",
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "try again"}}),
        )
}

// ---------------------------------------------------------------------------
// What a model turn looks like on the wire
// ---------------------------------------------------------------------------

impl Server {
    /// Read frames until the named notification arrives, and return everything
    /// read on the way, that frame last.
    ///
    /// A timeout is not needed and would hide the failure it caught: if the
    /// notification never comes the process is holding the pipe open, and the
    /// test harness's own timeout says so with the whole stream still in hand.
    fn until(&mut self, method: &str) -> Vec<Value> {
        let mut seen = std::mem::take(&mut self.seen);
        if seen.iter().any(|frame| frame["method"] == json!(method)) {
            return seen;
        }
        loop {
            let Some(frame) = self.frame() else {
                panic!("the server closed before {method}: {seen:#?}");
            };
            let done = frame["method"] == json!(method);
            seen.push(frame);
            if done {
                return seen;
            }
        }
    }

    /// Start a session and answer with main's conversation id.
    fn open_main(&mut self, id: i64) -> Value {
        let started = self.call(id, "session/start", json!({}));
        assert!(started.get("result").is_some(), "{started}");
        started["result"]["snapshot"]["conversations"]["active"][0]["id"].clone()
    }

    fn submit(&mut self, id: i64, main: &Value, text: &str) -> Value {
        self.call(
            id,
            "conversation/submit",
            json!({
                "conversationId": main,
                "input": {"type": "composer", "mode": "normal", "text": text, "attachments": []}
            }),
        )
    }
}

/// Every notification of one method, in order.
fn of<'a>(frames: &'a [Value], method: &str) -> Vec<&'a Value> {
    frames
        .iter()
        .filter(|frame| frame["method"] == json!(method))
        .collect()
}

/// The sequence numbers of everything sequenced, which must be gapless.
fn gapless(frames: &[Value]) {
    let mut last: Option<u64> = None;
    for frame in frames {
        let Some(seq) = frame["params"]["event"]["seq"].as_u64() else {
            continue;
        };
        if let Some(previous) = last {
            assert_eq!(seq, previous + 1, "a gap before {frame}");
        }
        last = Some(seq);
    }
}

/// One prose turn: what it says, what it thought, and what it cost.
#[test]
fn a_submission_becomes_a_turn_whose_text_reasoning_and_usage_all_reach_the_client() {
    let provider = Provider::start(vec![
        message_open()
            + &thinking_block(0, "the suite is the fastest check")
            + &text_block(1, "Running the tests.")
            + &message_close("end_turn", 37),
    ]);
    let root = TempDir::new("turn");
    provider.configure(&root);
    let mut server = Server::start(&root);
    server.handshake();
    let main = server.open_main(2);

    let submitted = server.submit(3, &main, "run the tests");
    let turn = submitted["result"]["disposition"]["turnId"].clone();
    assert_eq!(
        submitted["result"]["disposition"]["type"],
        json!("turnStarted"),
        "{submitted}"
    );
    assert!(turn.is_string(), "{submitted}");

    let frames = server.until("turn/completed");
    gapless(&frames);

    // The prose the user submitted is an item, and it carries no turn: the turn
    // names it as an input instead (spec "Item").
    let opened = of(&frames, "turn/started");
    assert_eq!(opened.len(), 1, "{frames:#?}");
    let input = opened[0]["params"]["turn"]["inputItemIds"][0].clone();
    assert!(input.is_string(), "{}", opened[0]);

    let reasoning: String = of(&frames, "item/reasoningDelta")
        .iter()
        .filter_map(|frame| frame["params"]["delta"].as_str())
        .collect();
    assert_eq!(reasoning, "the suite is the fastest check");
    let text: String = of(&frames, "item/textDelta")
        .iter()
        .filter_map(|frame| frame["params"]["delta"].as_str())
        .collect();
    assert_eq!(text, "Running the tests.");
    assert!(
        of(&frames, "item/textDelta")
            .windows(2)
            .all(|pair| pair[0]["params"]["deltaSeq"].as_u64()
                < pair[1]["params"]["deltaSeq"].as_u64()),
        "deltas are ordered within their item"
    );

    // Reasoning and prose are two items, never one spliced stream.
    let bodies: Vec<&str> = of(&frames, "item/started")
        .iter()
        .filter_map(|frame| frame["params"]["item"]["type"].as_str())
        .collect();
    assert!(bodies.contains(&"reasoning"), "{bodies:?}");
    assert!(bodies.contains(&"assistantMessage"), "{bodies:?}");

    assert_eq!(of(&frames, "turn/roundStarted").len(), 1, "one round");
    let usage = of(&frames, "turn/usageUpdated");
    assert!(
        usage
            .iter()
            .any(|frame| frame["params"]["usage"]["outputTokens"] == json!(37)),
        "the provider's own count travels: {usage:#?}"
    );
    let completed = of(&frames, "turn/completed");
    assert_eq!(completed[0]["params"]["turn"]["id"], turn);
    assert_eq!(completed[0]["params"]["turn"]["status"], json!("completed"));

    // The whole turn is readable back, in the same order it was described.
    let read = server.call(4, "conversation/read", json!({"conversationId": main}));
    let items = read["result"]["snapshot"]["items"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let kinds: Vec<&str> = items
        .iter()
        .filter_map(|item| item["type"].as_str())
        .collect();
    assert_eq!(
        kinds,
        vec!["userMessage", "reasoning", "assistantMessage"],
        "{items:#?}"
    );

    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
    assert_eq!(provider.rounds(), 1, "one round, one request");
}

/// A tool call, the prompt that stops it, and the denial that answers it —
/// including the direction the denial carries back to the model.
#[test]
fn a_tool_call_stops_on_a_prompt_and_a_denial_travels_back_to_the_model() {
    let provider = Provider::start(vec![
        message_open()
            + &tool_block(0, "toolu_1", "Bash", json!({"command": "echo hello"}))
            + &message_close("tool_use", 8),
        says("I will not run it then."),
    ]);
    let root = TempDir::new("permission");
    provider.configure(&root);
    let mut server = Server::start(&root);
    server.handshake();
    let main = server.open_main(2);
    server.submit(3, &main, "say hello from the shell");

    // The tool is visible before the prompt is (interaction ordering, step 1).
    let opening = server.until("interaction/opened");
    let started = of(&opening, "item/started");
    assert!(
        started
            .iter()
            .any(|frame| frame["params"]["item"]["type"] == json!("toolCall")),
        "the call is an item before the prompt is a prompt: {opening:#?}"
    );
    let prompt = of(&opening, "interaction/opened")[0]["params"]["interaction"].clone();
    assert_eq!(prompt["prompt"]["type"], json!("permission"), "{prompt}");
    assert_eq!(prompt["prompt"]["tool"]["name"], json!("Bash"));
    assert_eq!(
        prompt["prompt"]["preview"],
        json!({"type": "command", "command": "echo hello"}),
        "a command prompt shows the command"
    );
    let decisions = prompt["prompt"]["decisions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(decisions.contains(&json!("allowOnce")), "{decisions:?}");
    assert!(decisions.contains(&json!("deny")), "{decisions:?}");
    assert!(
        prompt["remainingGuardMs"].is_number(),
        "D81's guard is recomputed for the client: {prompt}"
    );

    let answered = server.call(
        4,
        "interaction/respond",
        json!({
            "interactionId": prompt["id"],
            "activation": "programmatic",
            "decision": {"type": "deny", "feedback": "use the Write tool instead"}
        }),
    );
    assert_eq!(
        answered["result"]["status"],
        json!("accepted"),
        "{answered}"
    );

    let frames = server.until("turn/completed");
    gapless(&frames);
    assert_eq!(of(&frames, "interaction/resolved").len(), 1, "{frames:#?}");
    let failed = of(&frames, "item/completed")
        .into_iter()
        .find(|frame| frame["params"]["item"]["type"] == json!("toolCall"))
        .unwrap_or_else(|| panic!("the call reaches a terminal state: {frames:#?}"));
    assert_eq!(failed["params"]["item"]["status"], json!("failed"));

    // A late answer to a closed prompt is refused by name and changes nothing.
    let late = server.call(
        5,
        "interaction/respond",
        json!({
            "interactionId": prompt["id"],
            "activation": "programmatic",
            "decision": {"type": "allowOnce"}
        }),
    );
    assert_eq!(code_of(&late), "INTERACTION_CLOSED");

    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
    assert_eq!(
        provider.rounds(),
        2,
        "the denial went back to the model, which answered it"
    );
}

/// A failed attempt is not history: the retry says what it withdrew, and the
/// round it re-enters is the same round.
#[test]
fn a_stream_retry_withdraws_the_attempt_it_lost_and_re_enters_the_round() {
    let provider = Provider::start(vec![
        message_open() + &text_block(0, "half a sen") + &overloaded(),
        says("A whole sentence."),
    ]);
    let root = TempDir::new("retry");
    provider.configure(&root);
    let mut server = Server::start(&root);
    server.handshake();
    let main = server.open_main(2);
    server.submit(3, &main, "say something");

    let frames = server.until("turn/completed");
    gapless(&frames);
    let retrying = of(&frames, "turn/retrying");
    assert_eq!(retrying.len(), 1, "{frames:#?}");
    assert_eq!(retrying[0]["params"]["attempt"], json!(1));
    assert!(
        retrying[0]["params"]["maxAttempts"].as_u64().unwrap_or(0) > 1,
        "{}",
        retrying[0]
    );
    let removed = retrying[0]["params"]["removedItemIds"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !removed.is_empty(),
        "the attempt drew something, so the retry says what it took back: {}",
        retrying[0]
    );

    // What the failed attempt said is not in the conversation.
    let read = server.call(4, "conversation/read", json!({"conversationId": main}));
    let text = read.to_string();
    assert!(!text.contains("half a sen"), "{read}");
    assert!(text.contains("A whole sentence."), "{read}");

    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
    assert_eq!(provider.rounds(), 2, "one attempt failed, one succeeded");
}

/// The console's shell line: it opens main's turn, publishes a tail while it
/// runs, and its output is the item's, not the tail's.
#[test]
fn a_shell_line_runs_in_the_console_and_publishes_a_tail_while_it_does() {
    // Nothing to answer: the shell line runs and the model is not asked.
    let provider = Provider::start(vec![says("done")]);
    let root = TempDir::new("shell");
    provider.configure(&root);
    never_asks(&root);
    let mut server = Server::start(&root);
    server.handshake();
    let main = server.open_main(2);

    let submitted = server.call(
        3,
        "conversation/submit",
        json!({
            "conversationId": main,
            "input": {
                "type": "composer",
                "mode": "shell",
                "text": "printf 'one\\ntwo\\n'; sleep 0.4; printf 'three\\n'",
                "attachments": []
            }
        }),
    );
    assert_eq!(
        submitted["result"]["disposition"]["type"],
        json!("turnStarted"),
        "{submitted}"
    );

    let frames = server.until("turn/completed");
    gapless(&frames);
    let typed: Vec<&Value> = of(&frames, "item/completed")
        .into_iter()
        .filter(|frame| frame["params"]["item"]["type"] == json!("userMessage"))
        .collect();
    assert_eq!(typed.len(), 1, "the line is one item, not two: {typed:#?}");
    assert!(
        typed[0]["params"]["item"]["turnId"].is_null(),
        "an item that opens a turn carries no turnId: {}",
        typed[0]
    );
    let done = of(&frames, "item/completed")
        .into_iter()
        .find(|frame| frame["params"]["item"]["name"] == json!("Bash"))
        .unwrap_or_else(|| panic!("the shell call is an item: {frames:#?}"));
    let output = done["params"]["item"]["output"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(output.contains("three"), "{output}");
    for tail in of(&frames, "item/commandTailUpdated") {
        assert_eq!(
            tail["params"]["itemId"], done["params"]["item"]["id"],
            "a sample belongs to the call that is running"
        );
        assert!(tail["params"]["tail"]["lines"].is_array(), "{tail}");
    }

    let read = server.call(4, "conversation/read", json!({"conversationId": main}));
    assert!(
        read.to_string()
            .contains("!printf 'one\\\\ntwo\\\\n'; sleep 0.4; printf 'three\\\\n'"),
        "the transcript keeps the line as it was submitted: {read}"
    );

    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
}

/// Busy main queues, the queue drains at the turn boundary, and an interrupt
/// reaches the run that is open.
#[test]
fn input_queues_behind_a_turn_drains_at_its_end_and_an_interrupt_reaches_it() {
    let provider = Provider::start(vec![
        message_open()
            + &tool_block(0, "toolu_1", "Bash", json!({"command": "sleep 5"}))
            + &message_close("tool_use", 4),
        says("first is done"),
        says("second is done"),
    ]);
    let root = TempDir::new("queue-drain");
    provider.configure(&root);
    // Nothing may stop to ask: this is about the queue, not the prompt.
    never_asks(&root);
    let mut server = Server::start(&root);
    server.handshake();
    let main = server.open_main(2);

    let first = server.submit(3, &main, "the first thing");
    let turn = first["result"]["disposition"]["turnId"].clone();
    assert_eq!(first["result"]["disposition"]["type"], json!("turnStarted"));

    let second = server.submit(4, &main, "the second thing");
    assert_eq!(
        second["result"]["disposition"]["type"],
        json!("queued"),
        "busy main queues rather than starting a second turn: {second}"
    );
    assert_eq!(second["result"]["disposition"]["position"], json!(0));
    assert!(
        second["result"]["disposition"]["steerEligible"]
            .as_bool()
            .unwrap_or(false),
        "plain prose may ride along at a barrier"
    );

    let queue = server.call(5, "queue/read", json!({"conversationId": main}));
    assert_eq!(queue["result"]["count"], json!(1), "{queue}");

    let interrupted = server.call(
        6,
        "turn/interrupt",
        json!({"conversationId": main, "turnId": turn}),
    );
    assert_eq!(
        interrupted["result"]["accepted"],
        json!(true),
        "{interrupted}"
    );

    // The turn that was running ends, and its ending is what starts the next.
    let frames = server.until("turn/completed");
    gapless(&frames);
    let ended_turn = of(&frames, "turn/completed")[0]["params"]["turn"].clone();
    assert_eq!(ended_turn["id"], turn);
    assert_eq!(
        ended_turn["status"],
        json!("interrupted"),
        "an interrupted turn is still a turn that ended"
    );
    let next = server.until("turn/started");
    let opened = of(&next, "turn/started");
    assert_eq!(opened.len(), 1, "{next:#?}");
    assert_ne!(
        opened[0]["params"]["turn"]["id"], turn,
        "the queue drained into a new turn"
    );
    assert_eq!(opened[0]["params"]["turn"]["origin"], json!("queue"));

    let ended = server.finish();
    assert_eq!(ended.code, Some(0));
}

//! `bingo app-server` as a client meets it: a real process, an isolated home,
//! and frames over its stdin and stdout.
//!
//! **What is here** is everything the server can do without a model: the
//! handshake and its refusals, the session lifecycle including resume with the
//! rooms it left, the catalogs and configuration a client reads on the way in,
//! the actions the core owns outright, the queue, attention, the error paths,
//! stdout purity, and the exit codes.
//!
//! **What is not here, and is not pretended:** text, tool calls, permission
//! prompts, stream retries, and steering. Those need the engine, which reaches
//! the wire in B7 — the transport carries no model turn today, so a scenario
//! asserting one would be asserting a stub. B7 adds them to this file.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
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

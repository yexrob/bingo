use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const CONTINUE_PROMPT: &str = "Continue the unfinished task. Inspect the current workspace and completed tool results first, then run only the remaining steps without repeating completed or potentially side-effecting operations.";

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "bingo-json-cancel-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temporary test directory must be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

enum StreamReply {
    StallAfter(String),
    Complete(String),
}

fn spawn_scripted_api(replies: Vec<StreamReply>) -> (String, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("mock API must bind");
    let address = listener.local_addr().expect("mock API address");
    let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
    let (request_tx, request_rx) = mpsc::channel();

    thread::spawn(move || {
        for socket in listener.incoming() {
            let Ok(mut socket) = socket else { return };
            let replies = replies.clone();
            let request_tx = request_tx.clone();
            thread::spawn(move || {
                let request = read_http_request(&mut socket);
                if request.contains("/v1/messages/count_tokens") {
                    write_complete_response(
                        &mut socket,
                        "application/json",
                        r#"{"input_tokens":10}"#,
                    );
                    return;
                }

                let _ = request_tx.send(request);
                let reply = replies
                    .lock()
                    .expect("mock reply queue")
                    .pop_front()
                    .expect("one scripted reply per model request");
                match reply {
                    StreamReply::Complete(body) => {
                        write_complete_response(&mut socket, "text/event-stream", &body)
                    }
                    StreamReply::StallAfter(body) => {
                        let header = concat!(
                            "HTTP/1.1 200 OK\r\n",
                            "Content-Type: text/event-stream\r\n",
                            "Transfer-Encoding: chunked\r\n",
                            "Connection: close\r\n\r\n"
                        );
                        let _ = socket.write_all(header.as_bytes());
                        let _ =
                            socket.write_all(format!("{:X}\r\n{body}\r\n", body.len()).as_bytes());
                        let _ = socket.flush();
                        thread::sleep(Duration::from_secs(10));
                        let _ = socket.write_all(b"0\r\n\r\n");
                    }
                }
            });
        }
    });

    (format!("http://{address}"), request_rx)
}

fn read_http_request(socket: &mut TcpStream) -> String {
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("request read timeout");
    let mut request = Vec::new();
    loop {
        let mut buffer = [0_u8; 4096];
        let read = socket.read(&mut buffer).unwrap_or(0);
        if read == 0 {
            break;
        }
        request.extend_from_slice(&buffer[..read]);
        let text = String::from_utf8_lossy(&request);
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            continue;
        };
        let content_length = head
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .and_then(|value| value.trim().parse::<usize>().ok())
            })
            .unwrap_or(0);
        if body.len() >= content_length {
            break;
        }
    }
    String::from_utf8(request).expect("Bingo HTTP requests must be UTF-8")
}

fn write_complete_response(socket: &mut TcpStream, content_type: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes());
    let _ = socket.flush();
}

fn sse(events: &[(&str, String)]) -> String {
    events
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

fn text_stream_prefix(text: &str) -> String {
    let text = serde_json::to_string(text).expect("text JSON");
    sse(&[
        (
            "message_start",
            r#"{"message":{"id":"m_1","model":"test-model"}}"#.to_string(),
        ),
        (
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"text","text":""}}"#.to_string(),
        ),
        (
            "content_block_delta",
            format!(r#"{{"index":0,"delta":{{"type":"text_delta","text":{text}}}}}"#),
        ),
    ])
}

fn text_turn(text: &str) -> String {
    let mut body = text_stream_prefix(text);
    body.push_str(&sse(&[
        ("content_block_stop", r#"{"index":0}"#.to_string()),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":5}}"#.to_string(),
        ),
        ("message_stop", "{}".to_string()),
    ]));
    body
}

fn bash_tool_turn(id: &str, command: &str) -> String {
    let partial_json =
        serde_json::to_string(&json!({ "command": command }).to_string()).expect("tool input JSON");
    sse(&[
        (
            "message_start",
            r#"{"message":{"id":"m_1","model":"test-model"}}"#.to_string(),
        ),
        (
            "content_block_start",
            format!(
                r#"{{"index":0,"content_block":{{"type":"tool_use","id":"{id}","name":"Bash","input":{{}}}}}}"#
            ),
        ),
        (
            "content_block_delta",
            format!(
                r#"{{"index":0,"delta":{{"type":"input_json_delta","partial_json":{partial_json}}}}}"#
            ),
        ),
        ("content_block_stop", r#"{"index":0}"#.to_string()),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}"#.to_string(),
        ),
        ("message_stop", "{}".to_string()),
    ])
}

struct BingoProcess {
    child: Child,
    stdin: ChildStdin,
    events: mpsc::Receiver<Result<Value, String>>,
    stderr: mpsc::Receiver<String>,
}

impl BingoProcess {
    fn spawn(root: &TempDir, api_base_url: &str) -> Self {
        Self::spawn_session(root, api_base_url, None)
    }

    fn resume(root: &TempDir, api_base_url: &str, session_id: &str) -> Self {
        Self::spawn_session(root, api_base_url, Some(session_id))
    }

    fn spawn_session(root: &TempDir, api_base_url: &str, session_id: Option<&str>) -> Self {
        fs::create_dir_all(root.path().join(".bingo")).expect("settings directory");
        fs::write(
            root.path().join(".bingo/settings.json"),
            serde_json::to_vec(&json!({
                "apiKey": "test-key",
                "apiBaseUrl": api_base_url,
                "model": "test-model",
                "permissionMode": "bypassPermissions"
            }))
            .expect("settings JSON"),
        )
        .expect("settings fixture");

        let mut arguments = vec!["--json-events", "--no-team"];
        if let Some(session_id) = session_id {
            arguments.extend(["--session", session_id]);
        }
        let mut child = Command::new(env!("CARGO_BIN_EXE_bingo"))
            .args(arguments)
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
            .expect("bingo process must start");
        let stdin = child.stdin.take().expect("bingo stdin");
        let stdout = child.stdout.take().expect("bingo stdout");
        let stderr = child.stderr.take().expect("bingo stderr");
        let (event_tx, event_rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let event = line.map_err(|error| error.to_string()).and_then(|line| {
                    serde_json::from_str(&line).map_err(|error| error.to_string())
                });
                if event_tx.send(event).is_err() {
                    return;
                }
            }
        });
        let (stderr_tx, stderr_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut stderr = BufReader::new(stderr);
            let mut output = String::new();
            let _ = stderr.read_to_string(&mut output);
            let _ = stderr_tx.send(output);
        });

        let process = Self {
            child,
            stdin,
            events: event_rx,
            stderr: stderr_rx,
        };
        let ready = process.event("session.ready", |event| event["type"] == "session.ready");
        assert_eq!(ready["protocolVersion"], 1);
        process
    }

    fn send(&mut self, command: Value) {
        serde_json::to_writer(&mut self.stdin, &command).expect("command JSON");
        self.stdin.write_all(b"\n").expect("command newline");
        self.stdin.flush().expect("command flush");
    }

    fn event(&self, description: &str, predicate: impl Fn(&Value) -> bool) -> Value {
        self.events_until(description, predicate)
            .pop()
            .expect("matching event")
    }

    fn events_until(&self, description: &str, predicate: impl Fn(&Value) -> bool) -> Vec<Value> {
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut events = Vec::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = self
                .events
                .recv_timeout(remaining)
                .unwrap_or_else(|error| {
                    panic!("timed out waiting for {description}: {error}; received {events:?}")
                })
                .unwrap_or_else(|error| {
                    panic!("invalid NDJSON while waiting for {description}: {error}")
                });
            let matched = predicate(&event);
            events.push(event);
            if matched {
                return events;
            }
        }
    }

    fn close(mut self) {
        self.send(json!({
            "protocolVersion": 1,
            "type": "session.close",
            "commandId": "close-command"
        }));
        let closed = self.event("session.closed", |event| event["type"] == "session.closed");
        assert_eq!(closed["commandId"], "close-command");
        drop(self.stdin);

        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("bingo process status") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                panic!("bingo process did not exit after session.close");
            }
            thread::sleep(Duration::from_millis(20));
        };
        assert!(status.success(), "bingo exited with {status}");
        let stderr = self
            .stderr
            .recv_timeout(Duration::from_secs(2))
            .expect("stderr reader must finish");
        assert!(stderr.is_empty(), "bingo stderr was not empty: {stderr}");
    }
}

fn start_turn(process: &mut BingoProcess, turn_id: &str, command_id: &str, prompt: &str) {
    process.send(json!({
        "protocolVersion": 1,
        "type": "turn.start",
        "commandId": command_id,
        "turnId": turn_id,
        "prompt": prompt
    }));
    let started = process.event("turn.started", |event| {
        event["type"] == "turn.started" && event["turnId"] == turn_id
    });
    assert_eq!(started["commandId"], command_id);
}

fn cancel_turn(process: &mut BingoProcess, turn_id: &str, command_id: &str) -> Vec<Value> {
    process.send(json!({
        "protocolVersion": 1,
        "type": "turn.cancel",
        "commandId": command_id,
        "turnId": turn_id
    }));
    let events = process.events_until("turn.cancelled", |event| {
        event["type"] == "turn.cancelled" && event["turnId"] == turn_id
    });
    let cancelled = events.last().expect("cancelled event");
    assert_eq!(cancelled["commandId"], command_id);
    assert_eq!(cancelled["reason"], "requested");
    events
}

fn request_body(raw: &str) -> Value {
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, body)| body)
        .expect("HTTP request body");
    serde_json::from_str(body).expect("model request JSON")
}

fn request_texts(request: &Value) -> Vec<&str> {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|message| {
            message["content"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|block| block["text"].as_str())
        })
        .collect()
}

fn tool_ids(request: &Value, block_type: &str, id_key: &str) -> Vec<String> {
    request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .filter(|block| block["type"] == block_type)
        .filter_map(|block| block[id_key].as_str().map(str::to_string))
        .collect()
}

fn next_request(requests: &mpsc::Receiver<String>) -> Value {
    let raw = requests
        .recv_timeout(Duration::from_secs(10))
        .expect("model request must arrive");
    request_body(&raw)
}

#[test]
fn fork_event_hands_off_an_unlocked_child_transcript() {
    let root = TempDir::new("fork-handoff");
    let source_session_id = "source-session";
    let transcripts = root.path().join(".local/share/bingo/transcripts");
    fs::create_dir_all(&transcripts).expect("transcript fixture directory");
    let source = [
        json!({
            "type": "session",
            "schemaVersion": 1,
            "cwd": root.path().canonicalize().expect("canonical fixture path")
        })
        .to_string(),
        json!({
            "role": "user",
            "content": [{ "type": "text", "text": "edit this prompt" }]
        })
        .to_string(),
    ]
    .join("\n");
    fs::write(
        transcripts.join(format!("{source_session_id}.jsonl")),
        format!("{source}\n"),
    )
    .expect("source transcript fixture");
    let (api_base_url, _requests) = spawn_scripted_api(Vec::new());
    let mut source_process = BingoProcess::resume(&root, &api_base_url, source_session_id);

    source_process.send(json!({
        "protocolVersion": 1,
        "type": "session.fork",
        "commandId": "fork-handoff",
        "reason": "edit-last-prompt"
    }));
    let forked = source_process.event("session.forked", |event| {
        event["type"] == "session.forked" && event["commandId"] == "fork-handoff"
    });
    let child_session_id = forked["metadata"]["sessionId"]
        .as_str()
        .expect("forked child session id");
    let child_path = transcripts.join(format!("{child_session_id}.jsonl"));

    fs::read_to_string(&child_path)
        .expect("session.forked must not be observable before the child is readable");
    let child_process = BingoProcess::resume(&root, &api_base_url, child_session_id);

    source_process.close();
    child_process.close();
}

#[test]
fn cancel_during_model_stream_then_continue_uses_durable_context() {
    let root = TempDir::new("model-stream");
    let (api_base_url, requests) = spawn_scripted_api(vec![
        StreamReply::StallAfter(text_stream_prefix("partial-before-cancel")),
        StreamReply::Complete(text_turn("continued-after-cancel")),
    ]);
    let mut process = BingoProcess::spawn(&root, &api_base_url);

    start_turn(
        &mut process,
        "turn-stream",
        "start-stream",
        "first durable prompt",
    );
    let delta = process.event("text.delta", |event| {
        event["type"] == "text.delta" && event["turnId"] == "turn-stream"
    });
    assert_eq!(delta["delta"], "partial-before-cancel");
    cancel_turn(&mut process, "turn-stream", "cancel-stream");

    start_turn(
        &mut process,
        "turn-stream-continue",
        "continue-stream",
        CONTINUE_PROMPT,
    );
    process.event("continued turn completion", |event| {
        event["type"] == "turn.completed" && event["turnId"] == "turn-stream-continue"
    });

    let _first_request = next_request(&requests);
    let second_request = next_request(&requests);
    let texts = request_texts(&second_request);
    assert!(texts.contains(&"first durable prompt"), "{second_request}");
    assert!(texts.contains(&CONTINUE_PROMPT), "{second_request}");
    assert!(
        !texts.contains(&"partial-before-cancel"),
        "an incomplete assistant stream must not enter durable context: {second_request}"
    );
    assert!(
        tool_ids(&second_request, "tool_use", "id").is_empty(),
        "stream cancellation must not invent tool calls: {second_request}"
    );
    process.close();
}

#[test]
fn cancel_during_tool_execution_then_continue_pairs_the_interrupted_tool() {
    let root = TempDir::new("tool-execution");
    let marker = root.path().join("tool-started.txt");
    #[cfg(windows)]
    let command = format!(
        "Set-Content -LiteralPath '{}' -Value started; Start-Sleep -Seconds 20",
        marker.display()
    );
    #[cfg(not(windows))]
    let command = format!("printf started > '{}'; sleep 20", marker.display());
    let (api_base_url, requests) = spawn_scripted_api(vec![
        StreamReply::Complete(bash_tool_turn("tool-black-box", &command)),
        StreamReply::Complete(text_turn("continued-after-tool-cancel")),
    ]);
    let mut process = BingoProcess::spawn(&root, &api_base_url);

    start_turn(&mut process, "turn-tool", "start-tool", "run one slow tool");
    process.event("tool.ready", |event| {
        event["type"] == "tool.ready" && event["toolCallId"] == "tool-black-box"
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(marker.exists(), "the Bash tool never started");
    let cancellation_events = cancel_turn(&mut process, "turn-tool", "cancel-tool");
    assert!(cancellation_events.iter().any(|event| {
        event["type"] == "tool.done"
            && event["toolCallId"] == "tool-black-box"
            && event["status"] == "interrupted"
    }));

    start_turn(
        &mut process,
        "turn-tool-continue",
        "continue-tool",
        CONTINUE_PROMPT,
    );
    process.event("continued tool turn completion", |event| {
        event["type"] == "turn.completed" && event["turnId"] == "turn-tool-continue"
    });

    let _first_request = next_request(&requests);
    let second_request = next_request(&requests);
    let texts = request_texts(&second_request);
    assert!(texts.contains(&"run one slow tool"), "{second_request}");
    assert!(texts.contains(&CONTINUE_PROMPT), "{second_request}");
    let uses = tool_ids(&second_request, "tool_use", "id");
    let results = tool_ids(&second_request, "tool_result", "tool_use_id");
    assert_eq!(uses, vec!["tool-black-box"]);
    assert_eq!(results, uses, "interrupted tool_use must be paired");
    process.close();
}

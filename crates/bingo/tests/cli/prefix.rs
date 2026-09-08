//! Consecutive wire requests through the real binary and built-in contributors.

use super::*;
use bingo_sdk::{ContentPart, ItemBody};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn event(value: Value) -> String {
    format!(
        "event: {}\ndata: {value}\n\n",
        value["type"].as_str().unwrap()
    )
}

fn tool_response(id: &str, name: &str, input: Value) -> String {
    let item = json!({
        "id": id, "type": "function_call", "call_id": id,
        "name": name, "arguments": input.to_string(), "status": "completed"
    });
    [
        event(json!({"type": "response.output_item.added", "item": item})),
        event(json!({"type": "response.output_item.done", "item": item})),
        event(json!({"type": "response.completed", "response": {
            "status": "completed", "usage": {"input_tokens": 100, "output_tokens": 1}
        }})),
    ]
    .concat()
}

async fn responding(server: &MockServer, response: String) {
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(response, "text/event-stream"))
        .up_to_n_times(1)
        .expect(1)
        .mount(server)
        .await;
}

async fn scenario(server: &MockServer) {
    for (id, name, input) in [
        ("create", "TaskCreate", json!({"subject": "Inspect cache"})),
        (
            "finish",
            "TaskUpdate",
            json!({"id": 1, "status": "completed"}),
        ),
        (
            "remember",
            "Write",
            json!({
                "file_path": ".bingo/data/memory/user/MEMORY.md",
                "content": "- [New fact](new.md) — newly remembered\n"
            }),
        ),
        (
            "instructions",
            "Write",
            json!({"file_path": "AGENTS.md", "content": "New project rule.\n"}),
        ),
        ("read", "Read", json!({"file_path": "notes.txt"})),
    ] {
        responding(server, tool_response(id, name, input)).await;
    }
    responding(server, responses_fixture("text.sse")).await;
    responding(server, responses_fixture("text.sse")).await;
}

fn snapshots(frames: &[Frame], id: &str) -> Vec<String> {
    frames
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::User { parts, origin }
                    if origin.surface.strip_prefix(bingo_sdk::CONTRIBUTOR_PREFIX) == Some(id) =>
                {
                    Some(parts.iter().filter_map(ContentPart::as_text).collect())
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn assert_prefixes(requests: &[Value]) {
    assert_eq!(requests.len(), 7);
    let key = requests[0]["prompt_cache_key"]
        .as_str()
        .expect("session affinity");
    for (index, pair) in requests.windows(2).enumerate() {
        if index < 5 {
            assert!(
                pair[0]["instructions"] == pair[1]["instructions"],
                "baseline changed within the session"
            );
        }

        assert!(
            pair[0]["tools"] == pair[1]["tools"],
            "tool definitions changed"
        );
        assert_eq!(pair[1]["prompt_cache_key"], key);
        let before = pair[0]["input"].as_array().unwrap();
        let after = pair[1]["input"].as_array().unwrap();
        assert!(after.starts_with(before), "already-sent wire input changed");
    }
    for request in &requests[..6] {
        let system = request["instructions"].as_str().unwrap();
        assert!(system.contains("Old project rule."));
        assert!(system.contains("previously remembered"));
        assert!(!system.contains("New project rule."));
        assert!(!system.contains("newly remembered"));
    }
    let resumed = requests[6]["instructions"].as_str().unwrap();
    assert!(resumed.contains("New project rule."));
    assert!(resumed.contains("newly remembered"));
    assert!(!resumed.contains("Old project rule."));
    assert!(!resumed.contains("previously remembered"));
}

async fn complete(command: Command) -> std::process::Output {
    tokio::time::timeout(
        std::time::Duration::from_secs(60),
        tokio::process::Command::from(command)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .expect("CLI did not finish; possible actor deadlock")
    .expect("CLI could not run")
}

#[tokio::test]
async fn context_snapshots_cannot_name_a_session_started_by_a_skill() {
    let server = responses_server(&["text.sse", "text.sse"]).await;
    let home = tempfile::tempdir().unwrap();
    let command = openai(&server, home.path(), "/guide");
    let first = complete(command).await;
    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));

    let mut command = openai(&server, home.path(), "Fix the parser");
    command.args(["--continue", "--output-format", "json"]);
    let next = complete(command).await;
    assert_eq!(next.status.code(), Some(0), "{}", stderr(&next));
    let mut command = openai(&server, home.path(), "/rename");
    command.arg("--continue");
    let named = complete(command).await;
    assert_eq!(named.status.code(), Some(0), "{}", stderr(&named));
    assert!(
        stdout(&named).contains("name: Fix the parser"),
        "{}",
        stdout(&named)
    );
}

#[tokio::test]
async fn knowledge_stays_fixed_between_rounds_and_refreshes_when_resumed() {
    let server = MockServer::start().await;
    scenario(&server).await;
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("notes.txt"), "notes\n").unwrap();
    std::fs::write(home.path().join("AGENTS.md"), "Old project rule.\n").unwrap();
    let memory = home.path().join(".bingo/data/memory/user");
    std::fs::create_dir_all(&memory).unwrap();
    std::fs::write(
        memory.join("MEMORY.md"),
        "- [Old fact](old.md) — previously remembered\n",
    )
    .unwrap();
    let mut command = openai(&server, home.path(), "Inspect the state.");
    command.args([
        "--output-format",
        "json",
        "--permission-mode",
        "bypassPermissions",
    ]);
    let out = complete(command).await;
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let frames = frames_of(&out);
    assert!(frames.iter().any(|f| matches!(
        f.event,
        Event::TurnCompleted {
            status: TurnStatus::Completed,
            ..
        }
    )));
    let calls: Vec<_> = frames
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::ToolCall {
                    name,
                    output: Some(output),
                    ..
                } => {
                    assert!(!output.is_error, "{name}: {:?}", output.parts);
                    Some(name.as_str())
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        ["TaskCreate", "TaskUpdate", "Write", "Write", "Read"]
    );
    assert!(
        snapshots(&frames, "context:memory").is_empty(),
        "memory must not be appended as dialogue"
    );
    let tasks = snapshots(&frames, "tasks");
    assert!(tasks.iter().any(|text| text.contains("Inspect cache")));
    assert!(
        !tasks.last().unwrap().contains("Inspect cache"),
        "completion clears the reminder"
    );

    let mut command = openai(&server, home.path(), "Continue.");
    command.args(["--continue", "--output-format", "json"]);
    let resumed = complete(command).await;
    assert_eq!(resumed.status.code(), Some(0), "{}", stderr(&resumed));
    let requests: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/v1/responses")
        .map(|r| r.body_json().unwrap())
        .collect();
    assert_prefixes(&requests);
}

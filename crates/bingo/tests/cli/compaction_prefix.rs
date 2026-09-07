//! Shared-prefix summarization and baseline refresh through the real binary.

use super::stream_json::{Ended, Host};
use super::*;
use serde_json::Value;

fn conversation(
    server: &wiremock::MockServer,
    home: &std::path::Path,
    settings: &std::path::Path,
) -> Command {
    let mut command = bingo();
    command
        .env("OPENAI_API_KEY", "sk-test")
        .env("OPENAI_BASE_URL", server.uri())
        .env("HOME", home)
        .args([
            "--print",
            "--provider",
            "openai",
            "--model",
            "gpt-5.4",
            "--input-format",
            "stream-json",
            "--output-format",
            "json",
            "--cwd",
        ])
        .arg(home)
        .arg("--settings")
        .arg(settings);
    command
}

fn ask(host: &mut Host, text: &str) {
    host.prompt(text);
    let completed = host.until_event("turnCompleted");
    assert_eq!(
        completed["event"]["status"]["kind"], "completed",
        "{completed}"
    );
}

fn run_compaction(mut command: Command, root: std::path::PathBuf) -> Ended {
    let mut host = Host::start(&mut command);
    for step in 0..8 {
        ask(
            &mut host,
            &format!("Step {step}: {}", "Keep the project history. ".repeat(100)),
        );
    }
    std::fs::write(root.join("AGENTS.md"), "New project baseline.\n").unwrap();
    ask(&mut host, "/compact");
    ask(&mut host, "Continue after compaction");
    host.finish()
}

fn assert_summary_prefix(requests: &[Value]) {
    assert_eq!(
        requests.len(),
        10,
        "eight normal requests, summary, then continuation"
    );
    let before = &requests[7];
    let summary = &requests[8];
    for key in ["instructions", "tools", "reasoning", "prompt_cache_key"] {
        assert!(summary[key] == before[key], "summary changed {key}");
    }
    let history = before["input"].as_array().unwrap();
    let input = summary["input"].as_array().unwrap();
    assert!(
        input.starts_with(history),
        "summary rewrote already-sent history"
    );
    assert!(
        input
            .last()
            .unwrap()
            .to_string()
            .contains("compacting the agent conversation")
    );
    assert!(
        summary["instructions"]
            .as_str()
            .unwrap()
            .contains("Old project baseline.")
    );
    assert!(
        summary.get("bingo").is_none(),
        "internal purpose leaked to provider wire"
    );
}

async fn scenario(fixtures: &[&str]) -> (Ended, Vec<Value>) {
    let server = responses_server(fixtures).await;
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("AGENTS.md"), "Old project baseline.\n").unwrap();
    let settings = script(r#"{"context":{"memory":false}}"#);
    let command = conversation(&server, home.path(), settings.path());
    let root = home.path().to_path_buf();
    let ended = tokio::task::spawn_blocking(move || run_compaction(command, root))
        .await
        .unwrap();
    assert_eq!(ended.code, Some(0), "{}", ended.err);
    let requests: Vec<Value> = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path() == "/v1/responses")
        .map(|request| request.body_json().unwrap())
        .collect();
    assert_summary_prefix(&requests);
    (ended, requests)
}

#[tokio::test]
async fn manual_summary_uses_the_old_baseline_and_next_request_refreshes_it() {
    let (ended, requests) = scenario(&["text.sse"; 10]).await;
    assert!(
        ended
            .lines
            .iter()
            .any(|frame| frame["event"]["type"] == "compacted")
    );
    let after = &requests[9];
    assert!(
        after["instructions"]
            .as_str()
            .unwrap()
            .contains("New project baseline.")
    );
    assert!(
        !after["instructions"]
            .as_str()
            .unwrap()
            .contains("Old project baseline.")
    );
    assert!(
        after["input"].as_array().unwrap().len() < requests[8]["input"].as_array().unwrap().len()
    );
}

#[tokio::test]
async fn rejected_summary_neither_executes_tools_nor_refreshes_the_baseline() {
    let mut fixtures = ["text.sse"; 10];
    fixtures[8] = "tools.sse";
    let (ended, requests) = scenario(&fixtures).await;
    assert!(
        !ended
            .lines
            .iter()
            .any(|frame| frame["event"]["type"] == "compacted")
    );
    assert!(
        !ended
            .lines
            .iter()
            .any(|frame| frame["event"]["item"]["body"]["kind"] == "toolCall")
    );
    let after = &requests[9];
    assert!(after["instructions"] == requests[7]["instructions"]);
    assert!(
        after["instructions"]
            .as_str()
            .unwrap()
            .contains("Old project baseline.")
    );
    assert!(
        after["input"]
            .as_array()
            .unwrap()
            .starts_with(requests[7]["input"].as_array().unwrap())
    );
}

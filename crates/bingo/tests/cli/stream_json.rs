//! The Claude Code envelope (ADR-0007 §8): a host that drives
//! `claude -p --output-format stream-json` drives bingo the same way.

use super::*;

fn lines_of(out: &Output) -> Vec<serde_json::Value> {
    stdout(out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

#[test]
fn stream_json_is_init_then_messages_then_result_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "alpha\n").unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"text":"Looking."},{"toolCall":{"name":"Read","input":{"file_path":"notes.txt"}}}]},
            {"steps":[{"text":"One line."}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", dir.path())
        .args(["--print", "--output-format", "stream-json", "--cwd"])
        .arg(dir.path())
        .arg("what is in notes.txt?"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let lines = lines_of(&out);
    let types: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    assert_eq!(
        types,
        [
            "system",
            "assistant",
            "assistant",
            "user",
            "assistant",
            "result"
        ],
        "{}",
        stdout(&out)
    );
    assert_eq!(lines[0]["subtype"], "init");
    assert!(
        lines[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "Read")
    );
    let session = lines[0]["session_id"].as_str().unwrap();
    assert!(lines.iter().all(|l| l["session_id"] == session));
    assert_eq!(lines[2]["message"]["content"][0]["type"], "tool_use");
    assert_eq!(lines[3]["message"]["content"][0]["type"], "tool_result");
    let result = lines.last().unwrap();
    assert_eq!(result["subtype"], "success");
    assert_eq!(result["is_error"], false);
    assert_eq!(result["result"], "One line.");
    assert_eq!(result["num_turns"], 2);
}

#[test]
fn a_failed_turn_is_a_result_line_with_errors() {
    let script =
        script(r#"{"responses":[{"steps":[{"error":{"kind":"auth","message":"bad key"}}]}]}"#);
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", tempfile::tempdir().unwrap().path())
        .args(["--print", "--output-format", "stream-json", "hello"]));
    assert_eq!(out.status.code(), Some(1));
    let lines = lines_of(&out);
    let result = lines.last().unwrap();
    assert_eq!(result["type"], "result");
    assert_eq!(result["subtype"], "error_during_execution");
    assert_eq!(result["is_error"], true);
    assert!(
        result["errors"][0].as_str().unwrap().contains("bad key"),
        "{result}"
    );
    assert!(
        result.get("result").is_none(),
        "no result text on the error arm"
    );
}

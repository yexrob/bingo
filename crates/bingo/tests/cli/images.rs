//! A picture beside a headless prompt (ADR-0040): `--image` on the command
//! line, and an `image` block on a stream-json user line. What is asserted
//! is the journal — the one place the picture is kept — not the stream,
//! which does not echo a person's own prompt.

use serde_json::{Value, json};

use super::*;
use crate::stream_json::{Host, hosted};

/// A tiny file that the extension table takes for a picture.
fn shot(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("shot.png");
    std::fs::write(&path, b"png").unwrap();
    path
}

/// Every journal line under the run's home: one session, one prompt.
fn journal(home: &std::path::Path) -> Vec<Value> {
    let sessions = home.join(".bingo/data/sessions");
    let dir = std::fs::read_dir(&sessions)
        .expect("the run wrote a session")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.join("journal.jsonl").exists())
        .expect("one session");
    std::fs::read_to_string(dir.join("journal.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The parts of the first user item the journal holds.
fn first_ask(home: &std::path::Path) -> Vec<Value> {
    journal(home)
        .into_iter()
        .filter_map(|line| {
            let item = line.pointer("/event/item")?;
            (item["body"]["kind"] == "user").then(|| item["body"]["parts"].clone())
        })
        .next()
        .and_then(|parts| parts.as_array().cloned())
        .expect("a user item in the journal")
}

#[test]
fn an_image_flag_puts_the_picture_beside_the_prompt_in_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let path = shot(dir.path());
    let script = script(r#"{"responses":[{"steps":[{"text":"A picture."}]}]}"#);
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", dir.path())
        .args(["--print", "--cwd"])
        .arg(dir.path())
        .arg("--image")
        .arg(&path)
        .arg("what is this?"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "A picture.");
    let parts = first_ask(dir.path());
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["text"], "what is this?");
    assert_eq!(parts[1]["type"], "image");
    assert_eq!(parts[1]["mediaType"], "image/png");
    assert_eq!(parts[1]["data"], "cG5n");
}

#[test]
fn an_image_that_does_not_read_is_exit_1_with_nothing_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(r#"{"responses":[{"steps":[{"text":"never"}]}]}"#);
    let missing = dir.path().join("missing.png");
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", dir.path())
        .args(["--print", "--cwd"])
        .arg(dir.path())
        .arg("--image")
        .arg(&missing)
        .arg("look"));
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).is_empty(), "stdout: {}", stdout(&out));
    assert!(
        stderr(&out).contains("missing.png"),
        "stderr names the path: {}",
        stderr(&out)
    );
}

#[test]
fn an_image_flag_under_stream_json_input_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = shot(dir.path());
    let out = run(bingo()
        .env("HOME", dir.path())
        .args(["--print", "--input-format", "stream-json", "--cwd"])
        .arg(dir.path())
        .arg("--image")
        .arg(&path));
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).is_empty());
    assert!(stderr(&out).contains("--image"), "{}", stderr(&out));
}

#[test]
fn a_stream_json_user_line_carries_its_image_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(r#"{"responses":[{"steps":[{"text":"Seen."}]}]}"#);
    let mut host = Host::start(&mut hosted(dir.path(), &script, &[]));
    host.send(&json!({
        "type": "user",
        "message": { "role": "user", "content": [
            { "type": "text", "text": "and this one" },
            { "type": "image", "source": {
                "type": "base64", "media_type": "image/jpeg", "data": "/9j/" } }
        ] },
        "parent_tool_use_id": Value::Null,
    }));
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    assert_eq!(ended.results()[0]["result"], "Seen.");
    let parts = first_ask(dir.path());
    assert_eq!(parts[0]["text"], "and this one");
    assert_eq!(parts[1]["type"], "image");
    assert_eq!(parts[1]["mediaType"], "image/jpeg");
}

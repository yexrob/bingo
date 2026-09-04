//! The bang line as a host drives it (M65): `!<command>` runs the shell there
//! and then, journals one shell item, and spends no model turn on it.
//!
//! Every run here is scripted with **no** responses at all, which is what
//! makes "no model request" an assertion rather than a hope: a request would
//! be answered with `script exhausted` and the run would say so.

use serde_json::{Value, json};

use super::stream_json::Host;
use super::*;

/// A run driven by a host, with a fake provider that can answer nothing.
fn no_answers(dir: &std::path::Path, format: &str) -> (tempfile::NamedTempFile, Command) {
    let script = script(r#"{"responses":[]}"#);
    let mut cmd = bingo();
    cmd.env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", dir)
        .args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            format,
            "--cwd",
        ])
        .arg(dir);
    (script, cmd)
}

/// Every completed item of a frame stream, as its body.
fn items(lines: &[Value]) -> Vec<Value> {
    lines
        .iter()
        .filter(|line| line["event"]["type"] == "itemCompleted")
        .map(|line| line["event"]["item"]["body"].clone())
        .collect()
}

/// The one thing a `!` line leaves behind is one shell item — the line, what
/// it wrote, the code it came to and where it ran — and no turn is opened for
/// it, so the provider is never asked anything.
#[test]
fn a_bang_line_journals_one_shell_item_and_spends_no_turn() {
    let dir = tempfile::tempdir().unwrap();
    let (_script, mut cmd) = no_answers(dir.path(), "json");
    let mut host = Host::start(&mut cmd);
    host.prompt("!echo hi");
    host.until_event("intentAck");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let bodies = items(&ended.lines);
    assert_eq!(bodies.len(), 1, "one item and no other: {bodies:#?}");
    assert_eq!(bodies[0]["kind"], "shell");
    assert_eq!(bodies[0]["command"], "echo hi");
    assert_eq!(bodies[0]["output"], "hi\n");
    assert_eq!(bodies[0]["exit"], 0);
    assert_eq!(bodies[0]["cwd"], json!(dir.path()));
    assert!(
        !ended
            .lines
            .iter()
            .any(|line| line["event"]["type"] == "turnStarted"),
        "no turn opened, so the model was never asked: {:#?}",
        ended.lines
    );
}

/// The same line through the Claude Code envelope: it is a user message,
/// because a user message is exactly what the model reads next turn.
#[test]
fn a_bang_line_reaches_a_host_as_the_user_message_it_becomes() {
    let dir = tempfile::tempdir().unwrap();
    let (_script, mut cmd) = no_answers(dir.path(), "stream-json");
    let mut host = Host::start(&mut cmd);
    host.prompt("!echo hi && false");
    host.until("user");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    assert_eq!(
        ended.types(),
        ["system", "user"],
        "no assistant and no result: {:#?}",
        ended.lines
    );
    assert_eq!(
        ended.lines[1]["message"]["content"][0]["text"],
        "$ echo hi && false\n```\nhi\n```\n[exit 1]"
    );
}

/// A bang with nothing after it has nothing to run: one refusal, nothing
/// journaled, and the run reports it the way it reports any failure.
#[test]
fn a_bang_with_nothing_after_it_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (_script, mut cmd) = no_answers(dir.path(), "json");
    let mut host = Host::start(&mut cmd);
    host.prompt("!   ");
    let ack = host.until_event("intentAck");
    assert_eq!(ack["event"]["outcome"]["kind"], "rejected");
    assert_eq!(ack["event"]["outcome"]["error"]["code"], "INVALID_INPUT");
    let ended = host.finish();
    assert_eq!(ended.code, Some(1), "a refused line is a failed run");
    assert!(ended.err.contains("code=INVALID_INPUT"), "{}", ended.err);
    assert!(items(&ended.lines).is_empty(), "nothing was journaled");
}

//! Background shell jobs as a host sees them (ADR-0018): a job started and
//! pulled by cursor across two turns, a kill that says how it ended, a command
//! backgrounded unbidden, a completion that opens a turn on a headless run,
//! and a running command a person moves into the background mid-turn.
//!
//! Every job id starts with `job_`, so a script that has started exactly one
//! job can name it by that prefix — which is what the model does with the
//! whole id once it has read the result.

use serde_json::Value;

use super::stream_json::{Host, hosted};
use super::*;

/// A hosted run that may spend a shell: the gate is not what these are about.
fn allowed(dir: &std::path::Path, script: &tempfile::NamedTempFile) -> Command {
    hosted(dir, script, &["--dangerously-skip-permissions"])
}

/// The text of the last completed call of `name`.
fn tool_result(lines: &[Value], name: &str) -> String {
    let call = lines
        .iter()
        .filter(|line| line["message"]["content"][0]["name"] == name)
        .map(|line| line["message"]["content"][0]["id"].clone())
        .next_back()
        .unwrap_or_else(|| panic!("no {name} call: {lines:#?}"));
    lines
        .iter()
        .find(|line| line["message"]["content"][0]["tool_use_id"] == call)
        .and_then(|line| line["message"]["content"][0]["content"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("the {name} call never came back: {lines:#?}"))
}

/// Every `tool_result` the run wrote, in order.
fn results_of(lines: &[Value]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line["message"]["content"][0]["type"] == "tool_result")
        .filter_map(|line| line["message"]["content"][0]["content"].as_str())
        .map(str::to_string)
        .collect()
}

/// What a `BashOutput` read holds between its `$ <command>` header and the
/// `[job …]` line under it, which is the output and nothing else.
fn read_body(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines
        .get(1..lines.len().saturating_sub(1))
        .unwrap_or_default()
        .join("\n")
}

/// Every log a run left behind.
fn logs(home: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = std::fs::read_dir(home.join(".bingo/data/bash"))
        .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    paths.sort();
    paths
}

fn log_text(home: &std::path::Path) -> String {
    let paths = logs(home);
    assert_eq!(paths.len(), 1, "one job, one log: {paths:?}");
    std::fs::read_to_string(&paths[0]).unwrap_or_default()
}

/// Turn one starts a job and reads the head of it; turn two reads on from the
/// cursor the first read gave back, then ends the job. The last response is
/// for the turn the kill's own notification opens.
///
/// The job's second line waits for a file the test makes, so what each pull
/// finds is the test's to decide and not the machine's load.
const PULL_THEN_KILL: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"Bash","input":{
        "command":"echo one; while [ ! -f go ]; do sleep 0.05; done; echo two; sleep 30",
        "background":true}}}]},
    {"steps":[{"delay":{"ms":400}},{"toolCall":{"name":"BashOutput","input":{"id":"job_"}}}]},
    {"steps":[{"text":"started it"}]},
    {"steps":[{"toolCall":{"name":"BashOutput","input":{"id":"job_","cursor":4}}}]},
    {"steps":[{"toolCall":{"name":"KillShell","input":{"id":"job_"}}}]},
    {"steps":[{"text":"read it and ended it"}]},
    {"steps":[{"text":"and I heard it end"}]}
]}"#;

#[test]
fn a_job_is_pulled_by_cursor_across_two_turns_and_then_killed() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(PULL_THEN_KILL);
    let mut host = Host::start(&mut allowed(dir.path(), &script));

    host.prompt("start it and read the first of it");
    host.until("result");
    // Only now may the job write its second line, so the cursor pull of the
    // next turn has something new to find and the first pull had not.
    std::fs::write(dir.path().join("go"), "").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(400));
    host.prompt("read the rest and stop it");
    host.until("result");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let started = tool_result(&ended.lines, "Bash");
    assert!(started.contains("job_"), "no job id: {started}");
    assert!(started.contains("BashOutput"), "{started}");

    let reads: Vec<String> = results_of(&ended.lines)
        .iter()
        .filter(|text| text.starts_with("$ echo one"))
        .map(|text| read_body(text))
        .collect();
    assert_eq!(reads.len(), 2, "two pulls: {reads:#?}");
    assert_eq!(reads[0], "one", "the first pull ran ahead or fell short");
    assert_eq!(reads[1], "two", "the cursor did not hold its place");
    let first = results_of(&ended.lines)
        .into_iter()
        .find(|text| text.starts_with("$ echo one"))
        .unwrap_or_default();
    assert!(first.contains("cursor 4"), "{first}");

    let killed = tool_result(&ended.lines, "KillShell");
    assert!(killed.contains("killed"), "{killed}");
    assert!(
        killed.contains("BashOutput"),
        "the log outlives it: {killed}"
    );

    // The file is the one representation: it holds everything both reads saw.
    assert_eq!(
        log_text(dir.path()),
        "one\ntwo\n",
        "the log is short of what the job wrote"
    );
}

/// A command that could never finish is backgrounded whatever the call said,
/// and an ordinary one is still waited for.
const TAIL_THEN_ECHO: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"Bash","input":{"command":"tail -f /etc/hosts","timeout":300}}}]},
    {"steps":[{"toolCall":{"name":"KillShell","input":{"id":"job_"}}}]},
    {"steps":[{"toolCall":{"name":"Bash","input":{"command":"echo plain"}}}]},
    {"steps":[{"text":"both"}]},
    {"steps":[{"text":"and I heard it end"}]}
]}"#;

#[test]
fn a_tail_f_backgrounds_itself_and_a_plain_command_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(TAIL_THEN_ECHO);
    let mut host = Host::start(&mut allowed(dir.path(), &script));
    host.prompt("follow the hosts file, then say hello");
    host.until("result");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let shells: Vec<String> = results_of(&ended.lines)
        .into_iter()
        .filter(|text| text.starts_with("Started `tail -f") || text.starts_with("$ echo plain"))
        .collect();
    assert_eq!(shells.len(), 2, "{shells:#?}");

    let followed = &shells[0];
    assert!(
        followed.contains("backgrounded although the call did not ask"),
        "{followed}"
    );
    assert!(followed.contains("`tail -f`"), "the reason: {followed}");
    assert!(followed.contains("job_"), "{followed}");

    let plain = &shells[1];
    assert!(
        plain.starts_with("$ echo plain\nplain\n[Exited with code 0]"),
        "an ordinary command was not waited for: {plain}"
    );
    assert_eq!(logs(dir.path()).len(), 1, "only the follow became a job");
}

/// The wake the old project could not do: the job ends after the turn that
/// started it has finished, and the notification opens a turn of its own on a
/// headless run.
const START_THEN_HEAR: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"Bash","input":{
        "command":"sleep 0.4; echo done","background":true}}}]},
    {"steps":[{"text":"started"}]},
    {"steps":[{"text":"the job finished"}]}
]}"#;

#[test]
fn a_finished_job_opens_a_turn_on_a_headless_run() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(START_THEN_HEAR);
    let mut host = Host::start(&mut allowed(dir.path(), &script));

    host.prompt("start it and tell me when it is done");
    let first = host.until("result");
    assert_eq!(first["result"], "started");

    // Nothing more is sent: only the job's end can open the next turn.
    std::thread::sleep(std::time::Duration::from_secs(3));
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let results = ended.results();
    assert_eq!(
        results.len(),
        2,
        "the job's end opened no turn: {:?}",
        ended.types()
    );
    assert_eq!(results[1]["result"], "the job finished");
    assert_eq!(
        results[0]["session_id"], results[1]["session_id"],
        "the wake landed on the session that started the job"
    );
    let log = log_text(dir.path());
    assert_eq!(log, "done\n", "{log}");
    assert!(
        !log.contains("nobody was told"),
        "the notification reached nobody: {log}"
    );
}

/// A person's `ctrl+b` is this command with the running call's id; the TUI
/// fires it as an action and a host types it, and both are the one door.
const PROMOTED: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"Bash","input":{"command":"echo before; sleep 30"}}}]},
    {"steps":[{"toolCall":{"name":"KillShell","input":{"id":"job_"}}}]},
    {"steps":[{"text":"it is in the background now"}]},
    {"steps":[{"text":"and I heard it end"}]}
]}"#;

#[test]
fn a_running_command_is_promoted_mid_turn_and_the_call_returns_early() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(PROMOTED);
    let mut host = Host::start(&mut allowed(dir.path(), &script));

    let started = std::time::Instant::now();
    host.prompt("run the slow thing");
    // The call is announced as the assistant asking for it, which is how a
    // surface knows which call `ctrl+b` is about.
    let asked = host.until("assistant");
    assert_eq!(asked["message"]["content"][0]["name"], "Bash");
    let call = asked["message"]["content"][0]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("the call id: {asked}"))
        .to_string();

    host.prompt(&format!("/bash.promote {call}"));
    let result = host.until("result");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(25),
        "the call waited for the command anyway"
    );
    assert_eq!(result["result"], "it is in the background now");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let promoted = tool_result(&ended.lines, "Bash");
    assert!(promoted.contains("moved into the background"), "{promoted}");
    assert!(promoted.contains("job_"), "no job id: {promoted}");
    assert!(promoted.contains("no timeout"), "{promoted}");

    // The same process carried on: what it wrote before the promotion is at
    // the head of the log it writes now.
    let log = log_text(dir.path());
    assert!(
        log.starts_with("before"),
        "the buffer did not follow: {log}"
    );
}

#[test]
fn an_id_no_job_has_is_an_error_result_the_model_can_correct() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"BashOutput","input":{"id":"job_nothing"}}}]},
            {"steps":[{"text":"there was none"}]}
        ]}"#,
    );
    let out = scripted_run(
        dir.path(),
        &script,
        &["--dangerously-skip-permissions"],
        "read a job that is not there",
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let said = frames_of(&out)
        .into_iter()
        .find_map(|f| match f.event {
            Event::ItemCompleted { item } => match item.body {
                bingo_sdk::ItemBody::ToolCall {
                    name,
                    output: Some(output),
                    ..
                } if name == "BashOutput" => Some(output),
                _ => None,
            },
            _ => None,
        })
        .expect("the BashOutput call completed");
    assert!(said.is_error);
    let text: String = said
        .parts
        .iter()
        .filter_map(bingo_sdk::ContentPart::as_text)
        .collect();
    assert!(text.contains("no job is called `job_nothing`"), "{text}");
    assert!(
        text.contains("No shell command has been backgrounded"),
        "{text}"
    );
}

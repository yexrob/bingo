//! Background shell jobs as a host sees them (ADR-0018): a job started and
//! pulled by cursor across two turns, a kill that says how it ended, a command
//! backgrounded unbidden, a completion that opens a turn on a headless run,
//! and a running command a person moves into the background mid-turn.
//!
//! Every job id starts with `job_`, so a script that has started exactly one
//! job can name it by that prefix — which is what the model does with the
//! whole id once it has read the result.

use serde_json::Value;

use super::stream_json::{Ended, Host, hosted};
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

/// Every byte journaled under a run's data dir. A wake's own text is what the
/// session heard, not something the stream prints, so this is where an
/// ongoing watch's notices are read back from (the precedent is `agents.rs`).
fn journal_text(dir: &std::path::Path) -> String {
    let mut text = String::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return text;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            text.push_str(&journal_text(&path));
        } else if let Ok(contents) = std::fs::read_to_string(&path) {
            text.push_str(&contents);
        }
    }
    text
}

/// A job that writes one matching line, waits, then bursts three more and
/// ends. Both waits are on files the test makes, so every notice lands
/// between two turns and no scan tick has to win a race for the test to mean
/// what it says.
fn gated(notify_all: bool) -> tempfile::NamedTempFile {
    let all = if notify_all {
        r#""notify_all":true,"#
    } else {
        ""
    };
    script(&format!(
        r#"{{"responses":[
            {{"steps":[{{"toolCall":{{"name":"Bash","input":{{
                "command":"echo warming; while [ ! -f start ]; do sleep 0.05; done; echo HIT one; while [ ! -f go ]; do sleep 0.05; done; echo HIT two; echo HIT three; echo HIT four",
                "background":true,{all}"notify_on":["HIT"]}}}}}}]}},
            {{"steps":[{{"text":"started it"}}]}},
            {{"steps":[{{"text":"heard the first hit"}}]}},
            {{"steps":[{{"text":"heard it finish"}}]}}
        ]}}"#
    ))
}

/// Drive `gated`: the job's first hit only happens once the turn that started
/// it is over, and the burst only once that hit's own turn is over.
fn run_gated(dir: &std::path::Path, script: &tempfile::NamedTempFile) -> Ended {
    let mut host = Host::start(&mut allowed(dir, script));
    host.prompt("watch it and tell me what it writes");
    assert_eq!(host.until("result")["result"], "started it");

    std::fs::write(dir.join("start"), "").unwrap();
    assert_eq!(host.until("result")["result"], "heard the first hit");

    std::fs::write(dir.join("go"), "").unwrap();
    assert_eq!(host.until("result")["result"], "heard it finish");
    host.finish()
}

/// The tail of the clause an ongoing watch adds for what its quiet window
/// swallowed, singular and plural alike.
const SINCE: &str = "matched since the last notice";

/// `notify_all` (ADR-0018 §8): the first hit wakes at once, the three that
/// follow inside the thirty-second window are only counted, and the count
/// rides the completion — one line and a number, never a list.
#[test]
fn an_ongoing_watch_wakes_once_and_counts_the_rest_onto_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let ended = run_gated(dir.path(), &gated(true));
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    assert_eq!(
        ended.results().len(),
        3,
        "the hit and the end are one turn each: {:?}",
        ended.types()
    );

    let journal = journal_text(&dir.path().join(".bingo/data"));
    assert!(
        journal.contains("wrote a line you asked to be told about"),
        "the first hit never woke the session"
    );
    assert!(
        journal.contains("It matched: HIT four"),
        "the completion carried no pending tally"
    );
    assert!(
        journal.contains(&format!("…and 2 more lines {SINCE}.")),
        "the two lines the window swallowed were not counted"
    );
    assert_eq!(
        journal.matches(SINCE).count(),
        1,
        "the count went out once and was reset by the notice that carried it"
    );
    // One line per notice: the swallowed lines are in the log, not the wake.
    assert_eq!(
        log_text(dir.path()),
        "warming\nHIT one\nHIT two\nHIT three\nHIT four\n"
    );
}

/// The default is unchanged (ADR-0018 §4): one notice for the first hit, and
/// the three lines after it are neither delivered nor counted.
#[test]
fn a_default_watch_still_says_it_once_and_counts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let ended = run_gated(dir.path(), &gated(false));
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    assert_eq!(ended.results().len(), 3, "{:?}", ended.types());

    let journal = journal_text(&dir.path().join(".bingo/data"));
    assert_eq!(
        journal
            .matches("wrote a line you asked to be told about")
            .count(),
        1,
        "one notification, not a storm"
    );
    assert!(!journal.contains(SINCE), "the default counts nothing");
    assert!(
        !journal.contains("It matched:"),
        "a fired watch has nothing left for the completion to carry"
    );
}

/// `notify_all` with nothing to watch for is refused before anything runs,
/// with the wording that says what to add (ADR-0018 §8).
#[test]
fn notify_all_with_no_condition_is_refused_and_starts_no_job() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Bash","input":{
                "command":"echo hi","background":true,"notify_all":true}}}]},
            {"steps":[{"text":"it was refused"}]}
        ]}"#,
    );
    let out = scripted_run(
        dir.path(),
        &script,
        &["--dangerously-skip-permissions"],
        "watch everything, but say what for",
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let refused = frames_of(&out)
        .into_iter()
        .find_map(|f| match f.event {
            Event::ItemCompleted { item } => match item.body {
                bingo_sdk::ItemBody::ToolCall {
                    name,
                    output: Some(output),
                    ..
                } if name == "Bash" => Some(output),
                _ => None,
            },
            _ => None,
        })
        .expect("the Bash call completed");
    assert!(refused.is_error);
    let text: String = refused
        .parts
        .iter()
        .filter_map(bingo_sdk::ContentPart::as_text)
        .collect();
    assert!(text.contains("notify_all watches nothing"), "{text}");
    assert!(text.contains("notify_on"), "{text}");
    assert!(text.contains("notify_regex"), "{text}");
    assert!(logs(dir.path()).is_empty(), "a refused call started a job");
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

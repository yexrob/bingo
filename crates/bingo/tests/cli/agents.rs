//! A sub-agent is a child session (ADR-0010): `SpawnAgent` in the foreground
//! hands the child's own text back as the call's result, and the run's stdout
//! stays the root's prose. A team is the roles `.bingo/team.json` declares,
//! seated as children of the root when it opens (ADR-0011). An agent writes
//! to the teammate beside it and every message wakes its target (ADR-0024);
//! a post written behind a room's head bounces instead of landing (ADR-0025).

use std::path::{Path, PathBuf};

use bingo_sdk::{ContentPart, ItemBody, Origin, SessionId, ToolOutput};
use serde_json::Value;

use super::*;

/// The text a completed call to `tool` returned, as the model read it.
fn tool_output(out: &Output, tool: &str) -> String {
    frames_of(out)
        .into_iter()
        .filter_map(|f| match f.event {
            Event::ItemCompleted { item } => match item.body {
                bingo_sdk::ItemBody::ToolCall { name, output, .. } if name == tool => output,
                _ => None,
            },
            _ => None,
        })
        .next_back()
        .unwrap_or_else(|| panic!("no {tool} call completed: {}", stdout(out)))
        .parts
        .iter()
        .filter_map(bingo_sdk::ContentPart::as_text)
        .collect()
}

/// The last thing the run's session said.
fn final_text(out: &Output) -> String {
    frames_of(out)
        .into_iter()
        .filter_map(|f| match f.event {
            Event::ItemCompleted { item } => match item.body {
                bingo_sdk::ItemBody::Assistant { text } => Some(text),
                _ => None,
            },
            _ => None,
        })
        .next_back()
        .unwrap_or_default()
}

/// The root calls `SpawnAgent`, the child answers, the root reports. One
/// script serves both sessions: the fake provider hands its responses out in
/// order across the process.
const FOREGROUND: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"prompt":"say hi","background":false}}}]},
    {"steps":[{"text":"hi from the child"}]},
    {"steps":[{"text":"the child said hi"}]}
]}"#;

#[test]
fn a_foreground_agent_answers_the_root_and_stdout_stays_the_root_s() {
    let home = tempfile::tempdir().unwrap();
    let script = script(FOREGROUND);
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", home.path())
        .args(["--print", "--cwd"])
        .arg(home.path())
        .arg("spawn one"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "the child said hi\n",
        "a text run writes the root's prose and nothing of the child's"
    );
    assert!(
        stderr(&out).contains("[tool] SpawnAgent"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn the_child_s_reply_is_the_tool_call_s_result() {
    let home = tempfile::tempdir().unwrap();
    let script = script(FOREGROUND);
    let out = scripted_run(home.path(), &script, &[], "spawn one");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let frames: Vec<Frame> = stdout(&out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect();
    let root = &frames[0].session;
    assert!(
        frames.iter().all(|f| &f.session == root),
        "a json run follows the root alone"
    );

    let spawn = frames
        .iter()
        .filter_map(|f| match &f.event {
            Event::ItemCompleted { item } => match &item.body {
                bingo_sdk::ItemBody::ToolCall { name, output, .. } if name == "SpawnAgent" => {
                    output.clone()
                }
                _ => None,
            },
            _ => None,
        })
        .next_back()
        .expect("the SpawnAgent call completed");
    let text: String = spawn
        .parts
        .iter()
        .filter_map(bingo_sdk::ContentPart::as_text)
        .collect();
    assert!(!spawn.is_error, "{text}");
    assert!(text.contains("hi from the child"), "{text}");
}

/// A background agent's end wakes the session that spawned it (the deliver
/// door of ADR-0010, the async-by-default policy of ADR-0018): a hosted run
/// gets a second result with no further input. The two middle responses are
/// the same words because the parent's receipt round and the child's turn
/// race for the script's cursor; the fourth can only be the woken turn's,
/// which fires strictly after both.
const BACKGROUND_WAKE: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"prompt":"work quietly","background":true}}}]},
    {"steps":[{"text":"spawned, or the work itself"}]},
    {"steps":[{"text":"spawned, or the work itself"}]},
    {"steps":[{"text":"heard the agent finish"}]}
]}"#;

#[test]
fn a_finished_background_agent_wakes_the_run_that_spawned_it() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(BACKGROUND_WAKE);
    let mut host = super::stream_json::Host::start(&mut super::stream_json::hosted(
        dir.path(),
        &script,
        &["--dangerously-skip-permissions"],
    ));
    host.prompt("spawn a background worker");
    let first = host.until("result");
    assert_eq!(first["result"], "spawned, or the work itself");

    // Nothing more is sent: only the agent's end can open the next turn.
    std::thread::sleep(std::time::Duration::from_secs(3));
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    let results = ended.results();
    assert_eq!(
        results.len(),
        2,
        "the agent's end opened no turn: {:?}",
        ended.types()
    );
    assert_eq!(results[1]["result"], "heard the agent finish");
    assert_eq!(
        results[0]["session_id"], results[1]["session_id"],
        "the wake landed on the session that spawned the agent"
    );
}

/// The other branch of the deliver door: the child finishes while the parent
/// is mid-turn. The wake is queued; whether it is absorbed at the next tool
/// barrier (ADR-0008 §2 — the parent hears it as input of the turn it is in,
/// no second turn) or drained when the turn ends (a second turn) depends on
/// which side of the last barrier the child's end lands — a real race this
/// test does not pretend to fix. The invariant is that the completion is
/// never lost: it reaches the parent's journal either way. The two identical
/// middle texts absorb the child/parent race for the script's cursor.
const BACKGROUND_WAKE_BUSY: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"prompt":"work quietly","background":true}}}]},
    {"steps":[{"toolCall":{"name":"Bash","input":{"command":"sleep 2"}}}]},
    {"steps":[{"text":"still the first turn, or the work"}]},
    {"steps":[{"text":"still the first turn, or the work"}]},
    {"steps":[{"text":"heard the agent finish"}]}
]}"#;

#[test]
fn a_wake_that_finds_the_parent_busy_is_never_lost() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(BACKGROUND_WAKE_BUSY);
    let mut host = super::stream_json::Host::start(&mut super::stream_json::hosted(
        dir.path(),
        &script,
        &["--dangerously-skip-permissions"],
    ));
    host.prompt("spawn a background worker and keep going");
    let first = host.until("result");
    assert_eq!(first["result"], "still the first turn, or the work");

    std::thread::sleep(std::time::Duration::from_secs(3));
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    let results = ended.results();
    assert!(
        matches!(results.len(), 1 | 2),
        "absorbed into the turn, or one drained turn after it — never more: {:?}",
        ended.types()
    );
    if let [_, second] = results[..] {
        assert_eq!(second["result"], "heard the agent finish");
    }
    // Either way the proof is the journal: the stream shows no line for an
    // absorbed steering item, but the parent's own record holds the
    // completion as a user item.
    let journal = journal_text(&dir.path().join(".bingo/data"));
    assert!(
        journal.contains("finished."),
        "the parent never heard the completion: {:?}",
        ended.types()
    );
}

/// Every byte journaled under the data dir, for asserting what a session
/// heard rather than what a stream chose to print.
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

/// A text or json run is attached to the tree as well (ADR-0010 §3): off a
/// tty the child's permission prompt is refused as the root's would be, and
/// the run ends instead of waiting on a prompt nobody can see. The output
/// itself stays the root's.
#[test]
fn a_childs_permission_is_refused_off_a_tty_in_every_output_format() {
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"prompt":"run it","background":false}}}]},
            {"steps":[{"toolCall":{"name":"Bash","input":{"command":"echo hi"}}}]},
            {"steps":[{"text":"the child was refused"}]},
            {"steps":[{"text":"root done"}]}
        ]}"#,
    );
    for format in ["text", "json"] {
        let home = tempfile::tempdir().unwrap();
        let out = run_within(
            bingo()
                .env("BINGO_FAKE_SCRIPT", script.path())
                .env("HOME", home.path())
                .args(["--print", "--output-format", format, "--cwd"])
                .arg(home.path())
                .arg("spawn one"),
            Duration::from_secs(30),
        );
        assert_eq!(
            out.status.code(),
            Some(0),
            "{format}: stderr: {}",
            stderr(&out)
        );
        match format {
            "text" => assert_eq!(stdout(&out), "root done\n"),
            _ => {
                let frames = frames_of(&out);
                let root = &frames[0].session;
                assert!(frames.iter().all(|f| &f.session == root), "{format}");
            }
        }
    }
}

/// A child whose turn fails has not answered: the root's call is an error
/// result that says so, never a reply that happens to be empty.
#[test]
fn a_child_that_failed_is_an_error_result_for_the_root() {
    let home = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"prompt":"run it","background":false}}}]},
            {"steps":[{"error":{"kind":"auth","message":"no key for the child"}}]},
            {"steps":[{"text":"root done"}]}
        ]}"#,
    );
    let out = scripted_run(home.path(), &script, &[], "spawn one");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let spawn = frames_of(&out)
        .into_iter()
        .filter_map(|f| match f.event {
            Event::ItemCompleted { item } => match item.body {
                bingo_sdk::ItemBody::ToolCall { name, output, .. } if name == "SpawnAgent" => {
                    output
                }
                _ => None,
            },
            _ => None,
        })
        .next_back()
        .expect("the SpawnAgent call completed");
    assert!(spawn.is_error, "{spawn:?}");
    let text: String = spawn
        .parts
        .iter()
        .filter_map(bingo_sdk::ContentPart::as_text)
        .collect();
    assert!(
        text.contains("failed:") && text.contains("no key for the child"),
        "{text}"
    );
}

/// A definition names the persona; the sub-agent note is the kernel's own
/// business, but the name a definition carries is the plugin's.
#[test]
fn a_project_definition_names_the_agent_it_starts() {
    let home = tempfile::tempdir().unwrap();
    let agents = home.path().join(".bingo/agents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("reviewer.md"),
        "---\ndescription: Reviews a diff\n---\nYou review diffs, briefly.\n",
    )
    .unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"prompt":"review it","agent":"reviewer","background":false}}}]},
            {"steps":[{"text":"the diff is fine"}]},
            {"steps":[{"text":"the reviewer approves"}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", home.path())
        .args(["--print", "--cwd"])
        .arg(home.path())
        .arg("ask the reviewer"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "the reviewer approves\n");
}

/// Two roles, and a script of exactly two responses: one for the round the
/// root spends on `ListAgents`, one for what it says after. A role that had
/// opened a turn would have eaten the second, and the run would end on the
/// tool call instead of the text.
const ROSTER: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"ListAgents","input":{}}}]},
    {"steps":[{"text":"two are seated"}]}
]}"#;

/// A project whose team is a reviewer and a scout, neither with a definition.
fn with_a_team(home: &std::path::Path) {
    let bingo = home.join(".bingo");
    std::fs::create_dir_all(&bingo).unwrap();
    std::fs::write(
        bingo.join("team.json"),
        r#"{"roles":[{"name":"reviewer"},{"name":"scout"}]}"#,
    )
    .unwrap();
}

/// The sessions a `ListAgents` listing names, in the order it listed them.
fn listed(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter(|word| word.starts_with("ses_"))
        .map(str::to_string)
        .collect()
}

#[test]
fn a_project_s_roles_are_seated_before_the_root_s_first_turn() {
    let home = tempfile::tempdir().unwrap();
    with_a_team(home.path());
    let out = scripted_run(home.path(), &script(ROSTER), &[], "who is here?");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let roster = tool_output(&out, "ListAgents");
    assert!(roster.contains("reviewer"), "{roster}");
    assert!(roster.contains("scout"), "{roster}");
    assert_eq!(listed(&roster).len(), 2, "{roster}");
    assert_eq!(
        final_text(&out),
        "two are seated",
        "a role that had answered would have taken this response"
    );
}

/// `--continue` lands on the person's session, not on the role whose journal
/// was written last (`Latest` prefers a root), and reopening the root brings
/// the same two roles back rather than seating two more.
#[test]
fn the_same_roles_come_back_on_continue() {
    let home = tempfile::tempdir().unwrap();
    with_a_team(home.path());
    let first = scripted_run(home.path(), &script(ROSTER), &[], "who is here?");
    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    let before = tool_output(&first, "ListAgents");
    let second = scripted_run(
        home.path(),
        &script(ROSTER),
        &["--continue"],
        "who is here now?",
    );
    assert_eq!(second.status.code(), Some(0), "stderr: {}", stderr(&second));
    assert_eq!(
        frames_of(&second)[0].session,
        frames_of(&first)[0].session,
        "the same root"
    );
    let after = tool_output(&second, "ListAgents");
    assert!(
        after.contains("reviewer") && after.contains("scout"),
        "{after}"
    );
    assert_eq!(
        listed(&after),
        listed(&before),
        "the roles were reopened, not seated again"
    );
    assert_eq!(final_text(&second), "two are seated");
}

/// The kernel's depth limit is one, so a child is not offered `SpawnAgent`:
/// the call it makes anyway finds no such tool, and no grandchild is minted.
/// Four responses, consumed in the one order a run without a third session
/// can consume them — root, child, child, root.
#[test]
fn a_child_has_no_spawn_agent_to_call() {
    let home = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"prompt":"go deeper","background":false}}}]},
            {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"prompt":"deeper still","background":false}}}]},
            {"steps":[{"text":"I cannot spawn agents"}]},
            {"steps":[{"text":"the child says it cannot"}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", home.path())
        .args(["--print", "--cwd"])
        .arg(home.path())
        .arg("spawn one"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "the child says it cannot\n");
    let err = stderr(&out);
    assert!(
        err.contains("SpawnAgent") && !err.contains("[error]"),
        "{err}"
    );
}

// ---- what another session's journal says -----------------------------------

/// The directory of the session whose summary carries `key`.
fn session_dir(home: &Path, key: &str) -> Option<PathBuf> {
    std::fs::read_dir(home.join(".bingo/data/sessions"))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|dir| keyed(dir, key))
}

fn keyed(dir: &Path, key: &str) -> bool {
    std::fs::read_to_string(dir.join("summary.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|summary| summary["key"] == key)
}

/// Every key the run left on disk, for a failure that says what was there.
fn keys(home: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(home.join(".bingo/data/sessions")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let text = std::fs::read_to_string(entry.path().join("summary.json")).ok()?;
            let summary: Value = serde_json::from_str(&text).ok()?;
            Some(summary["key"].to_string())
        })
        .collect()
}

/// One session's journal, as frames: the first line names the format, and a
/// line the run was still writing when it ended is not a frame either.
fn journal(home: &Path, key: &str) -> Vec<Frame> {
    let dir = session_dir(home, key)
        .unwrap_or_else(|| panic!("no session is keyed {key}; there are {:?}", keys(home)));
    let text = std::fs::read_to_string(dir.join("journal.jsonl")).expect("a journal on disk");
    text.lines()
        .skip(1)
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// What was said into a session: the text, and who signed it.
fn said(frames: &[Frame]) -> Vec<(String, Origin)> {
    frames
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::User { parts, origin } => Some((
                    parts.iter().filter_map(ContentPart::as_text).collect(),
                    origin.clone(),
                )),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Every completed result of `tool` in these frames, in the order they landed.
fn results(frames: &[Frame], tool: &str) -> Vec<ToolOutput> {
    frames
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::ToolCall {
                    name,
                    output: Some(output),
                    ..
                } if name == tool => Some(output.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn text_of(output: &ToolOutput) -> String {
    output
        .parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect()
}

fn root_of(out: &Output) -> SessionId {
    frames_of(out)[0].session.clone()
}

fn with_team(home: &Path, team: &str) {
    let bingo = home.join(".bingo");
    std::fs::create_dir_all(&bingo).unwrap();
    std::fs::write(bingo.join("team.json"), team).unwrap();
}

fn run_json(home: &Path, script: &tempfile::NamedTempFile, prompt: &str) -> Output {
    run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script.path())
            .env("HOME", home)
            .args(["--print", "--output-format", "json", "--cwd"])
            .arg(home)
            .arg(prompt),
        Duration::from_secs(90),
    )
}

// ---- peers (ADR-0024) ------------------------------------------------------

/// Both agents are spawned in the foreground, so every response up to the one
/// that matters goes out in one order: a foreground spawn holds the root until
/// the child's turn ends. After the message there is a race between the builder
/// finishing, the woken reviewer and the root resuming, so every response after
/// it says the same word and waits, which leaves the reviewer time to journal
/// what it was sent before the root's turn ends the run.
const PEER_REVIEW: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"reviewer","prompt":"stand by","background":false}}}]},
    {"steps":[{"text":"standing by"}]},
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"builder","prompt":"ask the reviewer to look","background":false}}}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"reviewer","text":"please look at the diff"}}}]},
    {"steps":[{"delay":{"ms":2000}},{"text":"done"}]},
    {"steps":[{"delay":{"ms":2000}},{"text":"done"}]},
    {"steps":[{"delay":{"ms":2000}},{"text":"done"}]},
    {"steps":[{"text":"done"}]}
]}"#;

/// The whole of ADR-0024 §1 end to end: an agent writes to the teammate beside
/// it — one it never started and cannot see in its own tree — and the message
/// arrives signed. The parent relays nothing, so its transcript is untouched.
#[test]
fn a_sibling_dm_reaches_the_teammate_and_leaves_the_parent_out_of_it() {
    let home = tempfile::tempdir().unwrap();
    let out = run_json(home.path(), &script(PEER_REVIEW), "have the builder ask");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let root = root_of(&out);

    let builder = journal(home.path(), &format!("agent/{root}/builder"));
    let sent = results(&builder, "SendMessage");
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(!sent[0].is_error, "{}", text_of(&sent[0]));

    let reviewer = journal(home.path(), &format!("agent/{root}/reviewer"));
    let heard = said(&reviewer);
    let dm = heard
        .iter()
        .find(|(text, _)| text == "please look at the diff")
        .unwrap_or_else(|| panic!("the teammate never heard it: {heard:?}"));
    assert_eq!(dm.1.principal.as_deref(), Some("builder"), "signed");
    assert_eq!(dm.1.conversation, None, "a direct message is nobody else's");

    let relayed = said(&frames_of(&out))
        .into_iter()
        .any(|(_, origin)| origin.principal.as_deref() == Some("builder"));
    assert!(!relayed, "the parent was not a switchboard");
}

/// The footgun ADR-0024 §2 removes: a child asks its parent and ends its turn,
/// the parent answers, and the answer opens a turn on the idle child instead
/// of lying in its queue until something else does. Under `Hold` nothing would
/// take the message off the queue, and the child's journal would never hold it.
const IDLE_ANSWER: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"worker","prompt":"ask me which one","background":false}}}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"parent","text":"which one?"}}}]},
    {"steps":[{"text":"asked"}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"worker","text":"the second one"}}}]},
    {"steps":[{"delay":{"ms":2000}},{"text":"done"}]},
    {"steps":[{"delay":{"ms":2000}},{"text":"done"}]},
    {"steps":[{"text":"done"}]}
]}"#;

#[test]
fn an_answer_from_the_parent_wakes_the_child_that_asked() {
    let home = tempfile::tempdir().unwrap();
    let out = run_json(home.path(), &script(IDLE_ANSWER), "spawn one and answer it");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let root = root_of(&out);

    let heard = said(&journal(home.path(), &format!("agent/{root}/worker")));
    let answer = heard
        .iter()
        .find(|(text, _)| text == "the second one")
        .unwrap_or_else(|| panic!("the answer never opened a turn: {heard:?}"));
    assert_eq!(
        answer.1.principal.as_deref(),
        Some("parent"),
        "the answer is signed by whoever wrote it"
    );
}

// ---- the serial room (ADR-0025) --------------------------------------------

/// The posts of a room: what was said into it, in order.
fn posts(home: &Path, root: &SessionId, room: &str) -> Vec<String> {
    said(&journal(home, &format!("rooms/{root}/{room}")))
        .into_iter()
        .map(|(text, _)| text)
        .collect()
}

/// What a member was handed of a room, post by post: a nudge is unsigned and
/// is nobody's post, so this is exactly the fan-out it received.
fn fanned_out(home: &Path, root: &SessionId, member: &str, room: &str) -> Vec<String> {
    said(&journal(home, &format!("agent/{root}/{member}")))
        .into_iter()
        .filter(|(_, origin)| {
            origin.conversation.as_deref() == Some(room) && origin.principal.is_some()
        })
        .map(|(text, _)| text)
        .collect()
}

/// The `owed` cards a run published on the root, in order, as names.
fn owed(out: &Output) -> Vec<String> {
    frames_of(out)
        .into_iter()
        .filter_map(|frame| match frame.event {
            Event::Signal {
                plugin,
                kind,
                payload,
            } if plugin == "bingo.rooms" && kind == "owed" => {
                Some(payload["rows"][0][1].as_str()?.to_string())
            }
            _ => None,
        })
        .collect()
}

/// The root posts, spawns the scout, and the scout posts — and the root is
/// never handed a post of its own room, so its next post is written behind the
/// room's head. Every response up to the retry goes out in one order: nothing
/// else has a turn to take one until the retry lands and wakes the scout.
const STALE_POST: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"morning"}}}]},
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"scout","prompt":"post what you found in #design","background":false}}}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"the build is green"}}}]},
    {"steps":[{"text":"posted"}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"then ship it"}}}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"then ship it"}}}]},
    {"steps":[{"delay":{"ms":1500}},{"text":"done"}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]}
]}"##;

#[test]
fn a_stale_post_bounces_with_what_it_missed_and_lands_on_the_retry() {
    let home = tempfile::tempdir().unwrap();
    with_team(
        home.path(),
        r#"{"rooms":[{"name":"design","members":["scout"]}]}"#,
    );
    let out = run_json(home.path(), &script(STALE_POST), "run the stand-up");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let root = root_of(&out);

    let sent = results(&frames_of(&out), "SendMessage");
    let [first, bounced, landed] = sent.as_slice() else {
        panic!("three posts were attempted: {sent:?}");
    };
    assert!(!first.is_error, "{}", text_of(first));
    assert!(bounced.is_error, "the root wrote behind the head");
    let quote = text_of(bounced);
    assert!(quote.contains("#design"), "{quote}");
    assert!(quote.contains("scout: the build is green"), "{quote}");
    assert!(!landed.is_error, "the retry: {}", text_of(landed));

    assert_eq!(
        posts(home.path(), &root, "design"),
        ["morning", "the build is green", "then ship it"],
        "one bounce, and the post lands once"
    );
}

/// The same shape, with a question standing in the room. Nothing a bounce
/// touches reaches the mention ledger, because the ledger is a fold of the
/// room's posts and a bounced post never became one.
const STALE_ANSWER: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"morning"}}}]},
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"scout","prompt":"ask in #design","background":false}}}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"@parent what does the build say?"}}}]},
    {"steps":[{"text":"asked"}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"@scout the answer is 42"}}}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"@scout the answer is 42"}}}]},
    {"steps":[{"delay":{"ms":1500}},{"text":"done"}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]}
]}"##;

#[test]
fn a_bounced_post_neither_opens_nor_answers_a_mention_debt() {
    let home = tempfile::tempdir().unwrap();
    with_team(
        home.path(),
        r#"{"rooms":[{"name":"design","members":["scout","parent"]}]}"#,
    );
    let out = run_json(home.path(), &script(STALE_ANSWER), "answer the scout");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let root = root_of(&out);

    let sent = results(&frames_of(&out), "SendMessage");
    let [_, bounced, landed] = sent.as_slice() else {
        panic!("three posts were attempted: {sent:?}");
    };
    assert!(bounced.is_error, "the root had not read the question");
    assert!(
        text_of(bounced).contains("scout: @parent what does the build say?"),
        "{}",
        text_of(bounced)
    );
    assert!(!landed.is_error, "the retry: {}", text_of(landed));

    let posted = posts(home.path(), &root, "design");
    assert_eq!(
        posted
            .iter()
            .filter(|p| *p == "@scout the answer is 42")
            .count(),
        1,
        "the bounce landed nothing: {posted:?}"
    );
    assert_eq!(posted.len(), 3, "{posted:?}");

    let owed = owed(&out);
    assert_eq!(
        owed.first().map(String::as_str),
        Some("parent"),
        "the scout's question opened a debt: {owed:?}"
    );
    assert_eq!(
        owed.last().map(String::as_str),
        Some("scout"),
        "only the post that landed answered it and asked again: {owed:?}"
    );
}

/// ADR-0025 §5, which the count of §2 leans on: one delivery per post per
/// member. Both members are spawned in the foreground before the post, so the
/// order up to it is the script's; every response after it waits, which leaves
/// the woken members time to journal what they were handed.
const ONE_POST: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"alpha","prompt":"stand by","background":false}}}]},
    {"steps":[{"text":"standing by"}]},
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"beta","prompt":"stand by","background":false}}}]},
    {"steps":[{"text":"standing by"}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"stand-up in five"}}}]},
    {"steps":[{"delay":{"ms":2000}},{"text":"done"}]},
    {"steps":[{"delay":{"ms":2000}},{"text":"done"}]},
    {"steps":[{"delay":{"ms":2000}},{"text":"done"}]},
    {"steps":[{"text":"done"}]}
]}"##;

#[test]
fn a_post_reaches_each_member_exactly_once() {
    let home = tempfile::tempdir().unwrap();
    with_team(
        home.path(),
        r#"{"rooms":[{"name":"design","members":["alpha","beta"]}]}"#,
    );
    let out = run_json(home.path(), &script(ONE_POST), "call the stand-up");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let root = root_of(&out);

    for member in ["alpha", "beta"] {
        assert_eq!(
            fanned_out(home.path(), &root, member, "#design"),
            ["stand-up in five"],
            "{member} was handed the post once and once only"
        );
    }
}

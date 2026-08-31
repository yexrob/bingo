//! Black-box: a room's task list is the board (ADR-0023). The four tools take
//! an optional `in: "#room"`, a member claims a task without ever saying who
//! it is, and an owner no session in the room answers to is marked at read
//! time with nothing rewritten.

use std::path::{Path, PathBuf};

use bingo_sdk::{ContentPart, ItemBody, SessionId, ToolOutput};

use super::*;

fn sessions(home: &Path) -> PathBuf {
    home.join(".bingo/data/sessions")
}

/// The directory of the session whose summary carries `key`.
fn session_dir(home: &Path, key: &str) -> Option<PathBuf> {
    std::fs::read_dir(sessions(home))
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .find(|dir| keyed(dir, key))
}

fn keyed(dir: &Path, key: &str) -> bool {
    std::fs::read_to_string(dir.join("summary.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .is_some_and(|summary| summary["key"] == key)
}

/// One session's journal, as frames. The first line names the format, and a
/// line the run was still writing when it ended is not a frame either.
fn journal(home: &Path, key: &str) -> Vec<Frame> {
    let dir = session_dir(home, key).unwrap_or_else(|| panic!("no session is keyed {key}"));
    let text = std::fs::read_to_string(dir.join("journal.jsonl")).expect("a journal on disk");
    text.lines()
        .skip(1)
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Every completed call of `tool` in these frames, in the order they ran.
fn results(frames: &[Frame], tool: &str) -> Vec<ToolOutput> {
    frames
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::ToolCall { name, output, .. } if name == tool => output.clone(),
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

/// The task list the room's journal holds, as the last `bingo.tasks` payload
/// published into it: what a board really stores, not what a listing said.
fn board_payload(home: &Path, key: &str) -> serde_json::Value {
    journal(home, key)
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::Extension {
                plugin,
                kind,
                payload,
            } if plugin == "bingo.tasks" && kind == "tasks" => Some(payload.clone()),
            _ => None,
        })
        .next_back()
        .unwrap_or_else(|| panic!("no task list was published into {key}"))
}

fn root_of(out: &Output) -> SessionId {
    frames_of(out)[0].session.clone()
}

/// The whole of ADR-0023 in one run. The root opens a room, puts two tasks on
/// its board — one for a name nobody here holds — and spawns a member in the
/// foreground, so the script's responses are handed out root, root, root,
/// root, member, member, member, member, root, root, root, in that order.
const BOARD: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"OpenRoom","input":{"name":"build","members":["worker"]}}}]},
    {"steps":[{"toolCall":{"name":"TaskCreate","input":{"subject":"write the plan","in":"#build"}}}]},
    {"steps":[{"toolCall":{"name":"TaskCreate","input":{"subject":"ship it","owner":"ghost","in":"#build"}}}]},
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"worker","prompt":"take a task off the board","background":false}}}]},
    {"steps":[{"toolCall":{"name":"TaskList","input":{"in":"#build"}}}]},
    {"steps":[{"toolCall":{"name":"TaskUpdate","input":{"id":1,"status":"in_progress","claim":true,"in":"#build"}}}]},
    {"steps":[{"toolCall":{"name":"TaskUpdate","input":{"id":1,"status":"completed","in":"#build"}}}]},
    {"steps":[{"text":"took the plan"}]},
    {"steps":[{"toolCall":{"name":"TaskList","input":{"in":"#build"}}}]},
    {"steps":[{"toolCall":{"name":"TaskList","input":{"in":"#nowhere"}}}]},
    {"steps":[{"text":"the board is read"}]}
]}"##;

#[test]
fn a_member_reads_a_room_s_board_claims_a_task_and_the_parent_sees_it() {
    let home = tempfile::tempdir().unwrap();
    let script = script(BOARD);
    let out = run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script.path())
            .env("HOME", home.path())
            .args([
                "--print",
                "--output-format",
                "json",
                "--allowed-tools",
                "OpenRoom",
                "--cwd",
            ])
            .arg(home.path())
            .arg("put the work on a board"),
        Duration::from_secs(60),
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let root = root_of(&out);

    // The member reads a board it never wrote to, and claims one task on it.
    // The name that lands is its own, which it was never told and never said.
    let member = journal(home.path(), &format!("agent/{root}/worker"));
    let listed = results(&member, "TaskList");
    assert_eq!(listed.len(), 1, "the member listed the board once");
    assert!(!listed[0].is_error, "{}", text_of(&listed[0]));
    assert_eq!(
        text_of(&listed[0]),
        "#1 [pending] write the plan\n#2 [pending] ship it — ghost (gone)"
    );

    let written = results(&member, "TaskUpdate");
    assert_eq!(written.len(), 2, "claimed, then completed");
    assert_eq!(
        text_of(&written[0]),
        "Updated #1 (in_progress): write the plan — worker",
        "the claim is stamped with the caller's own name"
    );
    assert_eq!(
        text_of(&written[1]),
        "Updated #1 (completed): write the plan"
    );

    // The parent reads the same board and sees the member's work, and a name
    // no session here answers to marked as gone.
    let seen = results(&frames_of(&out), "TaskList");
    assert_eq!(seen.len(), 2, "the board, then a room that is not here");
    assert!(!seen[0].is_error, "{}", text_of(&seen[0]));
    assert_eq!(
        text_of(&seen[0]),
        "#1 [completed] write the plan — worker\n#2 [pending] ship it — ghost (gone)"
    );

    // A name out of reach is a worded result the model can correct, not a
    // failed call: it says what is not here and what is.
    assert!(seen[1].is_error, "{}", text_of(&seen[1]));
    let refused = text_of(&seen[1]);
    assert!(refused.contains("#nowhere"), "{refused}");
    assert!(refused.contains("#build"), "{refused}");

    // Nothing was rewritten by any of the marking: the room's journal holds
    // the owner as it was written, and the parent's own list stays empty.
    let stored = board_payload(home.path(), &format!("rooms/{root}/build"));
    assert_eq!(stored[0]["owner"], "worker");
    assert_eq!(stored[1]["owner"], "ghost", "the mark reached the journal");
    assert!(
        !frames_of(&out).iter().any(|frame| matches!(
            &frame.event,
            Event::Extension { plugin, kind, .. } if plugin == "bingo.tasks" && kind == "tasks"
        )),
        "a board write landed on the caller's own list"
    );
}

/// The first run leaves one task claimed and unfinished on the board.
const CLAIMED: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"OpenRoom","input":{"name":"build","members":["worker"]}}}]},
    {"steps":[{"toolCall":{"name":"TaskCreate","input":{"subject":"write the plan","in":"#build"}}}]},
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"worker","prompt":"take a task off the board","background":false}}}]},
    {"steps":[{"toolCall":{"name":"TaskUpdate","input":{"id":1,"status":"in_progress","claim":true,"in":"#build"}}}]},
    {"steps":[{"text":"took the plan"}]},
    {"steps":[{"text":"it is being worked on"}]}
]}"##;

/// The second run only reads the board.
const READ_BOARD: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"TaskList","input":{"in":"#build"}}}]},
    {"steps":[{"text":"read"}]}
]}"##;

/// A later run of the same session, reading the board and nothing else.
fn board_line(home: &Path, root: &SessionId) -> String {
    let out = scripted_run(
        home,
        &script(READ_BOARD),
        &["--continue"],
        "what is on the board?",
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(&frames_of(&out)[0].session, root, "the same session");
    let listed = results(&frames_of(&out), "TaskList");
    assert!(!listed[0].is_error, "{}", text_of(&listed[0]));
    text_of(&listed[0])
}

/// The owner is not rewritten when its session goes: the parent hears the end
/// and edits the board deliberately (ADR-0023 §3). What the board does on its
/// own is say, at read time, that nobody here answers to that name any more.
#[test]
fn a_task_left_by_a_member_that_is_gone_reads_as_gone() {
    let home = tempfile::tempdir().unwrap();
    let first = run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script(CLAIMED).path())
            .env("HOME", home.path())
            .args([
                "--print",
                "--output-format",
                "json",
                "--allowed-tools",
                "OpenRoom",
                "--cwd",
            ])
            .arg(home.path())
            .arg("put the work on a board"),
        Duration::from_secs(60),
    );
    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    let root = root_of(&first);

    // While the member is still in the tree the board says its name plainly.
    assert_eq!(
        board_line(home.path(), &root),
        "#1 [in_progress] write the plan — worker"
    );

    // Now it is gone: nothing in the tree answers to `worker` any more.
    let member = session_dir(home.path(), &format!("agent/{root}/worker")).expect("the member ran");
    std::fs::remove_dir_all(&member).unwrap();

    assert_eq!(
        board_line(home.path(), &root),
        "#1 [in_progress] write the plan — worker (gone)"
    );
    assert_eq!(
        board_payload(home.path(), &format!("rooms/{root}/build"))[0]["owner"],
        "worker",
        "the mark was written into the board"
    );
}

/// Without `in`, every tool means the caller's own session — the same journal,
/// the same lines, the same run-to-run list as before there was a board.
const PRIVATE: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"TaskCreate","input":{"subject":"write the plan","owner":"nobody"}}}]},
    {"steps":[{"toolCall":{"name":"TaskList","input":{}}}]},
    {"steps":[{"text":"listed"}]}
]}"#;

#[test]
fn a_session_s_own_list_is_untouched_by_the_board() {
    let home = tempfile::tempdir().unwrap();
    let out = scripted_run(home.path(), &script(PRIVATE), &[], "note the plan");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let listed = results(&frames_of(&out), "TaskList");
    assert_eq!(
        text_of(&listed[0]),
        "#1 [pending] write the plan — nobody",
        "a private list asserts nothing about who its owners are"
    );
}

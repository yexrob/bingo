//! Black-box: what a post owes (ADR-0022). `@name` in a room opens a debt the
//! named member's next post closes; a member who stays silent is nudged — in
//! its own journal, never in the room — and while anything is owed the room's
//! parent carries the card. `@all` asks the room and chases nobody.

use std::path::{Path, PathBuf};

use bingo_sdk::{ContentPart, ItemBody, Origin, SessionId};
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

use super::*;

/// A project with one room in it, seated under the person's own session before
/// its first turn. `scout` is a member of it and no role: nothing is seated
/// under that name until the run spawns it, so every scenario here hands its
/// responses out in one order.
fn with_a_room(home: &Path) {
    let bingo = home.join(".bingo");
    std::fs::create_dir_all(&bingo).unwrap();
    std::fs::write(
        bingo.join("team.json"),
        r#"{"rooms":[{"name":"design","members":["scout"]}]}"#,
    )
    .unwrap();
}

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

/// One session's journal, as frames: the first line names the format, and a
/// line the run was still writing when it ended is not a frame either.
fn journal(home: &Path, key: &str) -> Vec<Frame> {
    let dir = session_dir(home, key).unwrap_or_else(|| panic!("no session is keyed {key}"));
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

/// The nudges a member was sent: what the room said with nobody's name on it.
/// A post is always signed, so this is exactly what is not one.
fn nudges(home: &Path, root: &SessionId, member: &str) -> Vec<String> {
    said(&journal(home, &format!("agent/{root}/{member}")))
        .into_iter()
        .filter(|(_, origin)| origin.surface == "room" && origin.principal.is_none())
        .map(|(text, _)| text)
        .collect()
}

/// The `owed` cards a run published on the root, in order.
fn cards(out: &Output) -> Vec<Value> {
    frames_of(out)
        .into_iter()
        .filter_map(|frame| match frame.event {
            Event::Signal {
                plugin,
                kind,
                payload,
            } if plugin == "bingo.rooms" && kind == "owed" => Some(payload),
            _ => None,
        })
        .collect()
}

fn root_of(out: &Output) -> SessionId {
    frames_of(out)[0].session.clone()
}

/// Every post in a room's journal, an hour older: what the process before this
/// one would have left behind. Only the items' own stamps move — the age of a
/// debt is the age of the post that opened it, and nothing else reads them.
fn age_the_posts(home: &Path, key: &str) {
    let dir = session_dir(home, key).unwrap_or_else(|| panic!("no session is keyed {key}"));
    let path = dir.join("journal.jsonl");
    let text = std::fs::read_to_string(&path).expect("a journal on disk");
    let long_ago = (Timestamp::now() - SignedDuration::from_hours(1)).to_string();
    let aged: Vec<String> = text
        .lines()
        .map(|line| aged_line(line, &long_ago))
        .collect();
    std::fs::write(&path, aged.join("\n") + "\n").unwrap();
}

fn aged_line(line: &str, at: &str) -> String {
    let Ok(mut frame) = serde_json::from_str::<Value>(line) else {
        return line.to_string();
    };
    let Some(item) = frame.pointer_mut("/event/item") else {
        return line.to_string();
    };
    item["startedAt"] = json!(at);
    item["completedAt"] = json!(at);
    frame.to_string()
}

fn run_in(home: &Path, script: &tempfile::NamedTempFile, extra: &[&str], prompt: &str) -> Output {
    run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script.path())
            .env("HOME", home)
            .args(["--print", "--cwd"])
            .arg(home)
            .args(extra)
            .arg(prompt),
        Duration::from_secs(60),
    )
}

/// Whatever asks next says the same word, so a second process does not depend
/// on which of the nudged member and the resuming root gets there first.
const DONE: &str = r#"{"responses":[
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]}
]}"#;

/// The root asks the scout something, asks the room something, and only then
/// spawns the scout — so every response is handed out in one order, and the
/// scout is a session that heard the question and said nothing.
const ASK_AND_SAY_NOTHING: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"@scout what does the build say?"}}}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"@all stand-up in five"}}}]},
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"scout","prompt":"say ok","background":false}}}]},
    {"steps":[{"text":"ok"}]},
    {"steps":[{"text":"done"}]}
]}"##;

/// A debt outlives the process that heard it asked: the next one re-derives it
/// from the room's own journal and chases what is already overdue — once, and
/// never for `@all`, which named nobody to chase.
#[test]
fn a_question_left_unanswered_is_chased_when_the_next_process_opens_the_room() {
    let home = tempfile::tempdir().unwrap();
    with_a_room(home.path());
    let asked = run_in(
        home.path(),
        &script(ASK_AND_SAY_NOTHING),
        &["--output-format", "json"],
        "ask them",
    );
    assert_eq!(asked.status.code(), Some(0), "stderr: {}", stderr(&asked));
    let root = root_of(&asked);
    assert!(
        nudges(home.path(), &root, "scout").is_empty(),
        "nothing is chased inside the first five minutes"
    );

    age_the_posts(home.path(), &format!("rooms/{root}/design"));
    let again = run_in(
        home.path(),
        &script(DONE),
        &["--continue", "--output-format", "json"],
        "carry on",
    );
    assert_eq!(again.status.code(), Some(0), "stderr: {}", stderr(&again));
    assert_eq!(root_of(&again), root, "--continue is the same session");

    let nudged = nudges(home.path(), &root, "scout");
    assert_eq!(
        nudged.len(),
        1,
        "one overdue question, one nudge: {nudged:?}"
    );
    assert!(nudged[0].contains("#design"), "{}", nudged[0]);
    assert!(
        nudged[0].contains("what does the build say?"),
        "{}",
        nudged[0]
    );
    assert!(
        !nudged[0].contains("stand-up"),
        "`@all` asked the room and chases nobody: {}",
        nudged[0]
    );

    let posts = said(&journal(home.path(), &format!("rooms/{root}/design")));
    assert_eq!(posts.len(), 2, "a nudge is not a post: {posts:?}");
}

/// The scout answers in the room, so nothing is ever chased: the room's own
/// journal says the debt closed, and the card on the parent goes with it.
const ASK_AND_ANSWER: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"@scout what does the build say?"}}}]},
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"scout","prompt":"post what you found in #design","background":false}}}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"it is green"}}}]},
    {"steps":[{"text":"posted"}]},
    {"steps":[{"text":"done"}]}
]}"##;

#[test]
fn the_card_stands_while_a_question_does_and_goes_when_it_is_answered() {
    let home = tempfile::tempdir().unwrap();
    with_a_room(home.path());
    let out = run_in(
        home.path(),
        &script(ASK_AND_ANSWER),
        &["--output-format", "json"],
        "ask them",
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let root = root_of(&out);

    let cards = cards(&out);
    let owing = cards
        .iter()
        .position(|card| card["kind"] == "table")
        .unwrap_or_else(|| panic!("no card was ever raised: {cards:?}"));
    assert_eq!(cards[owing]["rows"][0][0], "#design");
    assert_eq!(cards[owing]["rows"][0][1], "scout");
    assert_eq!(
        cards.last(),
        Some(&Value::Null),
        "answered, and the card goes: {cards:?}"
    );

    assert!(
        nudges(home.path(), &root, "scout").is_empty(),
        "speaking is the answer, and nobody was chased for it"
    );
    let posts = said(&journal(home.path(), &format!("rooms/{root}/design")));
    assert_eq!(posts.len(), 2, "the question and the answer: {posts:?}");
}

/// `/room` is where a person looks: the column names who has not answered and
/// for how long, and a debt survives the process it was opened in.
#[test]
fn the_room_table_names_who_owes_an_answer() {
    let home = tempfile::tempdir().unwrap();
    with_a_room(home.path());
    let asked = run_in(
        home.path(),
        &script(ASK_AND_SAY_NOTHING),
        &["--output-format", "json"],
        "ask them",
    );
    assert_eq!(asked.status.code(), Some(0), "stderr: {}", stderr(&asked));

    let listed = run_in(home.path(), &script(DONE), &["--continue"], "/room");
    assert_eq!(listed.status.code(), Some(0), "stderr: {}", stderr(&listed));
    let table = stdout(&listed);
    assert!(table.contains("room"), "{table}");
    assert!(table.contains("owed"), "the column is there: {table}");
    assert!(table.contains("#design"), "{table}");
    assert!(
        table.contains("scout 0s"),
        "who owes, and since when: {table}"
    );
    assert!(
        table.contains("@all"),
        "the room owes an answer too: {table}"
    );
}

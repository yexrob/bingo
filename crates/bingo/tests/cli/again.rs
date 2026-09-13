//! Black-box: a bounce costs one word (ADR-0053 §6). A room hands a post back
//! when somebody spoke while it was being written, and the draft it bounced is
//! the caller's own call — so `SendMessage { to, again: true }` lands those
//! words, once, without the model writing them a second time.

use std::path::{Path, PathBuf};
use std::time::Duration;

use bingo_sdk::{ContentPart, ItemBody};

use super::*;

/// Every session directory the run wrote.
fn dirs(home: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(home.join(".bingo/data/sessions")) else {
        return Vec::new();
    };
    entries.flatten().map(|entry| entry.path()).collect()
}

/// A session's summary as it stands on disk.
fn summary_of(dir: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(dir.join("summary.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// The run's root session: the one with nothing above it, which is the session
/// the person prompted and the one the room hangs under.
fn root_dir(home: &Path) -> Option<PathBuf> {
    dirs(home)
        .into_iter()
        .find(|dir| summary_of(dir).is_some_and(|summary| summary["parent"].is_null()))
}

/// The room the run opened, whatever the root's id turned out to be.
fn room_dir(home: &Path) -> Option<PathBuf> {
    dirs(home).into_iter().find(|dir| {
        summary_of(dir).is_some_and(|summary| {
            summary["key"]
                .as_str()
                .is_some_and(|key| key.starts_with("rooms/"))
        })
    })
}

/// One session's journal, as frames. The first line names the format, and a
/// line the run was still writing when it ended is not a frame either.
fn frames_at(dir: &Path) -> Vec<Frame> {
    let Ok(text) = std::fs::read_to_string(dir.join("journal.jsonl")) else {
        return Vec::new();
    };
    text.lines()
        .skip(1)
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// What was said into a session: the text of each post, and who signed it.
fn posts(frames: &[Frame]) -> Vec<(String, Option<String>)> {
    frames
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::User { parts, origin } => Some((
                    parts.iter().filter_map(ContentPart::as_text).collect(),
                    origin.principal.clone(),
                )),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// What every completed call to `tool` returned, in the order this session
/// made them.
fn results(frames: &[Frame], tool: &str) -> Vec<String> {
    frames
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::ToolCall {
                    name,
                    output: Some(output),
                    ..
                } if name == tool => Some(
                    output
                        .parts
                        .iter()
                        .filter_map(ContentPart::as_text)
                        .collect(),
                ),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// A run of the binary whose failure would be a hang: past the limit it is
/// killed and the test fails rather than the suite waiting on it.
fn run_in(home: &Path, script: &tempfile::NamedTempFile, extra: &[&str], prompt: &str) -> Output {
    run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script.path())
            .envs(home_env(home))
            .args(["--print", "--output-format", "json", "--cwd"])
            .arg(home)
            .args(extra)
            .arg(prompt),
        Duration::from_secs(60),
    )
}

/// The root opens a room for one purpose, spawns the scout in the foreground
/// and waits for it to post — so the room's head has moved by the time the
/// root writes into it. The root is off the roster: it keeps no cursor there
/// and posts blind (ADR-0025, consequences), so what it writes bounces and the
/// word after it lands the same draft. Every response past the root's last is
/// the same word, so the turn the landed post opens in the scout takes nothing
/// the root was waiting for.
const BOUNCED_THEN_AGAIN: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"OpenRoom","input":{"name":"design","purpose":"settle the room's verbs","members":["scout"],"listeners":[{"name":"scout","patience_s":0}]}}}]},
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"scout","prompt":"post what the build says in #design","background":false}}}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"the build is green"}}}]},
    {"steps":[{"text":"posted"}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"stand-up in five"}}}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","again":true}}}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]}
]}"##;

/// The whole of ADR-0053 §6, end to end: the bounce says how to post again,
/// one word does it, and the room holds the draft's own words exactly once.
#[test]
fn a_bounced_post_lands_on_the_word_after_it() {
    let home = tempfile::tempdir().unwrap();
    let script = script(BOUNCED_THEN_AGAIN);
    let out = run_in(
        home.path(),
        &script,
        &["--allowed-tools", "OpenRoom"],
        "convene them",
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let root = root_dir(home.path()).expect("a root session");
    let said = results(&frames_at(&root), "SendMessage");
    assert_eq!(said.len(), 2, "the root wrote twice: {said:?}");
    assert!(
        said[0].contains("scout: the build is green"),
        "the post the root never heard comes back with its own: {}",
        said[0]
    );
    assert!(
        said[0].contains("`again: true`"),
        "and the bounce says how to post it again: {}",
        said[0]
    );
    assert_eq!(said[1], "Posted to #design.");

    let room = room_dir(home.path()).expect("a room was opened");
    assert_eq!(
        posts(&frames_at(&room)),
        [
            ("the build is green".to_string(), Some("scout".to_string())),
            ("stand-up in five".to_string(), Some("parent".to_string())),
        ],
        "the draft landed once, in the words it was written in"
    );
}

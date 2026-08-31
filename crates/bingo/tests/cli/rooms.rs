//! Black-box: an agent opens a room (ADR-0021). `OpenRoom` hangs a room under
//! the caller, or — with `shared` — under the session that started it, which
//! is the whole of who hears what is posted there. A person is asked before
//! either happens, and the card they answer names the room, its members and
//! the tree it will hang in.

use std::path::{Path, PathBuf};

use bingo_sdk::{ContentPart, InteractionKind, ItemBody, Origin, SessionId};

use super::*;

/// A team of one: `scout` is seated under the root before its first turn, so
/// the agent the root spawns has a peer to convene.
fn with_a_scout(home: &Path) {
    let bingo = home.join(".bingo");
    std::fs::create_dir_all(&bingo).unwrap();
    std::fs::write(bingo.join("team.json"), r#"{"roles":[{"name":"scout"}]}"#).unwrap();
}

fn sessions(home: &Path) -> PathBuf {
    home.join(".bingo/data/sessions")
}

/// The directory of the session whose summary carries `key`, or nothing when
/// the run opened no such session.
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

/// One session's journal, as frames. The first line names the format; a last
/// line the run was still writing when it ended is not a frame either, so the
/// lines that parse are the journal, exactly as the store's own replay reads
/// it.
fn journal(home: &Path, key: &str) -> Vec<Frame> {
    let dir = session_dir(home, key).unwrap_or_else(|| panic!("no session is keyed {key}"));
    let text = std::fs::read_to_string(dir.join("journal.jsonl")).expect("a journal on disk");
    text.lines()
        .skip(1)
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// What another session said into this one: the text, and who signed it.
fn posts(frames: &[Frame]) -> Vec<(String, Origin)> {
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

/// The result a completed call to `tool` returned, in this session's journal.
fn tool_result(frames: &[Frame], tool: &str) -> bingo_sdk::ToolOutput {
    frames
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::ToolCall { name, output, .. } if name == tool => output.clone(),
                _ => None,
            },
            _ => None,
        })
        .next_back()
        .unwrap_or_else(|| panic!("no {tool} call completed"))
}

fn text_of(output: &bingo_sdk::ToolOutput) -> String {
    output
        .parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect()
}

fn root_of(out: &Output) -> SessionId {
    frames_of(out)[0].session.clone()
}

/// The root spawns `reviewer` in the foreground and waits for it, so the first
/// three responses are handed out in one order: root, reviewer, reviewer.
/// After the post there is no order to rely on — the woken scout, the reviewer
/// finishing and the root resuming ask in whatever order they get there — so
/// every response after it is the same word.
const CONVENE: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"reviewer","prompt":"convene the room","background":false}}}]},
    {"steps":[{"toolCall":{"name":"OpenRoom","input":{"name":"design","members":["reviewer","scout"],"shared":true}}}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"stand-up in five"}}}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]}
]}"##;

/// The whole of ADR-0021 §2, end to end: an agent opens a room under the
/// session that started it and posts into it, and the peer it named — an agent
/// it never started and cannot see — reads the post, told which room it came
/// from and who wrote it.
#[test]
fn an_agent_opens_a_shared_room_and_its_peer_hears_the_post() {
    let home = tempfile::tempdir().unwrap();
    with_a_scout(home.path());
    let script = script(CONVENE);
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
            .arg("convene them"),
        Duration::from_secs(60),
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let root = root_of(&out);

    // The room hangs under the root — the caller's parent — and not under the
    // agent that opened it. The key says which tree it is in.
    assert!(
        session_dir(home.path(), &format!("rooms/{root}/design")).is_some(),
        "a shared room hangs in the parent's tree"
    );

    let reviewer = journal(home.path(), &format!("agent/{root}/reviewer"));
    let opened = tool_result(&reviewer, "OpenRoom");
    assert!(!opened.is_error, "{}", text_of(&opened));
    assert_eq!(text_of(&opened), "#design: reviewer, scout");

    let scout = journal(home.path(), &format!("agent/{root}/scout"));
    let post = posts(&scout)
        .into_iter()
        .find(|(_, origin)| origin.surface == "room")
        .unwrap_or_else(|| panic!("the post never reached the peer: {:?}", posts(&scout)));
    assert_eq!(post.0, "stand-up in five");
    assert_eq!(
        post.1.principal.as_deref(),
        Some("reviewer"),
        "the peer is told who wrote it"
    );
    assert_eq!(
        post.1.conversation.as_deref(),
        Some("#design"),
        "and which room it came from"
    );
}

/// The card is the only place a person sees where a room will hang before
/// approving it, so it is asserted whole (ADR-0021 §5). Off a tty the answer
/// is a refusal, which is beside the point: what is pinned here is the words.
#[test]
fn the_permission_card_names_the_room_its_members_and_where_it_will_hang() {
    let home = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"OpenRoom","input":{"name":"design","members":["reviewer","scout"]}}}]},
            {"steps":[{"text":"it was not allowed"}]}
        ]}"#,
    );
    let out = scripted_run(home.path(), &script, &[], "open a room");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let summary = frames_of(&out)
        .into_iter()
        .find_map(|frame| match frame.event {
            Event::InteractionOpened { interaction } => match interaction.kind {
                InteractionKind::Permission { tool, summary, .. } if tool == "OpenRoom" => {
                    Some(summary)
                }
                _ => None,
            },
            _ => None,
        })
        .expect("the gate asked before opening a room");
    assert_eq!(
        summary,
        "OpenRoom #design under the caller with reviewer, scout"
    );
}

/// A root has no peers to convene, and is told so rather than quietly given a
/// room under itself — the two reach different audiences. The rule allows the
/// call, so the refusal is the tool's own.
#[test]
fn a_root_asking_to_share_is_refused_in_words_and_opens_nothing() {
    let home = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"OpenRoom","input":{"name":"design","shared":true}}}]},
            {"steps":[{"text":"no peers here"}]}
        ]}"#,
    );
    let out = scripted_run(
        home.path(),
        &script,
        &["--allowed-tools", "OpenRoom"],
        "share one",
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let root = root_of(&out);

    let refused = tool_result(&frames_of(&out), "OpenRoom");
    assert!(refused.is_error, "a root's shared room was opened");
    let text = text_of(&refused);
    assert!(text.contains("root"), "{text}");
    assert!(text.contains("without `shared`"), "{text}");
    assert!(
        session_dir(home.path(), &format!("rooms/{root}/design")).is_none(),
        "the refusal opened a room anyway"
    );
}

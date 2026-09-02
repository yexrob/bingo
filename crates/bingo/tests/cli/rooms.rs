//! Black-box: an agent opens a room (ADR-0021). `OpenRoom` hangs a room under
//! the caller, or — with `shared` — under the session that started it, which
//! is the whole of who hears what is posted there. A person is asked before
//! either happens, and the card they answer names the room, its members and
//! the tree it will hang in.

use std::path::{Path, PathBuf};

use bingo_sdk::{ContentPart, InteractionKind, ItemBody, Origin, SessionId};

use super::stream_json::Host;
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
    dirs(home).into_iter().find(|dir| keyed(dir, key))
}

/// Every session directory the run has written so far.
fn dirs(home: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(sessions(home)) else {
        return Vec::new();
    };
    entries.flatten().map(|entry| entry.path()).collect()
}

/// A session's summary as it stands on disk, for a test reading a run that is
/// still going.
fn summary_of(dir: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(dir.join("summary.json")).ok()?;
    serde_json::from_str(&text).ok()
}

fn keyed(dir: &Path, key: &str) -> bool {
    summary_of(dir).is_some_and(|summary| summary["key"] == key)
}

/// One session's journal, as frames. The first line names the format; a last
/// line the run was still writing when it ended is not a frame either, so the
/// lines that parse are the journal, exactly as the store's own replay reads
/// it.
fn journal(home: &Path, key: &str) -> Vec<Frame> {
    let dir = session_dir(home, key).unwrap_or_else(|| panic!("no session is keyed {key}"));
    frames_at(&dir)
}

fn frames_at(dir: &Path) -> Vec<Frame> {
    let Ok(text) = std::fs::read_to_string(dir.join("journal.jsonl")) else {
        return Vec::new();
    };
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
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"@scout stand-up in five"}}}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]},
    {"steps":[{"text":"done"}]}
]}"##;

/// The whole of ADR-0021 §2, end to end: an agent opens a room under the
/// session that started it and posts into it, and the peer it named — an agent
/// it never started and cannot see — reads the room at the head of the turn
/// the post opened, told which room it is and who wrote what.
#[test]
fn an_agent_opens_a_shared_room_and_its_peer_reads_the_post() {
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

    let scout =
        session_dir(home.path(), &format!("agent/{root}/scout")).expect("the peer was seated");
    let read = readings(&scout);
    assert_eq!(
        read,
        ["[#design, since you last read]\nreviewer: @scout stand-up in five"],
        "the peer read the room once, under its own label and in the author\'s name"
    );
    assert!(
        posts(&frames_at(&scout))
            .iter()
            .all(|(text, _)| text != "@scout stand-up in five"),
        "and the post itself was copied into nobody: {:?}",
        posts(&frames_at(&scout))
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

// ---- the holder on the roster (ADR-0028) -----------------------------------

/// A project with one room and a resident scout to fill a seat in it. The
/// scout is seated when the root session opens, so a person can write to it
/// without the root running a turn to start it. `listeners` is where a seat
/// asks for an ear other than the default patient one (ADR-0034 §6).
fn with_a_room_of(home: &Path, members: &str, listeners: &str) {
    let bingo = home.join(".bingo");
    std::fs::create_dir_all(&bingo).unwrap();
    std::fs::write(
        bingo.join("team.json"),
        format!(
            r#"{{"roles":[{{"name":"scout"}}],"rooms":[{{"name":"design",
                "members":{members},"listeners":{listeners}}}]}}"#
        ),
    )
    .unwrap();
}

/// The same, with every seat on the ear a bare name asks for.
fn with_a_room(home: &Path, members: &str) {
    with_a_room_of(home, members, "[]");
}

/// The same project, with the holder seated on an ear the roster names
/// (ADR-0029 §2): `patience_s: 0` is woken by every post as it lands.
fn with_a_listening_room(home: &Path, listeners: &str) {
    with_a_room_of(home, r#"["scout"]"#, listeners);
}

/// The binary as a host drives it: person messages one line at a time, so a
/// run outlives its first turn, and every session's frames on stdout.
fn hosting(home: &Path, script: &tempfile::NamedTempFile) -> Command {
    let mut cmd = bingo();
    cmd.env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", home)
        .args([
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "json",
            "--cwd",
        ])
        .arg(home);
    cmd
}

/// The run's root session: the one on disk with nothing above it, which is
/// the holder of every room this project declares.
fn root_dir(home: &Path) -> Option<PathBuf> {
    dirs(home)
        .into_iter()
        .find(|dir| summary_of(dir).is_some_and(|summary| summary["parent"].is_null()))
}

/// The room this project declares, whatever the root's id turned out to be.
fn room_dir(home: &Path) -> Option<PathBuf> {
    dirs(home).into_iter().find(|dir| {
        summary_of(dir).is_some_and(|summary| {
            summary["key"]
                .as_str()
                .is_some_and(|key| key.starts_with("rooms/"))
        })
    })
}

/// The turns a session ran, by how many started.
fn turns(frames: &[Frame]) -> usize {
    frames
        .iter()
        .filter(|frame| matches!(frame.event, Event::TurnStarted { .. }))
        .count()
}

/// The turns a session finished, however they ended.
fn ended(frames: &[Frame]) -> usize {
    frames
        .iter()
        .filter(|frame| matches!(frame.event, Event::TurnCompleted { .. }))
        .count()
}

/// Whether a session is still thinking: a turn started that has not ended.
fn busy(dir: &Path) -> bool {
    let frames = frames_at(dir);
    turns(&frames) > ended(&frames)
}

/// The resident scout's session, whatever key it was seated under.
fn scout_dir(home: &Path) -> Option<PathBuf> {
    dirs(home).into_iter().find(|dir| {
        summary_of(dir).is_some_and(|summary| {
            summary["key"]
                .as_str()
                .is_some_and(|key| key.ends_with("/scout"))
        })
    })
}

fn until_posted(home: &Path, n: usize) {
    until("the room was never posted into", || {
        room_dir(home).is_some_and(|dir| posts(&frames_at(&dir)).len() >= n)
    });
}

/// What a session read of its rooms: the pieces the rooms contributor folded
/// into the head of a turn (ADR-0034 §4). A contributor\'s piece is journaled
/// under `contributor:<id>`, so this is exactly the room\'s own reading and
/// nothing else the session was told.
fn readings(dir: &Path) -> Vec<String> {
    posts(&frames_at(dir))
        .into_iter()
        .filter(|(_, origin)| origin.surface == "contributor:rooms")
        .map(|(text, _)| text)
        .collect()
}

/// How many posts a set of readings quotes: one line names the room, and every
/// line under it is a post.
fn quoted(readings: &[String]) -> usize {
    readings
        .iter()
        .map(|said| said.lines().count().saturating_sub(1))
        .sum()
}

/// Wait until the holder has read `n` of the room's posts. A reading is
/// journaled where it lands, so this is a fact of the run rather than of the
/// clock.
fn until_read(home: &Path, n: usize) {
    until("the room's posts were never read by the holder", || {
        root_dir(home).is_some_and(|dir| quoted(&readings(&dir)) >= n)
    });
}

/// Wait until the holder has answered the room and nobody is still thinking.
/// The fake provider hands its responses out in one run-wide sequence, so a
/// prompt sent while the scout is mid-turn would take the response written for
/// the holder.
fn until_the_room_settles(home: &Path) {
    until("the post never opened a turn on the holder", || {
        let (Some(root), Some(scout)) = (root_dir(home), scout_dir(home)) else {
            return false;
        };
        let frames = frames_at(&root);
        ended(&frames) > 0 && turns(&frames) == ended(&frames) && !busy(&scout)
    });
}

/// What a session was told, and who signed it. A reading is the session's own
/// turn folding a room it sits in (ADR-0034 §4), not something said into it,
/// so it is not one of these.
fn heard(dir: &Path) -> Vec<(String, Option<String>)> {
    posts(&frames_at(dir))
        .into_iter()
        .filter(|(_, origin)| !origin.surface.starts_with("contributor:"))
        .map(|(text, origin)| (text, origin.principal))
        .collect()
}

/// The `owed` cards a run published, in order.
fn cards(lines: &[serde_json::Value]) -> Vec<serde_json::Value> {
    lines
        .iter()
        .filter_map(|line| serde_json::from_value::<Frame>(line.clone()).ok())
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

/// The person writes to the scout, not to the root (`@name` in the composer,
/// ADR-0010 §2), and the roster this script is run against leaves the holder
/// out — so the root hears none of it and runs no turn while the room fills up,
/// which is what its one scenario is about.
const TWO_POSTS: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"the build is green"}}}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"and the tests pass"}}}]},
    {"steps":[{"text":"posted"}]},
    {"steps":[{"text":"they say it is green"}]}
]}"##;

/// A burst of two: both posts belong to one model response, so the room fills
/// without the script's run-wide cursor racing the holder's woken turn for the
/// call that makes them. Every response after it is the same word, so whoever
/// asks next — the scout finishing, the holder waking — is answered harmlessly.
const A_BURST: &str = r##"{"responses":[
    {"steps":[
        {"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"the build is green"}}},
        {"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"and the tests pass"}}}
    ]},
    {"steps":[{"text":"posted"}]},
    {"steps":[{"text":"posted"}]},
    {"steps":[{"text":"posted"}]},
    {"steps":[{"text":"posted"}]},
    {"steps":[{"text":"posted"}]},
    {"steps":[{"text":"posted"}]}
]}"##;

/// ADR-0028 §2 as amended by ADR-0034 §7, end to end: `parent` on the roster
/// seats the room's own holder, a live one is woken by a post like any other
/// seat, and the turn it opens reads the room. The person writes only to the
/// scout, so every turn the holder runs is one a post opened. How many turns a
/// burst costs is the scheduler's business and is deliberately not pinned;
/// what is pinned is that nothing is lost and the order is the room's.
#[test]
fn a_rostered_live_holder_is_woken_by_a_post_and_reads_the_room() {
    let home = tempfile::tempdir().unwrap();
    with_a_room_of(
        home.path(),
        r#"["scout","parent"]"#,
        r#"[{"name":"parent","patience_s":0}]"#,
    );
    let script = script(A_BURST);
    let mut host = Host::start(&mut hosting(home.path(), &script));

    host.prompt("@scout post what you found in #design");
    until_read(home.path(), 2);
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let root = root_dir(home.path()).expect("a root session");
    let read = readings(&root).join("\n");
    assert!(read.contains("[#design, since you last read]"), "{read}");
    assert!(
        read.find("scout: the build is green") < read.find("scout: and the tests pass"),
        "every post was read, in the room's order: {read}"
    );
    assert!(
        turns(&frames_at(&root)) > 0,
        "a post opened a turn on a holder nobody prompted"
    );
}

/// A member calls on the holder by name, and the holder answers. The two
/// responses either session may take are the same word, so the race between
/// the scout finishing and the holder waking decides nothing; and the answer
/// wakes the scout in its turn, so there are spare words for whatever is still
/// talking when stdin closes.
const CALLED_ON: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"@parent which one ships?"}}}]},
    {"steps":[{"text":"asked"}]},
    {"steps":[{"text":"asked"}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"the second one"}}}]},
    {"steps":[{"text":"answered"}]},
    {"steps":[{"text":"answered"}]},
    {"steps":[{"text":"answered"}]},
    {"steps":[{"text":"answered"}]},
    {"steps":[{"text":"answered"}]}
]}"##;

/// ADR-0028 §3 and ADR-0034 §3: `@parent` is obligation, and being named is
/// also what wakes a patient seat. The turn it opens reads the room first, and
/// the name opens an ordinary mention debt that the holder's own next post
/// closes.
#[test]
fn a_post_that_calls_on_the_holder_wakes_it_and_is_owed_an_answer() {
    let home = tempfile::tempdir().unwrap();
    with_a_room(home.path(), r#"["scout","parent"]"#);
    let script = script(CALLED_ON);
    let mut host = Host::start(&mut hosting(home.path(), &script));

    host.prompt("@scout ask me in #design");
    until_the_room_settles(home.path());
    host.prompt("answer them");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let root = root_dir(home.path()).expect("a root session");
    assert_eq!(
        readings(&root).first().map(String::as_str),
        Some("[#design, since you last read]\nscout: @parent which one ships?"),
        "the post opened the holder's first turn and was read in it: {:?}",
        readings(&root)
    );
    assert_eq!(
        turns(&frames_at(&root)),
        2,
        "the post's turn, and then the person's"
    );

    let room = room_dir(home.path()).expect("the room was opened");
    assert_eq!(
        heard(&room)
            .into_iter()
            .map(|(text, _)| text)
            .collect::<Vec<String>>(),
        ["@parent which one ships?", "the second one"],
        "the question and the answer"
    );

    let cards = cards(&ended.lines);
    let owing = cards
        .iter()
        .position(|card| card["rows"][0][1] == "parent")
        .unwrap_or_else(|| panic!("the holder never owed anything: {cards:?}"));
    assert_eq!(cards[owing]["rows"][0][0], "#design");
    assert_eq!(
        cards.last(),
        Some(&serde_json::Value::Null),
        "the holder's own post closed it: {cards:?}"
    );
}

/// ADR-0028 §4: explicit, never default. The same scenario with the holder off
/// the roster is today's room — it hears nothing of what is said there, and
/// all its journal holds is what the person said to it.
#[test]
fn a_room_that_does_not_seat_the_holder_leaves_it_deaf() {
    let home = tempfile::tempdir().unwrap();
    with_a_room(home.path(), r#"["scout"]"#);
    let script = script(TWO_POSTS);
    let mut host = Host::start(&mut hosting(home.path(), &script));

    host.prompt("@scout post what you found in #design");
    until_posted(home.path(), 2);
    host.prompt("what did they say?");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let root = root_dir(home.path()).expect("a root session");
    assert_eq!(turns(&frames_at(&root)), 1);
    assert!(
        readings(&root).is_empty(),
        "a room reaches into the tree, not up out of it: {:?}",
        readings(&root)
    );
    assert_eq!(heard(&root), [("what did they say?".to_string(), None)]);
}

// ---- the ear on every seat (ADR-0029) --------------------------------------

/// ADR-0029 §1 under ADR-0034 §4, end to end: a patient holder is woken by no
/// post and reads the room whole at the head of its next turn, whoever opens
/// it. The person writes only to the scout, so any turn the holder runs is one
/// a post opened — and the whole point is that it runs none until the person
/// speaks to it.
#[test]
fn a_patient_holder_reads_the_room_at_the_head_of_its_next_turn() {
    let home = tempfile::tempdir().unwrap();
    with_a_listening_room(home.path(), r#"["parent"]"#);
    let script = script(A_BURST);
    let mut host = Host::start(&mut hosting(home.path(), &script));

    host.prompt("@scout post what you found in #design");
    until_posted(home.path(), 2);

    let root = root_dir(home.path()).expect("a root session");
    assert_eq!(
        turns(&frames_at(&root)),
        0,
        "a patient seat was woken by a post: {:?}",
        heard(&root)
    );

    host.prompt("what did they say?");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    assert_eq!(
        readings(&root),
        ["[#design, since you last read]\nscout: the build is green\nscout: and the tests pass"],
        "both posts, in the room\'s order, in one reading"
    );
    assert_eq!(
        heard(&root),
        [("what did they say?".to_string(), None)],
        "and the person\'s own line is the only thing said into it"
    );
    assert_eq!(
        turns(&frames_at(&root)),
        1,
        "all of it in the one turn the person opened"
    );
}

/// One more post into the same room, for the process that resumes it.
const ONE_MORE: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"and it shipped"}}}]},
    {"steps":[{"text":"posted"}]},
    {"steps":[{"text":"posted"}]},
    {"steps":[{"text":"posted"}]},
    {"steps":[{"text":"posted"}]},
    {"steps":[{"text":"posted"}]}
]}"##;

/// ADR-0034 §2 across processes: the cursor is journaled on the member's own
/// session, so `--continue` finds it and the resumed member reads what landed
/// after it and nothing it had already read.
#[test]
fn a_resumed_member_reads_only_what_its_cursor_left() {
    let home = tempfile::tempdir().unwrap();
    with_a_listening_room(home.path(), r#"["parent"]"#);

    let first = script(A_BURST);
    let mut host = Host::start(&mut hosting(home.path(), &first));
    host.prompt("@scout post what you found in #design");
    until_posted(home.path(), 2);
    host.prompt("what did they say?");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let root = root_dir(home.path()).expect("a root session");
    let read = readings(&root);
    assert_eq!(
        quoted(&read),
        2,
        "the first process read both posts: {read:?}"
    );

    let again = script(ONE_MORE);
    let mut resumed = hosting(home.path(), &again);
    resumed.arg("--continue");
    let mut host = Host::start(&mut resumed);
    host.prompt("@scout post the last one");
    until_posted(home.path(), 3);
    host.prompt("what did they say now?");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    assert_eq!(
        readings(&root),
        [
            read[0].clone(),
            "[#design, since you last read]\nscout: and it shipped".to_string(),
        ],
        "the resumed member read what landed after its cursor, and no more"
    );
}

/// ADR-0029 §5: obligation pierces every ear. The same roster, and a post that
/// calls the holder by name wakes it at once and opens the ordinary debt.
#[test]
fn a_post_that_calls_on_a_patient_holder_wakes_it_at_once() {
    let home = tempfile::tempdir().unwrap();
    with_a_listening_room(home.path(), r#"[{"name":"parent","patience_s":600}]"#);
    let script = script(CALLED_ON);
    let mut host = Host::start(&mut hosting(home.path(), &script));

    host.prompt("@scout ask me in #design");
    until_read(home.path(), 1);
    until_the_room_settles(home.path());
    host.prompt("answer them");
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let root = root_dir(home.path()).expect("a root session");
    assert_eq!(
        readings(&root).first().map(String::as_str),
        Some("[#design, since you last read]\nscout: @parent which one ships?"),
        "the mention pierced the patient ear: {:?}",
        readings(&root)
    );
    assert_eq!(
        turns(&frames_at(&root)),
        2,
        "the post's own turn, and then the person's"
    );

    let cards = cards(&ended.lines);
    let owing = cards
        .iter()
        .position(|card| card["rows"][0][1] == "parent")
        .unwrap_or_else(|| panic!("the holder never owed anything: {cards:?}"));
    assert_eq!(cards[owing]["rows"][0][0], "#design");
    assert_eq!(
        cards.last(),
        Some(&serde_json::Value::Null),
        "the holder's own post closed it: {cards:?}"
    );
}

/// One tool call, and the room's journal keeps what the seat asked for.
const RETUNES: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"Listen","input":{"room":"design","patience_s":300}}}]},
    {"steps":[{"text":"listening"}]},
    {"steps":[{"text":"listening"}]},
    {"steps":[{"text":"listening"}]}
]}"##;

/// ADR-0029 §4: a member retunes its own seat and the room's journal keeps it
/// as a register of its own, beside the roster that declared the seat.
#[test]
fn a_member_listens_and_the_room_s_journal_says_what_it_now_hears() {
    let home = tempfile::tempdir().unwrap();
    with_a_room(home.path(), r#"["scout"]"#);
    let script = script(RETUNES);
    let mut cmd = hosting(home.path(), &script);
    cmd.args(["--allowed-tools", "Listen"]);
    let mut host = Host::start(&mut cmd);

    host.prompt("@scout listen to #design");
    until("the seat never said how it listens", || {
        room_dir(home.path()).is_some_and(|dir| !ears(&frames_at(&dir)).is_empty())
    });
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);

    let room = room_dir(home.path()).expect("the room was opened");
    assert_eq!(
        ears(&frames_at(&room)),
        [("ear:scout".to_string(), 300)],
        "one register, for the seat that asked and no other"
    );

    let scout = scout_dir(home.path()).expect("the resident scout");
    let told = tool_result(&frames_at(&scout), "Listen");
    assert!(!told.is_error, "{}", text_of(&told));
    let said = text_of(&told);
    assert!(said.contains("#design"), "{said}");
    assert!(
        said.contains("300s"),
        "the receipt names the ear it now wears: {said}"
    );
}

/// The ears a room's journal holds: this plugin's `ear:` registers, with the
/// patience each of them stores.
fn ears(frames: &[Frame]) -> Vec<(String, u64)> {
    frames
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::Extension {
                plugin,
                kind,
                payload,
            } if plugin == "bingo.rooms" && kind.starts_with("ear:") => {
                Some((kind.clone(), payload["patience_s"].as_u64()?))
            }
            _ => None,
        })
        .collect()
}

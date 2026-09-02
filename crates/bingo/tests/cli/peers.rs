//! Black-box: what one agent says to another. An agent writes to the teammate
//! beside it — one the same agent started, which it never spawned and cannot
//! see in its own tree — and every message wakes its target, so an answer to
//! an idle agent is never stranded (ADR-0024). A post written behind a room's
//! head is handed back with what it missed instead of landing (ADR-0025).

use std::path::{Path, PathBuf};

use bingo_sdk::{ContentPart, ItemBody, Origin, SessionId, ToolOutput};
use serde_json::Value;

use super::*;

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
/// room's posts and a bounced post never became one. The holder stays off the
/// roster — seated, it would absorb the room and post level (ADR-0028) — so
/// the standing question is the room's `@all`, which no post of the root's
/// can close: the root is not a member.
const STALE_ANSWER: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"morning"}}}]},
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"scout","prompt":"ask in #design","background":false}}}]},
    {"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#design","text":"@all what does the build say?"}}}]},
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
        r#"{"rooms":[{"name":"design","members":["scout"]}]}"#,
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
        text_of(bounced).contains("scout: @all what does the build say?"),
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
        Some("@all"),
        "the scout's question opened a debt: {owed:?}"
    );
    assert_eq!(
        owed.last().map(String::as_str),
        Some("@all"),
        "neither the bounce nor a non-member's landed post closed it: {owed:?}"
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

// ---- standby members (ADR-0027) --------------------------------------------

/// Who every call of `tool` was addressed to, in the order it was made.
fn addressed(frames: &[Frame], tool: &str) -> Vec<String> {
    frames
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                ItemBody::ToolCall { name, input, .. } if name == tool => {
                    Some(input["to"].as_str().unwrap_or("?").to_string())
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// The turns a session ran, by how many started.
fn turns(frames: &[Frame]) -> usize {
    frames
        .iter()
        .filter(|frame| matches!(frame.event, Event::TurnStarted { .. }))
        .count()
}

/// Three roles seated silent in one room, and one post to start them.
///
/// Four sessions share one script here, and the script is dealt to whoever
/// asks next, so every response is addressed to the side that must have it.
/// The parent is known by the person's prompt, which only its own transcript
/// carries; a member is known by the `[from … in #relay]` header a post is
/// fanned out under, which only a session that was handed one carries. The
/// parent is not in the room and no member is ever told what the person said,
/// so neither can take the other's turn however the two races land.
///
/// Within the members the race decides nothing, because the response that
/// posts the next number is addressed to a session that has *read* the last
/// one. That is what `when` is for. A post the fan-out has delivered but whose
/// target was mid-turn sits in that session's queue, not in its journal, and a
/// room is serial (ADR-0025): a post written from behind the head is handed
/// back rather than landed. Deal `count 2` to a session in that state and the
/// number is refused and gone, the relay has nothing left awake, and the count
/// stops — which is a true thing about a room, wrongly blamed on the room by a
/// script that let any of three sessions carry the post. Matching each round on
/// the previous number is the same precondition the serial rule enforces, so
/// the deck can no longer deal a post to a session that would be refused.
///
/// The parent's tail is a delay long enough to hold the run open while the
/// relay runs; it is a liveness bound, not a bet on who asks first.
const RELAY: &str = r##"{"responses":[
    {"when":{"contains":"seat the relay"},"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"alpha","prompt":"You count in #relay: when a post hands you a number, post the next one.","standby":true}}}]},
    {"when":{"contains":"seat the relay"},"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"beta","prompt":"You count in #relay: when a post hands you a number, post the next one.","standby":true}}}]},
    {"when":{"contains":"seat the relay"},"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"gamma","prompt":"You count in #relay: when a post hands you a number, post the next one.","standby":true}}}]},
    {"when":{"contains":"seat the relay"},"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#relay","text":"start the count"}}}]},
    {"when":{"contains":"seat the relay"},"steps":[{"delay":{"ms":5000}},{"text":"they have it"}]},
    {"when":{"contains":"start the count"},"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#relay","text":"count 1"}}}]},
    {"when":{"contains":"count 1"},"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#relay","text":"count 2"}}}]},
    {"when":{"contains":"count 2"},"steps":[{"toolCall":{"name":"SendMessage","input":{"to":"#relay","text":"count 3"}}}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"not mine"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"not mine"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"not mine"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"not mine"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"not mine"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"not mine"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"done"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"done"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"done"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"done"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"done"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"done"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"done"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"done"}]},
    {"when":{"contains":"in #relay]"},"steps":[{"text":"done"}]},
    {"when":{"contains":"seat the relay"},"steps":[{"text":"done"}]},
    {"when":{"contains":"seat the relay"},"steps":[{"text":"done"}]}
]}"##;

/// ADR-0027 end to end: three members seated silent, one kickoff post, and a
/// relay that runs itself. The parent writes to nobody by name and is told
/// nothing back — the count is in the room, which is where the work was.
#[test]
fn one_kickoff_post_runs_a_relay_the_parent_never_dispatches() {
    let home = tempfile::tempdir().unwrap();
    with_team(
        home.path(),
        r#"{"rooms":[{"name":"relay","members":["alpha","beta","gamma"]}]}"#,
    );
    let out = run_json(home.path(), &script(RELAY), "seat the relay and start it");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let root = root_of(&out);

    assert_eq!(
        posts(home.path(), &root, "relay"),
        ["start the count", "count 1", "count 2", "count 3"],
        "one post per round, and the count reached three"
    );
    assert_eq!(
        addressed(&frames_of(&out), "SendMessage"),
        ["#relay"],
        "the parent posted once and dispatched to nobody by name"
    );

    for member in ["alpha", "beta", "gamma"] {
        let heard = said(&journal(home.path(), &format!("agent/{root}/{member}")));
        let (brief, origin) = heard.first().expect("{member} read something");
        assert!(brief.starts_with("You count in #relay"), "{heard:?}");
        assert_eq!(origin.conversation, None, "the brief came from the spawn");
        assert_eq!(
            heard[1].0, "start the count",
            "the kickoff followed the brief it was read with: {heard:?}"
        );
    }
}

/// The other half of ADR-0027 §1: seating a member costs nothing until
/// something wakes it, and nothing here does.
const NEVER_WOKEN: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"understudy","prompt":"wait for the call","standby":true}}}]},
    {"steps":[{"text":"seated"}]}
]}"##;

#[test]
fn a_standby_member_nothing_wakes_runs_no_turn_at_all() {
    let home = tempfile::tempdir().unwrap();
    let out = run_json(home.path(), &script(NEVER_WOKEN), "seat an understudy");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let root = root_of(&out);

    let seated = journal(home.path(), &format!("agent/{root}/understudy"));
    assert_eq!(turns(&seated), 0, "a seated member idles at zero tokens");
    assert!(
        said(&seated).is_empty(),
        "its brief is held, not journalled: {:?}",
        said(&seated)
    );
}

/// ADR-0027 §5, where the model meets it.
const WAIT_FOR_A_STANDBY: &str = r##"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"ghost","prompt":"go","standby":true,"background":false}}}]},
    {"steps":[{"text":"it would have waited forever"}]}
]}"##;

#[test]
fn waiting_for_a_standby_agent_is_refused_before_one_is_started() {
    let home = tempfile::tempdir().unwrap();
    let out = run_json(home.path(), &script(WAIT_FOR_A_STANDBY), "wait for a ghost");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let root = root_of(&out);

    let spawned = results(&frames_of(&out), "SpawnAgent");
    let [refused] = spawned.as_slice() else {
        panic!("one spawn was attempted: {spawned:?}");
    };
    assert!(refused.is_error, "{}", text_of(refused));
    let words = text_of(refused);
    assert!(words.contains("wait forever"), "{words}");
    assert!(words.contains("background: true"), "{words}");
    assert!(
        session_dir(home.path(), &format!("agent/{root}/ghost")).is_none(),
        "nothing was started to be waited on: {:?}",
        keys(home.path())
    );
}

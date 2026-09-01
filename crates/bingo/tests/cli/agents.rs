//! A sub-agent is a child session (ADR-0010): `SpawnAgent` in the foreground
//! hands the child's own text back as the call's result, and the run's stdout
//! stays the root's prose. A team is the roles `.bingo/team.json` declares,
//! seated as children of the root when it opens (ADR-0011).

use super::*;

/// The last completed result of `tool`, as the model read it: the text, and
/// whether it read it as an error.
fn tool_result(out: &Output, tool: &str) -> bingo_sdk::ToolOutput {
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
}

/// The text a completed call to `tool` returned, as the model read it.
fn tool_output(out: &Output, tool: &str) -> String {
    tool_result(out, tool)
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

/// One session's journal as it was written, found by the key its summary
/// carries: an agent's is `agent/<root>/<name>`. Read for what a session
/// heard, which no stream of the root's shows.
fn agent_journal(home: &std::path::Path, key: &str) -> String {
    let sessions = home.join(".bingo/data/sessions");
    let dirs = std::fs::read_dir(&sessions).expect("the run wrote its sessions");
    for dir in dirs.flatten().map(|entry| entry.path()) {
        let summary = std::fs::read_to_string(dir.join("summary.json")).unwrap_or_default();
        if summary.contains(&format!("\"{key}\"")) {
            return std::fs::read_to_string(dir.join("journal.jsonl")).unwrap_or_default();
        }
    }
    panic!("no session is keyed {key}");
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

// ---- the join (M23, ADR-0027) ----------------------------------------------

/// Two agents, then one wait for both. Every response up to the wait goes out
/// in one order: a foreground spawn holds the root until the child's turn has
/// ended, so neither child is still running when the join begins.
const JOIN: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"alpha","prompt":"say who you are","background":false}}}]},
    {"steps":[{"text":"alpha is done"}]},
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"beta","prompt":"say who you are","background":false}}}]},
    {"steps":[{"text":"beta is done"}]},
    {"steps":[{"toolCall":{"name":"WaitAgent","input":{"agents":["beta","alpha"]}}}]},
    {"steps":[{"text":"both answered"}]}
]}"#;

#[test]
fn a_join_hands_back_every_reply_in_the_order_it_was_asked_for() {
    let home = tempfile::tempdir().unwrap();
    let out = scripted_run(home.path(), &script(JOIN), &[], "ask them both");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let joined = tool_result(&out, "WaitAgent");
    let text = tool_output(&out, "WaitAgent");
    assert!(!joined.is_error, "both agents answered: {text}");
    let beta = text
        .find("beta is done")
        .unwrap_or_else(|| panic!("{text}"));
    let alpha = text
        .find("alpha is done")
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        beta < alpha,
        "the order asked, not the order spawned: {text}"
    );
    assert_eq!(final_text(&out), "both answered");
}

/// One agent that finishes and one that will not, joined under a deadline the
/// second cannot meet. The spawn and the wait are one round, so the root asks
/// the provider for nothing between them: the background child takes the
/// slow response the moment it is woken, and the root's next request comes a
/// whole deadline later. The delay is fifteen times the deadline, so what the
/// run asserts does not turn on how fast the machine is.
const JOIN_DEADLINE: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"name":"done","prompt":"say the diff is fine","background":false}}}]},
    {"steps":[{"text":"the diff is fine"}]},
    {"steps":[
        {"toolCall":{"name":"SpawnAgent","input":{"name":"slow","prompt":"take your time","background":true}}},
        {"toolCall":{"name":"WaitAgent","input":{"agents":["done","slow"],"timeout_s":2}}}
    ]},
    {"steps":[{"delay":{"ms":30000}},{"text":"eventually"}]},
    {"steps":[{"text":"one of them is still at it"}]}
]}"#;

#[test]
fn a_deadline_names_who_finished_and_who_is_still_working() {
    let home = tempfile::tempdir().unwrap();
    let out = run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script(JOIN_DEADLINE).path())
            .env("HOME", home.path())
            .args(["--print", "--output-format", "json", "--cwd"])
            .arg(home.path())
            .arg("wait for both"),
        Duration::from_secs(60),
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let joined = tool_result(&out, "WaitAgent");
    let text = tool_output(&out, "WaitAgent");
    assert!(joined.is_error, "one of them did not answer: {text}");
    assert!(
        text.contains("the diff is fine"),
        "the reply that landed is still readable: {text}"
    );
    assert!(text.contains("still working after 2s"), "{text}");
    assert_eq!(final_text(&out), "one of them is still at it");
}

/// ADR-0027: a seated member's brief is journalled when it is absorbed, so a
/// member nothing has woken has said nothing and has no turn behind it. The
/// wait says that, and says it at once — the deadline is never reached,
/// because there is nothing to wait for.
const WAIT_ON_A_SEATED_MEMBER: &str = r#"{"responses":[
    {"steps":[
        {"toolCall":{"name":"SpawnAgent","input":{"name":"understudy","prompt":"wait for the call","standby":true}}},
        {"toolCall":{"name":"WaitAgent","input":{"agents":["understudy"],"timeout_s":600}}}
    ]},
    {"steps":[{"text":"it has not started"}]}
]}"#;

#[test]
fn waiting_on_an_unwoken_member_says_it_is_seated_not_finished() {
    let home = tempfile::tempdir().unwrap();
    let out = run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script(WAIT_ON_A_SEATED_MEMBER).path())
            .env("HOME", home.path())
            .args(["--print", "--output-format", "json", "--cwd"])
            .arg(home.path())
            .arg("wait for the understudy"),
        Duration::from_secs(60),
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let joined = tool_result(&out, "WaitAgent");
    let text = tool_output(&out, "WaitAgent");
    assert!(joined.is_error, "nothing has been said to read: {text}");
    assert!(
        text.contains("is seated and nothing has woken it"),
        "{text}"
    );
    assert!(!text.contains("finished without saying anything"), "{text}");
    assert!(
        !text.contains("still working"),
        "nothing was waited for: {text}"
    );
    let root = &frames_of(&out)[0].session;
    let seated = agent_journal(home.path(), &format!("agent/{root}/understudy"));
    assert!(
        !seated.contains(r#""type":"turnStarted""#),
        "no turn has ever run in it: {seated}"
    );
    assert!(
        !seated.contains(r#""type":"itemCompleted""#),
        "its brief is held in the queue, not journalled as an item: {seated}"
    );
    assert_eq!(final_text(&out), "it has not started");
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

//! A sub-agent is a child session (ADR-0010): `SpawnAgent` in the foreground
//! hands the child's own text back as the call's result, and the run's stdout
//! stays the root's prose. A team is the roles `.bingo/team.json` declares,
//! seated as children of the root when it opens (ADR-0011).

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

//! A sub-agent is a child session (ADR-0010): `SpawnAgent` in the foreground
//! hands the child's own text back as the call's result, and the run's stdout
//! stays the root's prose.

use super::*;

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

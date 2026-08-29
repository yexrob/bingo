//! A shell hook in the settings runs on Claude Code's contract: a
//! `PreToolUse` hook that says deny stops the tool before it runs, and the
//! model hears why.

use super::*;

#[test]
fn a_pre_tool_use_hook_that_denies_stops_the_write_and_tells_the_model() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let deny = r#"printf '%s' '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"not today"}}'"#;
    let settings = script(
        &serde_json::json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Write",
                    "hooks": [{ "type": "command", "command": deny }]
                }]
            }
        })
        .to_string(),
    );
    let fake = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Write","input":{"file_path":"x.txt","content":"no\n"}}}]},
            {"steps":[{"text":"understood"}]}
        ]}"#,
    );
    let out = run(bingo()
        .args(["--print", "--output-format", "json", "--cwd"])
        .arg(project.path())
        .arg("--settings")
        .arg(settings.path())
        .arg("write it")
        .env("HOME", home.path())
        .env("BINGO_FAKE_SCRIPT", fake.path()));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let frames: Vec<Frame> = stdout(&out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let denied = frames.iter().find_map(|f| match &f.event {
        Event::ItemCompleted { item } => match &item.body {
            bingo_sdk::ItemBody::ToolCall {
                name,
                output: Some(output),
                ..
            } if name == "Write" => Some(output.clone()),
            _ => None,
        },
        _ => None,
    });
    let output = denied.expect("the Write call completed with an output");
    assert!(output.is_error, "{output:?}");
    assert!(
        output.parts[0]
            .as_text()
            .is_some_and(|t| t.contains("not today")),
        "{output:?}"
    );
    assert!(
        !frames
            .iter()
            .any(|f| matches!(f.event, Event::InteractionOpened { .. })),
        "a hook's deny asks nobody"
    );
    assert!(!project.path().join("x.txt").exists());
    assert!(matches!(
        frames.last().map(|f| &f.event),
        Some(Event::TurnCompleted {
            status: TurnStatus::Completed,
            ..
        })
    ));
}

//! A `SKILL.md` in the project is a `/name` the kernel dispatches: its body,
//! with the arguments substituted, becomes the turn's prompt (ADR-0009 §3).

use super::*;

#[test]
fn a_project_skill_is_a_command_whose_body_becomes_the_prompt() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let skill = project.path().join(".bingo/skills/hello");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\ndescription: greet someone\nargument-hint: <name>\n---\nSay hello to $ARGUMENTS, warmly.\n",
    )
    .unwrap();
    let script = script(r#"{"responses":[{"steps":[{"text":"done"}]}]}"#);
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "--output-format", "json", "--cwd"])
        .arg(project.path())
        .arg("/hello world")
        .env("HOME", home.path()));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let frames: Vec<Frame> = stdout(&out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let asked = frames.iter().find_map(|f| match &f.event {
        Event::ItemCompleted { item } => match &item.body {
            bingo_sdk::ItemBody::User { parts, origin } => {
                Some((parts[0].as_text()?.to_owned(), origin.clone()))
            }
            _ => None,
        },
        _ => None,
    });
    let (prompt, origin) = asked.expect("the command's prompt is journaled");
    assert_eq!(
        origin.surface, "command",
        "the command spoke, not the surface it was typed on"
    );
    let lines: Vec<&str> = prompt.lines().collect();
    assert_eq!(
        lines.first().copied(),
        Some("/hello world"),
        "the line that was typed leads the prompt: {lines:?}"
    );
    assert!(
        lines[2].starts_with("Base directory for this skill: ") && lines[2].contains("hello"),
        "the expansion says where the skill lives: {lines:?}"
    );
    assert_eq!(
        lines.last().copied(),
        Some("Say hello to world, warmly."),
        "the skill's body, expanded, is what the model was asked: {lines:?}"
    );
    assert!(matches!(
        frames.last().map(|f| &f.event),
        Some(Event::TurnCompleted {
            status: TurnStatus::Completed,
            ..
        })
    ));
}

#[test]
fn an_unknown_slash_command_is_still_refused() {
    let home = tempfile::tempdir().unwrap();
    let script = script(r#"{"responses":[]}"#);
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "/nosuchskill now"])
        .env("HOME", home.path()));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("unknown command: /nosuchskill"),
        "{}",
        stderr(&out)
    );
}

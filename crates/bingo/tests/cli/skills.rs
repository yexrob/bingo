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
    let out = run(bingo()
        .args(["--print", "--output-format", "json", "--cwd"])
        .arg(project.path())
        .arg("/hello world")
        .env("HOME", home.path()));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let frames: Vec<Frame> = stdout(&out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let prompt = frames.iter().find_map(|f| match &f.event {
        Event::ItemCompleted { item } => match &item.body {
            bingo_sdk::ItemBody::User { parts, .. } => parts[0].as_text().map(str::to_owned),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(
        prompt.as_deref(),
        Some("Say hello to world, warmly.\n"),
        "the skill's body, expanded, is what the model was asked"
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
    let out = run(bingo()
        .args(["--print", "/nosuchskill now"])
        .env("HOME", home.path()));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("unknown command: /nosuchskill"),
        "{}",
        stderr(&out)
    );
}

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
        .envs(home_env(home.path())));
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
        .envs(home_env(home.path())));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("unknown command: /nosuchskill"),
        "{}",
        stderr(&out)
    );
}

/// What a completed call to `tool` handed back, as the model read it.
fn tool_result(out: &Output, tool: &str) -> String {
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
        .map(|output| {
            output
                .parts
                .iter()
                .filter_map(bingo_sdk::ContentPart::as_text)
                .collect()
        })
        .unwrap_or_else(|| panic!("no {tool} call completed: {}", stdout(out)))
}

#[test]
fn a_plugin_s_page_is_a_skill_the_model_can_read_by_name() {
    let home = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Skill","input":{"name":"guide-mcp"}}}]},
            {"steps":[{"text":"read"}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "--output-format", "json", "--cwd"])
        .arg(home.path())
        .arg("how do mcp servers work here?")
        .envs(home_env(home.path())));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let page = tool_result(&out, "Skill");
    assert!(
        page.starts_with("# MCP"),
        "the page the mcp plugin wrote is what came back: {page}"
    );
}

#[test]
fn the_prompt_lists_the_pages_the_loaded_plugins_wrote() {
    let home = tempfile::tempdir().unwrap();
    // The fake provider answers only a request that carries this text, and the
    // system prompt is part of what it reads: no line, no answer, no run.
    let script = script(
        r#"{"responses":[
            {"when":{"contains":"- guide-permissions —"},"steps":[{"text":"listed"}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "--cwd"])
        .arg(home.path())
        .arg("hello")
        .envs(home_env(home.path())));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "listed");
}

/// The pages a default build carries, one line each. The `# Skills` listing
/// and the map's own `## Pages` section are generated from the same gathering
/// (ADR-0054 §§2–3), so a plugin that owns a noun and wrote nothing about it
/// is missing from both, and this is where that shows.
///
/// One run per page: the fake provider answers only a request carrying the
/// line, so a missing page is a run that fails by name rather than a list
/// compared to a list.
#[test]
#[ignore = "until slice B1 lands"]
fn the_prompt_lists_a_page_for_every_plugin_that_owns_a_noun() {
    for page in [
        "guide-agents",
        "guide-channels",
        "guide-hooks",
        "guide-mcp",
        "guide-memory",
        "guide-permissions",
        "guide-rooms",
        "guide-skills",
        "guide-tui",
    ] {
        let out = asked_for(page);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{page} is on no line of the prompt: {}",
            stderr(&out)
        );
        assert_eq!(stdout(&out).trim(), "listed", "{page}");
    }
}

/// One `--print` run whose only answer is addressed to the prompt line `page`
/// would take.
fn asked_for(page: &str) -> Output {
    let home = tempfile::tempdir().unwrap();
    let script = script(&format!(
        r#"{{"responses":[
            {{"when":{{"contains":"- {page} —"}},"steps":[{{"text":"listed"}}]}}
        ]}}"#
    ));
    run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "--cwd"])
        .arg(home.path())
        .arg("hello")
        .envs(home_env(home.path())))
}

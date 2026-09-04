//! `ListModels` (ADR-0026): the model landscape as a model reads it —
//! providers with their sign-in state, models with the facts the embedded
//! snapshot carries — and a spawn that names one of each.

use super::*;

/// What a completed call to `tool` handed back, as the model read it.
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

fn text_of(output: &bingo_sdk::ToolOutput) -> String {
    output
        .parts
        .iter()
        .filter_map(bingo_sdk::ContentPart::as_text)
        .collect()
}

/// The lines indented under one provider's header: the models it serves.
fn block(listing: &str, provider: &str) -> Vec<String> {
    listing
        .lines()
        .skip_while(|line| !line.starts_with(&format!("{provider}  ")))
        .skip(1)
        .take_while(|line| line.starts_with("  "))
        .map(|line| line.trim().to_string())
        .collect()
}

fn line_of(lines: &[String], starts: &str) -> String {
    lines
        .iter()
        .find(|line| line.starts_with(starts))
        .unwrap_or_else(|| panic!("no line for {starts} in {lines:#?}"))
        .clone()
}

const LIST: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"ListModels","input":{}}}]},
    {"steps":[{"text":"listed"}]}
]}"#;

/// The scripted provider is registered like any other, so the listing names
/// it, its configured model and its sign-in state — and a catalogued model
/// beside it carries the snapshot's facts.
#[test]
fn list_models_names_every_provider_its_models_and_its_sign_in_state() {
    let home = tempfile::tempdir().unwrap();
    let script = script(LIST);
    let out = scripted_run(
        home.path(),
        &script,
        &["--model", "fake-1"],
        "what is there?",
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let listed = tool_result(&out, "ListModels");
    assert!(!listed.is_error, "{listed:?}");
    let listing = text_of(&listed);
    // Whether the endpoint has answered yet is the background's business, so
    // only the part that does not depend on it is pinned here.
    assert!(
        listing
            .lines()
            .any(|line| line.starts_with("fake  no sign-in needed")),
        "{listing}"
    );
    assert_eq!(
        line_of(&block(&listing, "fake"), "fake-1"),
        "fake-1  no facts in the snapshot",
        "a model the snapshot does not carry is listed without facts"
    );

    let sonnet = line_of(&block(&listing, "anthropic"), "claude-sonnet-4-5  ");
    for fact in ["context ", "output ", "reasoning", "images"] {
        assert!(sonnet.contains(fact), "{sonnet}");
    }
    assert!(
        listing.ends_with("asked what it serves."),
        "the listing says where its facts and its ids came from: {listing}"
    );
}

/// The two ids the listing hands out are the two `SpawnAgent` takes.
const SPAWN_ON_A_NAMED_MODEL: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"prompt":"say hi",
        "background":false,"provider":"fake","model":"fake-1"}}}]},
    {"steps":[{"text":"hi from the child"}]},
    {"steps":[{"text":"the child said hi"}]}
]}"#;

#[test]
fn a_spawn_that_names_a_provider_and_a_model_still_lands() {
    let home = tempfile::tempdir().unwrap();
    let script = script(SPAWN_ON_A_NAMED_MODEL);
    let out = scripted_run(home.path(), &script, &[], "spawn one on the fake provider");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let spawned = tool_result(&out, "SpawnAgent");
    let text = text_of(&spawned);
    assert!(!spawned.is_error, "{text}");
    assert!(text.contains("hi from the child"), "{text}");
}

/// `/models` is the kernel's own listing (ADR-0008 §4): each provider, where
/// its ids came from and how old that answer is. `/models refresh` asks the
/// endpoints now and says what came back.
#[test]
fn the_models_command_lists_the_catalogue_and_refreshes_on_demand() {
    let home = tempfile::tempdir().unwrap();
    let script = script(r#"{"responses":[]}"#);
    let ask = |line: &str| {
        run_within(
            bingo()
                .env("HOME", home.path())
                .env("BINGO_FAKE_SCRIPT", script.path())
                .args(["--print", "--cwd"])
                .arg(home.path())
                .arg(line),
            std::time::Duration::from_secs(20),
        )
    };

    let refreshed = ask("/models refresh");
    assert_eq!(
        refreshed.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&refreshed)
    );
    let counts = stdout(&refreshed);
    assert!(counts.contains("fake 1 models"), "{counts}");

    let listed = ask("/models");
    assert_eq!(listed.status.code(), Some(0), "stderr: {}", stderr(&listed));
    let listing = stdout(&listed);
    assert!(
        listing.contains("fake  1 models · from the endpoint · asked just now"),
        "{listing}"
    );
    assert!(listing.contains("\n  fake-1\n"), "{listing}");
}

/// An argument the command does not take is refused, not guessed at.
#[test]
fn models_refuses_an_argument_it_does_not_know() {
    let home = tempfile::tempdir().unwrap();
    let script = script(r#"{"responses":[]}"#);
    let out = run_within(
        bingo()
            .env("HOME", home.path())
            .env("BINGO_FAKE_SCRIPT", script.path())
            .args(["--print", "--cwd"])
            .arg(home.path())
            .arg("/models everything"),
        std::time::Duration::from_secs(20),
    );
    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout(&out));
    let err = stderr(&out);
    assert!(err.contains("unknown argument `everything`"), "{err}");
}

/// `--model` is a layer, not a decision. `/model` inside a session writes the
/// user settings so the next start opens on it; a flag names the model for
/// this run and leaves the file exactly as it found it — the same bargain
/// Claude Code's `--model` and Codex's `-c model=…` make.
#[test]
fn a_model_flag_is_not_remembered() {
    let home = tempfile::tempdir().unwrap();
    let script = script(r#"{"responses":[{"steps":[{"text":"ok"}]}]}"#);
    let out = scripted_run(home.path(), &script, &["--model", "fake-1"], "hello");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let settings = home.path().join(".bingo/settings.json");
    assert!(
        !settings.exists(),
        "a one-run override wrote {}: {}",
        settings.display(),
        std::fs::read_to_string(&settings).unwrap_or_default()
    );
}

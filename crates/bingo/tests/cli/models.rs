//! `ListModels` (ADR-0026): the model landscape as a model reads it —
//! providers with their sign-in state, models with the facts the embedded
//! snapshot carries — and a spawn that names one of each.

use super::journal::text_of;
use super::*;

/// What a completed call to `tool` handed back, as the model read it.
fn tool_result(out: &Output, tool: &str) -> bingo_sdk::ToolOutput {
    frames_of(out)
        .into_iter()
        .filter_map(|f| match f.event {
            Event::ItemCompleted { item } => match item.body {
                bingo_sdk::ItemBody::ToolCall { name, output, .. } if name == tool => {
                    output.map(|output| *output)
                }
                _ => None,
            },
            _ => None,
        })
        .next_back()
        .unwrap_or_else(|| panic!("no {tool} call completed: {}", stdout(out)))
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
                .envs(home_env(home.path()))
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
            .envs(home_env(home.path()))
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

/// `--model` is a one-run layer, not a deliberate `/model` choice. Neither
/// settings nor the next-start preference are written by a CLI override.
#[test]
fn a_model_flag_is_not_remembered() {
    let home = tempfile::tempdir().unwrap();
    let script = script(r#"{"responses":[{"steps":[{"text":"ok"}]}]}"#);
    let out = scripted_run(home.path(), &script, &["--model", "fake-1"], "hello");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    for relative in [
        ".bingo/settings.json",
        ".bingo/settings.toml",
        ".bingo/data/last-model.json",
    ] {
        let path = home.path().join(relative);
        assert!(
            !path.exists(),
            "a one-run override wrote {}",
            path.display()
        );
    }
}

const THINKING_SETTINGS: &str = r#"# Keep the user's settings and model notes.
provider = "fake"
model = "new-model"
thinking = "low"
maxTokens = 1024

# An unrelated model must not be rewritten.
[models."fake/other-model"]
reasoning = true # Independently declared capability.
contextWindow = 64000
images = true
"#;

const NON_REASONING_MODEL: &str = r#"
# Capability metadata is not permission to ask for effort.
[models."fake/new-model"]
reasoning = false # Keep the user's declaration.
contextWindow = 32000
maxOutput = 2048
images = false
"#;

fn thinking_home(metadata: &str) -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".bingo")).unwrap();
    std::fs::create_dir(home.path().join("project")).unwrap();
    std::fs::write(
        home.path().join(".bingo/settings.toml"),
        format!("{THINKING_SETTINGS}{metadata}"),
    )
    .unwrap();
    home
}

fn thinking_command(home: &std::path::Path, format: &str, prompt: &str) -> Output {
    let script = script(r#"{"responses":[]}"#);
    run_within(
        bingo()
            .envs(home_env(home))
            .env("BINGO_FAKE_SCRIPT", script.path())
            .args(["--print", "--output-format", format, "--cwd"])
            .arg(home.join("project"))
            .arg(prompt),
        PATIENCE,
    )
}

fn thinking_text(home: &std::path::Path, prompt: &str) -> String {
    let out = thinking_command(home, "text", prompt);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "");
    stdout(&out)
}

/// A model absent from the snapshot still receives the configured effort.
/// `/think` remembers effort, not a fabricated capability declaration.
#[test]
fn thinking_on_an_unknown_model_is_effective_and_remembered() {
    let home = thinking_home("");
    let path = home.path().join(".bingo/settings.toml");
    let listing = thinking_text(home.path(), "/model");
    assert!(
        listing.starts_with(
            "model: fake/new-model\nthinking: low\nusage: /model [<provider>/]<model>\n"
        )
    );
    assert!(listing.contains("pinned: none"), "{listing}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), THINKING_SETTINGS);
    assert_eq!(
        thinking_text(home.path(), "/think high --keep"),
        "⇄ thinking high · kept for new sessions\n"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        THINKING_SETTINGS.replace("thinking = \"low\"", "thinking = \"high\"")
    );
    let saved = super::settings::read(&path);
    assert_eq!(saved["thinking"], "high");
    assert!(saved["models"].get("fake/new-model").is_none());
    let listing = thinking_text(home.path(), "/model");
    assert!(listing.starts_with(
        "model: fake/new-model\nthinking: high\nusage: /model [<provider>/]<model>\n"
    ));
    assert!(listing.contains("pinned: none"), "{listing}");
    assert_eq!(
        thinking_text(home.path(), "/think"),
        "thinking: high\nusage: /think <minimal|low|medium|high|xhigh|max|off>\n"
    );
}

/// Every line remains a frame, even when changing effort on a model whose
/// metadata explicitly says it does not reason. The metadata is untouched.
#[test]
fn thinking_ignores_false_capability_metadata_without_rewriting_it() {
    let home = thinking_home(NON_REASONING_MODEL);
    let out = thinking_command(home.path(), "json", "/think xhigh --keep");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stderr(&out), "");
    let frames = frames_of(&out);
    let applied = frames.iter().find_map(|frame| match &frame.event {
        Event::IntentAck {
            outcome: bingo_sdk::IntentOutcome::Applied { result },
            ..
        } => Some(result),
        _ => None,
    });
    assert_eq!(
        applied.expect("the command is applied")["message"],
        "⇄ thinking xhigh · kept for new sessions"
    );
    let path = home.path().join(".bingo/settings.toml");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        format!("{THINKING_SETTINGS}{NON_REASONING_MODEL}")
            .replace("thinking = \"low\"", "thinking = \"xHigh\"")
    );
    let saved = super::settings::read(&path);
    assert_eq!(saved["thinking"], "xHigh");
    assert_eq!(saved["models"]["fake/new-model"]["reasoning"], false);
    let listing = thinking_text(home.path(), "/model");
    assert!(listing.starts_with(
        "model: fake/new-model\nthinking: xhigh\nusage: /model [<provider>/]<model>\n"
    ));
    assert!(listing.contains("pinned: none"), "{listing}");
}

#[test]
fn thinking_off_and_invalid_levels_preserve_model_overrides() {
    let reasoning_model = NON_REASONING_MODEL.replace("reasoning = false", "reasoning = true");
    for (index, metadata) in ["", NON_REASONING_MODEL, reasoning_model.as_str()]
        .into_iter()
        .enumerate()
    {
        let home = thinking_home(metadata);
        let path = home.path().join(".bingo/settings.toml");
        let original = std::fs::read_to_string(&path).unwrap();
        if index == 0 {
            let invalid = thinking_command(home.path(), "text", "/think maximum");
            assert_eq!(invalid.status.code(), Some(1));
            assert_eq!(stdout(&invalid), "");
            assert_eq!(
                stderr(&invalid),
                "[error] code=INVALID_INPUT msg=unknown thinking level: maximum\n"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
        assert_eq!(
            thinking_text(home.path(), "/think off --keep"),
            "⇄ thinking off · kept for new sessions\n"
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original.replace("thinking = \"low\"\n", "")
        );
        assert_eq!(
            super::settings::read(&path)["thinking"],
            serde_json::Value::Null
        );
        if index == 0 {
            let listing = thinking_text(home.path(), "/model");
            assert!(
                listing.starts_with("model: fake/new-model\nusage: /model [<provider>/]<model>\n")
            );
            assert!(listing.contains("pinned: none"), "{listing}");
        }
        assert_eq!(
            thinking_text(home.path(), "/think"),
            "thinking: off\nusage: /think <minimal|low|medium|high|xhigh|max|off>\n"
        );
    }
}

#[test]
fn model_is_a_next_start_default_but_only_keep_writes_settings() {
    let home = thinking_home("");
    let path = home.path().join(".bingo/settings.toml");
    for (command, expected) in [
        (
            "/model fake/another",
            "⇄ another · fake · thinking low · this session · next-start default\n",
        ),
        ("/think high", "⇄ thinking high · this session\n"),
    ] {
        assert_eq!(thinking_text(home.path(), command), expected);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), THINKING_SETTINGS);
    }
    assert_eq!(
        thinking_text(home.path(), "/model fake/another --keep"),
        "⇄ another · fake · thinking low · kept for new sessions\n"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        THINKING_SETTINGS.replace("model = \"new-model\"", "model = \"another\"")
    );
    let saved = super::settings::read(&path);
    assert_eq!(saved["provider"], "fake");
    assert_eq!(saved["model"], "another");
    assert_eq!(saved["thinking"], "low");
}

fn model_run(home: &std::path::Path, flags: &[&str], command: &str) -> Output {
    let script = script(r#"{"responses":[]}"#);
    run_within(
        bingo()
            .envs(home_env(home))
            .env("BINGO_FAKE_SCRIPT", script.path())
            .args(["--print", "--cwd"])
            .arg(home.join("project"))
            .args(flags)
            .arg(command),
        PATIENCE,
    )
}

fn model_report(home: &std::path::Path, flags: &[&str], model: &str) {
    let out = model_run(home, flags, "/model");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).starts_with(&format!("model: fake/{model}\n")),
        "{}",
        stdout(&out)
    );
}

#[test]
fn deliberate_model_choices_survive_restart_but_flags_and_bad_state_do_not_replace_them() {
    let home = thinking_home("");
    let path = home.path().join(".bingo/data/last-model.json");
    let selected = model_run(home.path(), &[], "/model fake/org/chosen");
    assert_eq!(selected.status.code(), Some(0), "{}", stderr(&selected));
    let before = std::fs::read(&path).unwrap();
    model_report(home.path(), &[], "org/chosen");
    model_report(home.path(), &["--model", "cli-choice"], "cli-choice");
    model_report(home.path(), &["--provider", "fake"], "new-model");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    model_report(home.path(), &[], "org/chosen");
    let rejected = model_run(home.path(), &[], "/model fake/");
    assert_eq!(rejected.status.code(), Some(1));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    for text in [
        "broken",
        r#"{"version":1,"provider":"removed","model":"stale"}"#,
    ] {
        std::fs::write(&path, text).unwrap();
        model_report(home.path(), &[], "new-model");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
    }
    assert_eq!(
        std::fs::read_to_string(home.path().join(".bingo/settings.toml")).unwrap(),
        THINKING_SETTINGS
    );
}

#[test]
fn settings_precedence_suppresses_remembered_pairs_without_changing_other_merges() {
    let home = thinking_home("");
    let selected = model_run(home.path(), &[], "/model fake/remembered");
    assert_eq!(selected.status.code(), Some(0), "{}", stderr(&selected));
    let path = home.path().join(".bingo/data/last-model.json");
    let before = std::fs::read(&path).unwrap();
    let project = home.path().join("project/.bingo");
    std::fs::create_dir_all(&project).unwrap();
    let extra = home.path().join("explicit.toml");
    std::fs::write(&extra, "thinking = \"high\"\n").unwrap();
    let settings = ["--settings", extra.to_str().unwrap()];
    model_report(home.path(), &settings, "remembered");
    std::fs::write(project.join("settings.toml"), "provider = \"fake\"\n").unwrap();
    model_report(home.path(), &settings, "new-model");
    std::fs::write(project.join("settings.toml"), "model = \"project\"\n").unwrap();
    model_report(home.path(), &settings, "project");
    std::fs::write(project.join("settings.local.toml"), "model = \"local\"\n").unwrap();
    model_report(home.path(), &settings, "local");
    std::fs::write(&extra, "model = \"explicit\"\nthinking = \"high\"\n").unwrap();
    model_report(home.path(), &settings, "explicit");
    let cli = ["--settings", extra.to_str().unwrap(), "--model", "cli"];
    model_report(home.path(), &cli, "cli");
    let report = stdout(&model_run(home.path(), &cli, "/model"));
    assert!(report.contains("thinking: high"), "{report}");
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

/// `provider/` names no model: print refuses it on stderr instead of setting a
/// model literally called `provider/`.
#[test]
fn a_provider_with_no_model_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let script = script(r#"{"responses":[]}"#);
    let out = run_within(
        bingo()
            .envs(home_env(home.path()))
            .env("BINGO_FAKE_SCRIPT", script.path())
            .args(["--print", "--cwd"])
            .arg(home.path())
            .arg("/model fake/"),
        std::time::Duration::from_secs(20),
    );
    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout(&out));
    assert_eq!(stdout(&out), "");
    let err = stderr(&out);
    assert!(err.contains("`fake/` names no model"), "{err}");
}

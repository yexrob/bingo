//! `bingo plugins list|enable|disable` and what a switch does to the next run
//! (ADR-0057). The answer is the one thing on stdout; every diagnostic is on
//! stderr and a refusal is one `[error]` line and a non-zero exit.

use super::*;

fn user_settings(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".bingo/settings.toml")
}

fn settings_json(home: &std::path::Path) -> serde_json::Value {
    super::settings::read(&user_settings(home))
}

fn plugins(home: &std::path::Path) -> Command {
    let mut cmd = bingo();
    cmd.envs(home_env(home))
        .arg("--cwd")
        .arg(home)
        .arg("plugins");
    cmd
}

/// One row of a listing, by the name it starts with.
fn row<'a>(listing: &'a str, name: &str) -> Vec<&'a str> {
    let prefix = format!("{name}\t");
    listing
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no {name} row in {listing}"))
        .split('\t')
        .collect()
}

/// A `--print` run over the fake provider, with the frames on stdout so a
/// tool's answer can be read back.
fn scripted(home: &std::path::Path, script: &tempfile::NamedTempFile, prompt: &str) -> Output {
    run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .envs(home_env(home))
        .args(["--print", "--output-format", "json", "--cwd"])
        .arg(home)
        .arg(prompt))
}

/// The one response a script needs when the command answers by itself.
const UNUSED: &str = r#"{"responses":[{"steps":[{"text":"unused"}]}]}"#;

const FETCH: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"WebFetch","input":{"url":"https://example.com"}}}]},
    {"steps":[{"text":"I could not fetch it."}]}
]}"#;

/// A switch is written to the user layer, shown in the listing, and taken at
/// the next start: the model is not offered a tool whose plugin is off.
#[test]
fn a_tool_plugin_switched_off_is_written_listed_and_gone_from_the_model_s_tools() {
    let home = tempfile::tempdir().unwrap();
    let fetch = script(FETCH);
    let before = scripted(home.path(), &fetch, "fetch it");
    assert!(
        !stdout(&before).contains("tool not found"),
        "the tool is there until it is switched off: {}",
        stdout(&before)
    );

    let off = run(plugins(home.path()).args(["disable", "bingo.tools.web"]));
    assert_eq!(off.status.code(), Some(0), "stderr: {}", stderr(&off));
    assert_eq!(stdout(&off), "bingo.tools.web is off at the next start.\n");
    assert_eq!(stderr(&off), "");
    assert_eq!(
        settings_json(home.path())["enabledPlugins"],
        serde_json::json!({ "bingo.tools.web": false })
    );
    assert!(
        !home.path().join(".bingo/settings.local.json").exists(),
        "a project file is never written"
    );

    let listed = run(plugins(home.path()).arg("list"));
    assert_eq!(listed.status.code(), Some(0), "stderr: {}", stderr(&listed));
    assert_eq!(stderr(&listed), "");
    let said = stdout(&listed);
    assert_eq!(row(&said, "bingo.tools.web")[2], "off");
    assert_eq!(
        row(&said, "bingo.tools.web")[3],
        "switched off in the settings"
    );
    assert_eq!(
        row(&said, "bingo.tools.fs")[2],
        "on",
        "and its neighbours are"
    );

    let again = script(FETCH);
    let after = scripted(home.path(), &again, "fetch it");
    assert_eq!(after.status.code(), Some(0), "stderr: {}", stderr(&after));
    assert!(
        stdout(&after).contains("tool not found: WebFetch"),
        "the model reached for a tool this run does not have: {}",
        stdout(&after)
    );

    let unused = script(UNUSED);
    let table = run(bingo()
        .env("BINGO_FAKE_SCRIPT", unused.path())
        .envs(home_env(home.path()))
        .args(["--print", "--cwd"])
        .arg(home.path())
        .arg("/plugins"));
    assert_eq!(table.status.code(), Some(0), "stderr: {}", stderr(&table));
    let drawn = stdout(&table);
    assert!(drawn.contains("bingo.tools.web"), "{drawn}");
    assert!(drawn.contains("switched off in the settings"), "{drawn}");

    let on = run(plugins(home.path()).args(["enable", "bingo.tools.web"]));
    assert_eq!(stdout(&on), "bingo.tools.web is on at the next start.\n");
    assert_eq!(
        settings_json(home.path())["enabledPlugins"],
        serde_json::json!({ "bingo.tools.web": true }),
        "a switch flips rather than being taken out"
    );
}

/// The two switches nobody may write: one the binary cannot run without, and
/// a name no listing knows (ADR-0057 §3).
#[test]
fn a_needed_plugin_and_a_name_nobody_lists_are_refused_with_the_reason() {
    let home = tempfile::tempdir().unwrap();

    let refused = run(plugins(home.path()).args(["disable", "bingo.store.jsonl"]));
    assert_eq!(refused.status.code(), Some(1));
    assert_eq!(stdout(&refused), "");
    let said = stderr(&refused);
    assert!(said.contains("code=INVALID_INPUT"), "{said}");
    assert!(said.contains("ignores its switch"), "{said}");
    assert!(
        !user_settings(home.path()).exists(),
        "a refusal writes nothing"
    );

    let unknown = run(plugins(home.path()).args(["disable", "bingo.tools.wb"]));
    assert_eq!(unknown.status.code(), Some(1));
    assert_eq!(stdout(&unknown), "");
    assert!(
        stderr(&unknown).contains("bingo plugins list"),
        "a person with the wrong spelling is told where to look: {}",
        stderr(&unknown)
    );
    assert!(!user_settings(home.path()).exists());
}

/// The example this repository ships, installed the way a person installs one.
fn install_wordcount(home: &std::path::Path) {
    let example = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/plugins/wordcount");
    let plugin = home.join(".bingo/plugins/wordcount");
    std::fs::create_dir_all(&plugin).unwrap();
    for file in ["plugin.json", "main.py"] {
        std::fs::copy(example.join(file), plugin.join(file)).unwrap();
    }
}

const COUNT: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"plugin__wordcount__count","input":{"path":"notes.txt"}}}]},
    {"steps":[{"text":"Counted."}]}
]}"#;

/// An external plugin is switched by the same key and listed in the same
/// table, and switched off its process is never spawned — so its tool never
/// reaches the model (ADR-0057 §4, §5). Skipped without `python3`, as the
/// bridge's own black-box suite is.
#[test]
fn an_external_plugin_is_listed_switched_and_never_spawned() {
    if !python::python3() {
        eprintln!("skipped: no python3");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    install_wordcount(home.path());
    std::fs::write(home.path().join("notes.txt"), "alpha beta\n").unwrap();

    let listed = run(plugins(home.path()).arg("list"));
    let said = stdout(&listed);
    assert_eq!(
        row(&said, "wordcount")[1],
        "0.1.0",
        "its manifest's version"
    );
    assert_eq!(row(&said, "wordcount")[2], "on");

    let off = run(plugins(home.path()).args(["disable", "wordcount"]));
    assert_eq!(off.status.code(), Some(0), "stderr: {}", stderr(&off));
    assert_eq!(stdout(&off), "wordcount is off at the next start.\n");
    let said = stdout(&run(plugins(home.path()).arg("list")));
    assert_eq!(row(&said, "wordcount")[2], "off");
    assert_eq!(row(&said, "wordcount")[3], "switched off in the settings");

    let count = script(COUNT);
    let out = scripted(home.path(), &count, "count the words in notes.txt");
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("tool not found: plugin__wordcount__count"),
        "the process was never spawned, so it contributed nothing: {}",
        stdout(&out)
    );

    let unused = script(UNUSED);
    let table = run(bingo()
        .env("BINGO_FAKE_SCRIPT", unused.path())
        .envs(home_env(home.path()))
        .args(["--print", "--cwd"])
        .arg(home.path())
        .arg("/modules"));
    assert_eq!(table.status.code(), Some(0), "stderr: {}", stderr(&table));
    let drawn = stdout(&table);
    assert!(
        drawn.contains("wordcount") && drawn.contains("plugin-rpc"),
        "the bridge's own plugins are in the kernel's table, named for it: {drawn}"
    );
}

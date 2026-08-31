//! Black-box: the wordcount example plugin, driven through the real binary.
//!
//! The exit criterion of ADR-0015 §7 — a third party writes one Tool and one
//! Command in a language that is not Rust, and both run. Nothing here knows
//! anything about the bridge's insides: a directory with a `plugin.json` in
//! it, a prompt, and what came out.
//!
//! Every test skips where `python3` is absent, so a machine without it says so
//! rather than failing.

// An integration test is not `cfg(test)`; the test-only lint relief is spelled out.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use bingo_sdk::{Event, Frame, InteractionKind, ItemBody};

/// The example this repository ships, which is what a person would copy.
fn example() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/wordcount")
}

/// Whether a plugin written in Python can run here at all.
fn python3() -> bool {
    Command::new("python3")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// A home with the example installed the way a person installs one, and a
/// project with something to count in it.
fn installed() -> (tempfile::TempDir, tempfile::TempDir) {
    let home = tempfile::tempdir().unwrap();
    let plugin = home.path().join(".bingo/plugins/wordcount");
    std::fs::create_dir_all(&plugin).unwrap();
    for file in ["plugin.json", "main.py"] {
        std::fs::copy(example().join(file), plugin.join(file)).unwrap();
    }
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("notes.txt"),
        "alpha beta gamma\ndelta epsilon\n",
    )
    .unwrap();
    (home, project)
}

fn script(dir: &Path, json: &str) -> PathBuf {
    let path = dir.join("script.json");
    std::fs::write(&path, json).unwrap();
    path
}

fn bingo(home: &Path, project: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bingo"));
    command
        .env("HOME", home)
        .env_remove("BINGO_FAKE_SCRIPT")
        .args(["--print", "--cwd"])
        .arg(project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run(command: &mut Command) -> Output {
    command.output().expect("the binary runs")
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("utf-8 stdout")
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("utf-8 stderr")
}

fn frames(out: &Output) -> Vec<Frame> {
    stdout(out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

/// The script that makes the model call the plugin's tool.
const COUNT: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"plugin__wordcount__count","input":{"path":"notes.txt"}}}]},
    {"steps":[{"text":"Counted."}]}
]}"#;

#[test]
fn the_plugin_s_tool_reaches_the_model_named_for_the_plugin_and_untrusted() {
    if !python3() {
        eprintln!("skipped: no python3");
        return;
    }
    let (home, project) = installed();
    let script = script(home.path(), COUNT);
    let out = run(bingo(home.path(), project.path())
        .env("BINGO_FAKE_SCRIPT", &script)
        .args(["--output-format", "json"])
        .arg("count the words in notes.txt"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let frames = frames(&out);
    let asked = frames.iter().find_map(|f| match &f.event {
        Event::InteractionOpened { interaction } => match &interaction.kind {
            InteractionKind::Permission { tool, .. } => Some(tool.clone()),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(
        asked.as_deref(),
        Some("plugin__wordcount__count"),
        "the gate asked about the plugin's tool, by the name the card shows"
    );
    let denied = frames.iter().any(|f| {
        matches!(
            &f.event,
            Event::InteractionResolved {
                answer: bingo_sdk::Answer::Deny { .. },
                ..
            }
        )
    });
    assert!(denied, "and nobody was at the keyboard to allow it");
}

#[test]
fn the_plugin_s_tool_counts_the_file_the_session_is_working_in() {
    if !python3() {
        eprintln!("skipped: no python3");
        return;
    }
    let (home, project) = installed();
    let script = script(home.path(), COUNT);
    let out = run(bingo(home.path(), project.path())
        .env("BINGO_FAKE_SCRIPT", &script)
        .args(["--output-format", "json", "--dangerously-skip-permissions"])
        .arg("count the words in notes.txt"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));

    let output = frames(&out).into_iter().find_map(|f| match f.event {
        Event::ItemCompleted { item } => match item.body {
            ItemBody::ToolCall {
                name,
                output: Some(output),
                ..
            } if name == "plugin__wordcount__count" => Some(output),
            _ => None,
        },
        _ => None,
    });
    let output = output.expect("the plugin's tool ran and answered");
    assert!(!output.is_error, "{output:?}");
    assert_eq!(
        output.parts[0].as_text(),
        Some("5 words, 2 lines, 31 characters"),
        "the call carried the session's working directory, so the file was found"
    );
    let table = output.display.expect("a plugin ships a View of its own");
    assert!(table.fold().contains("notes.txt"), "{}", table.fold());
}

#[test]
fn the_plugin_s_command_answers_with_its_view() {
    if !python3() {
        eprintln!("skipped: no python3");
        return;
    }
    let (home, project) = installed();
    // The command answers by itself, but a session still needs a provider
    // (f251b1f: without a script there is no fake one). The response is
    // never consumed.
    let script = script(
        home.path(),
        r#"{"responses":[{"steps":[{"text":"unused"}]}]}"#,
    );
    let out = run(bingo(home.path(), project.path())
        .env("BINGO_FAKE_SCRIPT", &script)
        .arg("/wordcount notes.txt"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let said = stdout(&out);
    assert!(said.contains("notes.txt"), "{said}");
    assert!(said.contains('5') && said.contains("31"), "{said}");
}

/// With no `plugins/` directory anywhere the bridge is inert: the binary
/// behaves exactly as it did before this plugin existed.
#[test]
fn a_host_with_no_plugins_installed_runs_as_it_always_did() {
    let home = tempfile::tempdir().unwrap();
    let project = tempfile::tempdir().unwrap();
    let script = script(
        home.path(),
        r#"{"responses":[{"steps":[{"text":"Hello from the fake provider."}]}]}"#,
    );
    let out = run(bingo(home.path(), project.path())
        .env("BINGO_FAKE_SCRIPT", &script)
        .arg("hello"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Hello from the fake provider.\n");
    assert_eq!(stderr(&out), "");
}

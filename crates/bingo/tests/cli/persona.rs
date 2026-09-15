//! The stance (M99): the words are tested for what they reach, not for what
//! they say. A response the fake provider will only hand over when the block
//! is in the request is the proof that it got there.

use super::*;

/// A project that has written its own settings, at `.bingo/settings.json`.
fn project(settings: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".bingo")).unwrap();
    std::fs::write(dir.path().join(".bingo/settings.json"), settings).unwrap();
    dir
}

#[test]
fn the_stance_reaches_the_model_in_the_system_prompt() {
    let script = script(
        r#"{"responses":[
            {"when":{"contains":"Never deviate silently"},"steps":[{"text":"Heard."}]}
        ]}"#,
    );
    let out = run(bingo().env("BINGO_FAKE_SCRIPT", script.path()).args([
        "--print",
        "--provider",
        "fake",
        "hello",
    ]));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Heard.\n");
}

/// The person's words replace the whole block rather than joining it: the
/// first response is a trap that only a request still carrying the shipped
/// stance can take, and it fails the turn (ADR-0059 §1).
#[test]
fn a_projects_own_text_replaces_the_stance_entirely() {
    let project = project(r#"{"persona": {"text": "You are Bingo the pirate."}}"#);
    let script = script(
        r#"{"responses":[
            {"when":{"contains":"Never deviate silently"},
             "steps":[{"error":{"kind":"request","message":"the shipped stance is still here"}}]},
            {"when":{"contains":"You are Bingo the pirate."},"steps":[{"text":"Arr."}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "--provider", "fake", "--cwd"])
        .arg(project.path())
        .arg("hello"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Arr.\n");
}

/// A misspelled field under the key stops the run and says which word it did
/// not know, rather than leaving the shipped stance in force silently.
#[test]
fn a_typo_under_the_key_stops_the_run_and_names_the_field() {
    let project = project(r#"{"persona": {"txet": "arr"}}"#);
    let script = script(r#"{"responses":[{"steps":[{"text":"never asked"}]}]}"#);
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "--provider", "fake", "--cwd"])
        .arg(project.path())
        .arg("hello"));
    assert_ne!(out.status.code(), Some(0), "stdout: {}", stdout(&out));
    assert!(stderr(&out).contains("txet"), "{}", stderr(&out));
}

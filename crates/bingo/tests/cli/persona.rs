//! The stance (M99): the words are tested for what they reach, not for what
//! they say. A response the fake provider will only hand over when the block
//! is in the request is the proof that it got there.

use super::*;

/// A project that has written its own settings, at `.bingo/settings.toml`.
fn project(settings: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".bingo")).unwrap();
    std::fs::write(dir.path().join(".bingo/settings.toml"), settings).unwrap();
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

/// The plugin has something to say about itself, so it writes a page, and the
/// page is one line of the prompt's `# Skills` listing (ADR-0054 §§1–2). The
/// fake provider answers only a request carrying that line: no page, no
/// answer, no run.
#[test]
fn the_page_this_plugin_wrote_is_listed_in_the_prompt() {
    let home = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"when":{"contains":"- guide-persona —"},"steps":[{"text":"listed"}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "--provider", "fake", "--cwd"])
        .arg(home.path())
        .arg("hello")
        .envs(home_env(home.path())));
    assert_eq!(
        out.status.code(),
        Some(0),
        "guide-persona is on no line of the prompt: {}",
        stderr(&out)
    );
    assert_eq!(stdout(&out).trim(), "listed");
}

/// Where the block sits: after the kernel's own two blocks, before the words
/// the project left. The request the fake provider matches on is the system
/// prompt joined by newlines, so each seam is one needle.
#[test]
fn the_stance_sits_between_the_kernels_blocks_and_the_projects_own() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("AGENTS.md"), "be brief\n").unwrap();
    for seam in ["</env>\n# Judgement", "left behind.\n# Instructions from "] {
        let script = script(
            &serde_json::json!({"responses":[
                {"when":{"contains":seam},"steps":[{"text":"Seen."}]}
            ]})
            .to_string(),
        );
        let out = run(bingo()
            .env("BINGO_FAKE_SCRIPT", script.path())
            .args(["--print", "--provider", "fake", "--cwd"])
            .arg(project.path())
            .arg("hello"));
        assert_eq!(
            out.status.code(),
            Some(0),
            "no request carried {seam:?}: {}",
            stderr(&out)
        );
        assert_eq!(stdout(&out), "Seen.\n");
    }
}

/// The person's words replace the whole block rather than joining it: the
/// first response is a trap that only a request still carrying the shipped
/// stance can take, and it fails the turn (ADR-0059 §1).
#[test]
fn a_projects_own_text_replaces_the_stance_entirely() {
    let project = project("[persona]\ntext = \"You are Bingo the pirate.\"\n");
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
    let project = project("[persona]\ntxet = \"arr\"\n");
    let script = script(r#"{"responses":[{"steps":[{"text":"never asked"}]}]}"#);
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "--provider", "fake", "--cwd"])
        .arg(project.path())
        .arg("hello"));
    assert_ne!(out.status.code(), Some(0), "stdout: {}", stdout(&out));
    assert!(stderr(&out).contains("txet"), "{}", stderr(&out));
}

/// The plugin's off switch is the kernel's (ADR-0059 §1, ADR-0057 §1): with
/// `enabledPlugins["bingo.persona"] = false` the block is not in the request,
/// and the only response the script offers is the one that a request still
/// carrying it cannot take.
#[test]
fn switched_off_the_stance_is_not_in_the_prompt() {
    let project = project("[enabledPlugins]\n\"bingo.persona\" = false\n");
    let script = script(
        r#"{"responses":[
            {"when":{"contains":"Never deviate silently"},
             "steps":[{"error":{"kind":"request","message":"the stance is still here"}}]},
            {"steps":[{"text":"Quiet."}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "--provider", "fake", "--cwd"])
        .arg(project.path())
        .arg("hello"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Quiet.\n");
}

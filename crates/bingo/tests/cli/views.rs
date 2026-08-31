//! ADR-0013 §2 on the headless surfaces: a tool's display view rides the
//! json frames verbatim and prints as its fold under the verdict in text mode.

use bingo_sdk::{Event, ItemBody, View};

use super::*;

const EDIT: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"Edit","input":{"file_path":"greeting.txt","old_string":"alpha","new_string":"beta"}}}]},
    {"steps":[{"text":"Done."}]}
]}"#;

fn edited_run(dir: &std::path::Path, format: &[&str]) -> Output {
    std::fs::write(dir.join("greeting.txt"), "alpha\n").unwrap();
    let script = script(EDIT);
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "--permission-mode", "acceptEdits", "--cwd"])
        .arg(dir)
        .args(format)
        .arg("change it"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(dir.join("greeting.txt")).unwrap(),
        "beta\n"
    );
    out
}

#[test]
fn an_edit_carries_its_diff_view_in_the_json_frames() {
    let dir = tempfile::tempdir().unwrap();
    let out = edited_run(dir.path(), &["--output-format", "json"]);
    let display = frames_of(&out).into_iter().find_map(|f| match f.event {
        Event::ItemCompleted {
            item:
                bingo_sdk::Item {
                    body:
                        ItemBody::ToolCall {
                            output: Some(output),
                            ..
                        },
                    ..
                },
        } => output.display,
        _ => None,
    });
    match display {
        Some(View::Diff { unified }) => {
            assert!(
                unified.contains("-alpha") && unified.contains("+beta"),
                "{unified}"
            );
        }
        other => panic!("the edit's display is its diff, not {other:?}"),
    }
}

#[test]
fn an_edit_prints_its_diff_under_the_verdict_in_text_mode() {
    let dir = tempfile::tempdir().unwrap();
    let out = edited_run(dir.path(), &[]);
    let err = stderr(&out);
    let verdict = err
        .find("[tool] Edit ok")
        .unwrap_or_else(|| panic!("no verdict in: {err}"));
    let after = &err[verdict..];
    assert!(after.contains("\n  -alpha\n  +beta\n"), "{err}");
}

/// An instant command under --print answers and the run ends: this hung
/// forever before the Applied ack became an exit (found in M11d's review).
#[test]
fn an_instant_command_prints_its_answer_and_leaves() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(r#"{"responses":[]}"#);
    let out = run_within(
        bingo()
            .env("BINGO_FAKE_SCRIPT", script.path())
            .args(["--print", "--cwd"])
            .arg(dir.path())
            .arg("/status"),
        std::time::Duration::from_secs(20),
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let answer = stdout(&out);
    assert!(answer.contains("mode: default"), "{answer}");
    assert!(answer.contains("cwd: "), "{answer}");
}

/// The fake provider exists for the scripted harness alone: a binary run
/// without `BINGO_FAKE_SCRIPT` never defaults to it — with nothing else
/// configured it refuses with the real provider's hint instead of quietly
/// answering from a script.
#[test]
fn a_binary_without_a_script_has_no_fake_provider() {
    let dir = tempfile::tempdir().unwrap();
    let out = run_within(
        bingo()
            .env("HOME", dir.path())
            .env_remove("ANTHROPIC_API_KEY")
            .args(["--print", "--cwd"])
            .arg(dir.path())
            .arg("/status"),
        std::time::Duration::from_secs(20),
    );
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.contains("AUTH_REQUIRED"), "{err}");
    assert!(err.contains("anthropic"), "{err}");
    assert!(!err.contains("fake"), "{err}");
}

//! Black-box: the binary as a host would run it. Stdout carries prose (or
//! frames) and nothing else; every diagnostic is on stderr; a failure is
//! one `[error] code=… msg=…` line and a non-zero exit.

use std::io::Write;
use std::process::{Command, Output, Stdio};

use bingo_sdk::{Event, Frame, TurnStatus};

fn bingo() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bingo"));
    cmd.env_remove("BINGO_FAKE_SCRIPT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("the binary runs")
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("utf-8 stdout")
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("utf-8 stderr")
}

fn script(json: &str) -> tempfile::NamedTempFile {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(json.as_bytes()).unwrap();
    file
}

#[test]
fn print_streams_prose_to_stdout_and_nothing_else() {
    let out = run(bingo().args(["--print", "--provider", "fake", "hello"]));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Hello from the fake provider.\n");
    assert_eq!(stderr(&out), "");
}

#[test]
fn json_output_is_one_frame_per_line_ending_in_turn_completed() {
    let out = run(bingo().args(["--print", "--output-format", "json", "hello"]));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let frames: Vec<Frame> = stdout(&out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect();
    assert!(frames.len() > 3);
    assert!(
        frames.windows(2).all(|w| w[0].seq < w[1].seq),
        "frames arrive in seq order"
    );
    assert!(frames.iter().all(|f| f.session == frames[0].session));
    assert!(matches!(
        frames.last().map(|f| &f.event),
        Some(Event::TurnCompleted {
            status: TurnStatus::Completed,
            ..
        })
    ));
    assert!(
        frames
            .iter()
            .any(|f| matches!(&f.event, Event::ItemDelta { .. })),
        "the ephemeral deltas are on the wire too"
    );
}

#[test]
fn a_tool_round_reads_the_file_and_reports_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes.txt"), "alpha\nbeta\n").unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"text":"Looking."},{"toolCall":{"name":"Read","input":{"file_path":"notes.txt"}}}]},
            {"steps":[{"text":"Two lines."}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "--cwd"])
        .arg(dir.path())
        .arg("what is in notes.txt?"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Looking.Two lines.\n");
    let err = stderr(&out);
    assert!(
        err.contains("[tool] Read {\"file_path\":\"notes.txt\"}"),
        "{err}"
    );
    assert!(err.contains("[tool] Read ok"), "{err}");
}

#[test]
fn a_failed_turn_is_one_error_line_and_exit_1() {
    let script =
        script(r#"{"responses":[{"steps":[{"error":{"kind":"auth","message":"no key"}}]}]}"#);
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "hello"]));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "");
    let err = stderr(&out);
    assert!(err.starts_with("[error] code=AUTH_REQUIRED msg="), "{err}");
    assert_eq!(err.lines().count(), 1, "{err}");
}

#[test]
fn a_missing_prompt_and_an_unknown_provider_are_errors_before_any_turn() {
    let out = run(bingo().args(["--print"]));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).starts_with("[error] code=INVALID_INPUT msg="),
        "{}",
        stderr(&out)
    );

    let out = run(bingo().args(["--print", "--provider", "nope", "hello"]));
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).starts_with("[error] code=PROVIDER_UNAVAILABLE msg="),
        "{}",
        stderr(&out)
    );
    assert_eq!(stdout(&out), "");
}

#[test]
fn without_print_the_binary_says_what_is_missing() {
    let out = run(bingo().arg("hello"));
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("--print"));
}

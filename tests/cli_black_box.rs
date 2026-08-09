use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "bingo-cli-test-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temporary test directory must be created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn isolated_command(root: &TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bingo"));
    command
        .current_dir(root.path())
        .env("HOME", root.path())
        .env("XDG_CONFIG_HOME", root.path().join("config"))
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn run(root: &TempDir, args: &[&str]) -> Output {
    isolated_command(root)
        .args(args)
        .output()
        .expect("bingo process must start")
}

fn run_with_stdin(root: &TempDir, args: &[&str], stdin: &str) -> Output {
    let mut child = isolated_command(root)
        .args(args)
        .stdin(Stdio::piped())
        .spawn()
        .expect("bingo process must start");
    {
        use std::io::Write;
        let mut pipe = child.stdin.take().expect("stdin pipe");
        pipe.write_all(stdin.as_bytes()).expect("stdin write");
    }
    child.wait_with_output().expect("bingo process must exit")
}

#[test]
fn json_events_eof_cancels_active_turn_without_mixing_stdout() {
    let root = TempDir::new("json-eof-cancel");
    let command = concat!(
        r#"{"protocolVersion":1,"type":"turn.start","commandId":"turn-1-command","turnId":"turn-1","prompt":"hello"}"#,
        "\n"
    );

    let output = run_with_stdin(&root, &["--json-events", "--no-team"], command);

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("NDJSON output must be UTF-8");
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("each stdout line must be JSON"))
        .collect();
    assert_eq!(events[0]["type"], "session.ready");
    assert_eq!(events[1]["type"], "turn.started");
    let last = events.last().expect("terminal event");
    assert_eq!(last["type"], "turn.cancelled");
    assert_eq!(last["reason"], "stdin-eof");
    assert_eq!(last["commandId"], serde_json::Value::Null);
}

#[test]
fn json_events_rejects_duplicate_command_ids_without_closing() {
    let root = TempDir::new("json-duplicate-command");
    let commands = concat!(
        r#"{"protocolVersion":1,"type":"models.list","commandId":"duplicate-1","provider":"missing"}"#,
        "\n",
        r#"{"protocolVersion":1,"type":"models.list","commandId":"duplicate-1","provider":"missing"}"#,
        "\n",
        r#"{"protocolVersion":1,"type":"session.close","commandId":"close-1"}"#,
        "\n"
    );

    let output = run_with_stdin(&root, &["--json-events", "--no-team"], commands);

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("NDJSON output must be UTF-8");
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("each stdout line must be JSON"))
        .collect();
    assert!(events.iter().any(|event| {
        event["type"] == "error"
            && event["commandId"] == "duplicate-1"
            && event["msg"] == "commandId must be unique"
            && event["recoverable"] == true
    }));
    assert_eq!(
        events.last().expect("closed event")["type"],
        "session.closed"
    );
}

#[test]
fn json_events_session_lifecycle_renames_then_deletes() {
    let root = TempDir::new("json-lifecycle");
    let commands = concat!(
        r#"{"protocolVersion":1,"type":"session.rename","commandId":"rename-1","name":"Named Session"}"#,
        "\n",
        r#"{"protocolVersion":1,"type":"session.delete","commandId":"delete-1"}"#,
        "\n"
    );

    let output = run_with_stdin(&root, &["--json-events", "--no-team"], commands);

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("NDJSON output must be UTF-8");
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("each stdout line must be JSON"))
        .collect();
    assert_eq!(events[0]["type"], "session.ready");
    assert_eq!(events[1]["type"], "session.renamed");
    assert_eq!(events[1]["commandId"], "rename-1");
    assert_eq!(events[2]["type"], "session.deleted");
    assert_eq!(events[2]["commandId"], "delete-1");
    let path = events[1]["metadata"]["transcriptPath"]
        .as_str()
        .expect("renamed transcript path");
    assert!(!Path::new(path).exists());
}

#[test]
fn json_events_exact_session_resumes_without_fragment_matching() {
    let root = TempDir::new("json-resume");
    let close = concat!(
        r#"{"protocolVersion":1,"type":"session.close","commandId":"close-1"}"#,
        "\n"
    );
    let created = run_with_stdin(&root, &["--json-events", "--no-team"], close);
    let created_stdout = String::from_utf8(created.stdout).expect("NDJSON output must be UTF-8");
    let ready: serde_json::Value =
        serde_json::from_str(created_stdout.lines().next().expect("ready line"))
            .expect("ready event JSON");
    let session_id = ready["sessionId"].as_str().expect("session id");

    let resumed = run_with_stdin(
        &root,
        &["--json-events", "--no-team", "--session", session_id],
        close,
    );
    assert!(resumed.status.success());
    let resumed_stdout = String::from_utf8(resumed.stdout).expect("NDJSON output must be UTF-8");
    let resumed_ready: serde_json::Value =
        serde_json::from_str(resumed_stdout.lines().next().expect("ready line"))
            .expect("ready event JSON");
    assert_eq!(resumed_ready["metadata"]["resumed"], true);
    assert_eq!(resumed_ready["sessionId"], session_id);

    let fragment = &session_id[..session_id.len() - 1];
    let rejected = run_with_stdin(
        &root,
        &["--json-events", "--no-team", "--session", fragment],
        close,
    );
    assert_eq!(rejected.status.code(), Some(2));
    assert!(rejected.stderr.is_empty());
    let event: serde_json::Value = serde_json::from_slice(&rejected.stdout).expect("fatal JSON");
    assert_eq!(event["type"], "error");
    assert_eq!(event["code"], "BAD_ARGUMENT");
}

#[test]
fn json_events_close_is_ndjson_only_and_gapless() {
    let root = TempDir::new("json-close");
    let command = concat!(
        r#"{"protocolVersion":1,"type":"session.close","commandId":"close-1"}"#,
        "\n"
    );

    let output = run_with_stdin(&root, &["--json-events", "--no-team"], command);

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("NDJSON output must be UTF-8");
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("each stdout line must be JSON"))
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["type"], "session.ready");
    assert_eq!(events[0]["seq"], 1);
    assert_eq!(events[1]["type"], "session.closed");
    assert_eq!(events[1]["seq"], 2);
    assert_eq!(events[1]["commandId"], "close-1");
}

#[test]
fn json_events_malformed_command_is_a_fatal_protocol_error() {
    let root = TempDir::new("json-malformed");

    let output = run_with_stdin(&root, &["--json-events", "--no-team"], "not-json\n");

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("NDJSON output must be UTF-8");
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).expect("each stdout line must be JSON"))
        .collect();
    assert_eq!(events[0]["type"], "session.ready");
    assert_eq!(events[1]["type"], "error");
    assert_eq!(events[1]["code"], "BAD_ARGUMENT");
    assert_eq!(events[1]["recoverable"], false);
    assert!(!stdout.contains("[error]"));
}

#[test]
fn json_events_startup_error_is_one_structured_event() {
    let root = TempDir::new("json-startup-error");
    fs::create_dir_all(root.path().join(".bingo")).expect("project config directory");
    fs::write(root.path().join(".bingo/settings.json"), b"not json")
        .expect("invalid settings fixture");

    let output = run_with_stdin(&root, &["--json-events", "--no-team"], "");

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("NDJSON output must be UTF-8");
    let event: serde_json::Value = serde_json::from_str(stdout.trim()).expect("fatal event JSON");
    assert_eq!(event["type"], "error");
    assert_eq!(event["seq"], 1);
    assert_eq!(event["sessionId"], serde_json::Value::Null);
    assert_eq!(event["code"], "CONFIG_INVALID");
}

#[test]
fn version_is_a_fast_path_even_with_invalid_settings() {
    let root = TempDir::new("version");
    fs::create_dir_all(root.path().join(".bingo")).expect("project config directory");
    fs::write(root.path().join(".bingo/settings.json"), b"not json")
        .expect("invalid settings fixture");

    let output = run(&root, &["--version"]);

    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("version output must be UTF-8"),
        format!("bingo {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn help_is_a_fast_path_even_with_invalid_settings() {
    let root = TempDir::new("help");
    fs::create_dir_all(root.path().join(".bingo")).expect("project config directory");
    fs::write(root.path().join(".bingo/settings.json"), b"not json")
        .expect("invalid settings fixture");

    let output = run(&root, &["--help"]);

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("help output must be UTF-8");
    assert!(stdout.starts_with("Rust agent CLI\n"));
    assert!(stdout.contains("Usage: bingo"));
    assert!(stdout.contains("--inline"));
    assert!(stdout.contains("Inline mode: finalized output stays in the terminal scrollback"));
    assert!(stdout.contains("--fullscreen"));
    assert!(stdout.contains("Fullscreen mode (default)"));
    assert!(output.stderr.is_empty());
}

#[test]
fn conflicting_display_modes_fail_at_the_cli_boundary() {
    let root = TempDir::new("display-mode-conflict");

    let output = run(&root, &["--inline", "--fullscreen"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("clap error output must be UTF-8");
    assert!(stderr.contains("the argument '--inline' cannot be used with '--fullscreen'"));
}

#[test]
fn readme_command_tables_document_explicit_public_sharing() {
    let english = include_str!("../README.md");
    assert!(
        english.contains("| `bingo share [session] [--public] [--open] [-o path]` |")
            && english.contains("`--public` explicitly publishes a link anyone can access"),
        "English command table must expose local-by-default sharing and explicit public opt-in"
    );

    let chinese = include_str!("../README.zh-CN.md");
    assert!(
        chinese.contains("| `bingo share [会话] [--public] [--open] [-o 路径]` |")
            && chinese.contains("`--public` 才显式发布任何人可访问的链接"),
        "Chinese command table must expose local-by-default sharing and explicit public opt-in"
    );
}

#[test]
fn non_tty_errors_use_the_stable_single_line_contract() {
    let root = TempDir::new("error");
    fs::create_dir_all(root.path().join(".bingo")).expect("project config directory");
    fs::write(root.path().join(".bingo/settings.json"), b"not json")
        .expect("invalid settings fixture");

    let output = run(&root, &["--print", "hello"]);

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).expect("error output must be UTF-8");
    assert!(stderr.starts_with("[error] code=CONFIG_INVALID msg="));
    assert_eq!(stderr.lines().count(), 1);
    assert!(!stderr.contains('\u{1b}'));
}

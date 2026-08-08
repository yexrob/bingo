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

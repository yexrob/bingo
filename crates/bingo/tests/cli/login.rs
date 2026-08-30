//! `bingo login|logout <provider>` (ADR-0012 §5): the kernel's two credential
//! commands without a session. The receipt is the one line on stdout; every
//! diagnostic is on stderr and a failure is one `[error]` line.

use super::*;

#[test]
fn a_provider_without_a_sign_in_says_so() {
    let home = tempfile::tempdir().unwrap();
    let out = run(bingo()
        .env("HOME", home.path())
        .args(["login", "fake", "--cwd"])
        .arg(home.path()));
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "");
    let err = stderr(&out);
    assert!(
        err.starts_with("[error] code=INVALID_INPUT") && err.contains("login"),
        "{err}"
    );
}

#[test]
fn an_unknown_provider_is_refused_by_name() {
    let home = tempfile::tempdir().unwrap();
    let out = run(bingo()
        .env("HOME", home.path())
        .args(["logout", "nope", "--cwd"])
        .arg(home.path()));
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.starts_with("[error] code=PROVIDER_UNAVAILABLE") && err.contains("`nope`"),
        "{err}"
    );
}

#[test]
fn device_and_paste_exclude_each_other() {
    let out = run(bingo().args(["login", "codex", "--device", "--paste"]));
    assert_ne!(out.status.code(), Some(0));
    assert!(stderr(&out).contains("--paste"), "{}", stderr(&out));
}

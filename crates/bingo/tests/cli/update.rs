//! `bingo update` (M63, ADR-0043), against a release served on the loopback.
//!
//! What is asserted is the file on disk afterwards, not a claim about it: the
//! whole thing — the answer, the list, the archive, the checksum, the unpack
//! and the two renames — runs against a copy of this binary, and the copy is
//! then asked what version it is.

use bingo_update::{asset, sha256};
use wiremock::matchers::{method, path as at};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

/// The version the fake release is out at, above anything this build is.
const NEWER: &str = "9.9.9";

/// The fake release: `releases/latest` answers with an archive and a list,
/// both served beside it under `/dl`.
async fn released(name: &str, archive: Vec<u8>, list: String) -> MockServer {
    let server = MockServer::start().await;
    let url = |file: &str| format!("{}/dl/{file}", server.uri());
    let answer = serde_json::json!({
        "tag_name": format!("v{NEWER}"),
        "prerelease": false,
        "assets": [
            { "name": name, "browser_download_url": url(name) },
            { "name": asset::CHECKSUMS, "browser_download_url": url(asset::CHECKSUMS) },
        ],
    });
    let answering = |path: String, body: ResponseTemplate| {
        Mock::given(method("GET")).and(at(path)).respond_with(body)
    };
    answering(
        "/repos/yexrob/bingo/releases/latest".into(),
        ResponseTemplate::new(200).set_body_string(answer.to_string()),
    )
    .mount(&server)
    .await;
    answering(
        format!("/dl/{name}"),
        ResponseTemplate::new(200).set_body_bytes(archive),
    )
    .mount(&server)
    .await;
    answering(
        format!("/dl/{}", asset::CHECKSUMS),
        ResponseTemplate::new(200).set_body_string(list),
    )
    .mount(&server)
    .await;
    server
}

/// `bingo update`, in its own home, against the release `server` serves.
fn updating(binary: &std::path::Path, home: &std::path::Path, server: &MockServer) -> Command {
    let mut cmd = Command::new(binary);
    cmd.env("HOME", home)
        .env("BINGO_UPDATE_API", server.uri())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .arg("update");
    cmd
}

/// A copy of this binary, which is what an update is allowed to replace.
fn copied(dir: &std::path::Path) -> std::path::PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let copy = bin.join(asset::binary());
    std::fs::copy(env!("CARGO_BIN_EXE_bingo"), &copy).unwrap();
    copy
}

/// The two lines `--check` is allowed to print.
fn reported(latest: &str) -> String {
    format!(
        "current: {}\nlatest:  {latest}\n",
        env!("CARGO_PKG_VERSION")
    )
}

#[tokio::test]
async fn check_reports_the_two_versions_and_nothing_else() {
    let server = released("bingo.tar.gz", Vec::new(), String::new()).await;
    let home = tempfile::tempdir().unwrap();
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_bingo"));
    let out = run(updating(&binary, home.path(), &server).arg("--check"));

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), reported(NEWER));
    assert_eq!(stderr(&out), "");
}

#[tokio::test]
async fn an_api_that_answers_nothing_is_exit_one_with_nothing_on_stdout() {
    let server = MockServer::start().await;
    let home = tempfile::tempdir().unwrap();
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_bingo"));
    let out = run(updating(&binary, home.path(), &server).arg("--check"));

    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "", "nothing is said before the answer is in");
    assert!(stderr(&out).contains("[error]"), "{}", stderr(&out));
}

#[tokio::test]
async fn an_archive_that_is_not_what_the_list_says_is_refused() {
    let Some(target) = asset::target() else {
        return; // a build the release line does not publish updates itself
    };
    let name = asset::name(target);
    let wrong = format!("{}  {name}\n", "0".repeat(64));
    let server = released(&name, b"not the archive".to_vec(), wrong).await;
    let dir = tempfile::tempdir().unwrap();
    let copy = copied(dir.path());

    let out = run(&mut updating(&copy, dir.path(), &server));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        stdout(&out),
        reported(NEWER),
        "the versions, and no receipt"
    );
    assert!(
        stderr(&out).contains("not what checksums.txt says"),
        "{}",
        stderr(&out)
    );
    assert!(!copy.with_extension("old").exists(), "nothing was moved");
}

/// The whole round trip. The archive holds a script rather than a build of a
/// version that does not exist, which is what lets the replaced copy be asked
/// what it is; the zip half of this is Windows' and is unpacked by `tar.exe`
/// there, which no unix machine can stand in for.
#[cfg(unix)]
#[tokio::test]
async fn the_binary_becomes_the_release_it_was_told_about() {
    let Some(target) = asset::target() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let name = asset::name(target);
    let (archive, list) = packed(dir.path(), &name);
    let server = released(&name, archive, list).await;
    let copy = copied(dir.path());

    let out = run(&mut updating(&copy, dir.path(), &server));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!("{}updated to {NEWER}\n", reported(NEWER))
    );

    let after = run(Command::new(&copy)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped()));
    assert_eq!(stdout(&after).trim(), format!("bingo {NEWER}"));
    assert!(
        !copy.with_extension("old").exists(),
        "and nothing beside it"
    );
    assert!(!copy.with_extension("new").exists());
}

/// A `tar.gz` holding one runnable `bingo` that says which version it is,
/// and the `checksums.txt` line for the archive itself.
#[cfg(unix)]
fn packed(dir: &std::path::Path, name: &str) -> (Vec<u8>, String) {
    use std::os::unix::fs::PermissionsExt;
    let inside = dir.join("packed");
    std::fs::create_dir_all(&inside).unwrap();
    let binary = inside.join("bingo");
    std::fs::write(&binary, format!("#!/bin/sh\necho 'bingo {NEWER}'\n")).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    let archive = dir.join(name);
    let packed = Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&inside)
        .arg("bingo")
        .output()
        .unwrap();
    assert!(packed.status.success(), "{packed:?}");
    let bytes = std::fs::read(&archive).unwrap();
    let list = format!("{}  {name}\n", sha256::hex(&sha256::digest(&bytes)));
    (bytes, list)
}

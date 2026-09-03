//! A picture beside a headless prompt (ADR-0040): `--image` on the command
//! line — a path or a URL this machine fetches (ADR-0041 §3) — and an `image`
//! block on a stream-json user line. What is asserted is the journal — the one
//! place the picture is kept — not the stream, which does not echo a person's
//! own prompt.

use base64::Engine;
use bingo_pictures::testing::{ImageFormat, drawn, png_bytes};
use serde_json::{Value, json};
use wiremock::matchers::{method, path as at};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::stream_json::{Host, hosted};

/// A picture on disk, in the format `name`'s extension says.
fn shot_of(dir: &std::path::Path, name: &str, bytes: Vec<u8>) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// The picture most of these tests hand over.
fn shot(dir: &std::path::Path) -> std::path::PathBuf {
    shot_of(dir, "shot.png", png_bytes(3, 2))
}

fn base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A picture served on the loopback, and the URL that reaches it.
async fn serving(bytes: Vec<u8>) -> (MockServer, String) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(at("/shot"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
        .mount(&server)
        .await;
    let url = format!("{}/shot", server.uri());
    (server, url)
}

/// `--print --image <source> <prompt>`, in its own home.
fn asked_with(dir: &std::path::Path, source: &str) -> Output {
    let script = script(r#"{"responses":[{"steps":[{"text":"A picture."}]}]}"#);
    run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", dir)
        .args(["--print", "--cwd"])
        .arg(dir)
        .args(["--image", source])
        .arg("what is this?"))
}

/// Every journal line under the run's home: one session, one prompt.
fn journal(home: &std::path::Path) -> Vec<Value> {
    let sessions = home.join(".bingo/data/sessions");
    let dir = std::fs::read_dir(&sessions)
        .expect("the run wrote a session")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.join("journal.jsonl").exists())
        .expect("one session");
    std::fs::read_to_string(dir.join("journal.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The parts of the first user item the journal holds.
fn first_ask(home: &std::path::Path) -> Vec<Value> {
    journal(home)
        .into_iter()
        .filter_map(|line| {
            let item = line.pointer("/event/item")?;
            (item["body"]["kind"] == "user").then(|| item["body"]["parts"].clone())
        })
        .next()
        .and_then(|parts| parts.as_array().cloned())
        .expect("a user item in the journal")
}

#[test]
fn an_image_flag_puts_the_picture_beside_the_prompt_in_the_journal() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = png_bytes(3, 2);
    let path = shot_of(dir.path(), "shot.png", bytes.clone());
    let out = asked_with(dir.path(), &path.to_string_lossy());
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "A picture.");
    let parts = first_ask(dir.path());
    assert_eq!(parts[0]["type"], "text");
    assert_eq!(parts[0]["text"], "what is this?");
    assert_eq!(parts[1]["type"], "image");
    assert_eq!(parts[1]["mediaType"], "image/png");
    assert_eq!(parts[1]["data"], base64(&bytes), "the file's own bytes");
}

/// A format no provider takes is one a person still has: it is decoded on
/// the way in and the journal holds a PNG (ADR-0041 §2).
#[test]
fn an_image_of_a_wider_format_reaches_the_journal_as_png() {
    let dir = tempfile::tempdir().unwrap();
    let path = shot_of(dir.path(), "shot.bmp", drawn(4, 5, ImageFormat::Bmp));
    let out = asked_with(dir.path(), &path.to_string_lossy());
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let parts = first_ask(dir.path());
    assert_eq!(parts[1]["mediaType"], "image/png");
}

/// The picture is fetched by this machine and journaled as bytes: a session
/// replays without the URL ever being reachable again (ADR-0041 §3).
#[tokio::test]
async fn an_image_url_is_fetched_and_journaled() {
    let bytes = drawn(6, 6, ImageFormat::Jpeg);
    let (_server, url) = serving(bytes.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let out = asked_with(dir.path(), &url);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "A picture.");
    let parts = first_ask(dir.path());
    assert_eq!(parts[1]["type"], "image");
    assert_eq!(parts[1]["mediaType"], "image/jpeg");
    assert_eq!(parts[1]["data"], base64(&bytes));
}

#[tokio::test]
async fn an_image_url_that_is_not_there_is_exit_1_with_nothing_on_stdout() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("{}/gone.png", server.uri());
    let out = asked_with(dir.path(), &url);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).is_empty(), "stdout: {}", stdout(&out));
    assert!(
        stderr(&out).contains(&url),
        "stderr names it: {}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("404"), "{}", stderr(&out));
}

/// A picture over the journal's cap is refused at the edge, before a session
/// is opened for it — and refused off the header, not after 5 MB is held.
#[tokio::test]
async fn an_image_url_over_the_cap_is_exit_1_with_nothing_on_stdout() {
    let (_server, url) = serving(vec![0u8; bingo_sdk::Image::MAX_BYTES + 1]).await;
    let dir = tempfile::tempdir().unwrap();
    let out = asked_with(dir.path(), &url);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).is_empty(), "stdout: {}", stdout(&out));
    assert!(stderr(&out).contains("too large"), "{}", stderr(&out));
}

/// A URL that answers with something that is not a picture at all.
#[tokio::test]
async fn a_url_that_is_not_a_picture_is_exit_1_and_says_so() {
    let (_server, url) = serving(b"<!doctype html><html>a page</html>".to_vec()).await;
    let dir = tempfile::tempdir().unwrap();
    let out = asked_with(dir.path(), &url);
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).is_empty(), "stdout: {}", stdout(&out));
    assert!(stderr(&out).contains("not a picture"), "{}", stderr(&out));
}

#[test]
fn an_image_that_does_not_read_is_exit_1_with_nothing_on_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.png");
    let out = asked_with(dir.path(), &missing.to_string_lossy());
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).is_empty(), "stdout: {}", stdout(&out));
    assert!(
        stderr(&out).contains("missing.png"),
        "stderr names the path: {}",
        stderr(&out)
    );
}

#[test]
fn an_image_flag_under_stream_json_input_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = shot(dir.path());
    let out = run(bingo()
        .env("HOME", dir.path())
        .args(["--print", "--input-format", "stream-json", "--cwd"])
        .arg(dir.path())
        .arg("--image")
        .arg(&path));
    assert_eq!(out.status.code(), Some(1));
    assert!(stdout(&out).is_empty());
    assert!(stderr(&out).contains("--image"), "{}", stderr(&out));
}

#[test]
fn a_stream_json_user_line_carries_its_image_blocks() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(r#"{"responses":[{"steps":[{"text":"Seen."}]}]}"#);
    let mut host = Host::start(&mut hosted(dir.path(), &script, &[]));
    host.send(&json!({
        "type": "user",
        "message": { "role": "user", "content": [
            { "type": "text", "text": "and this one" },
            { "type": "image", "source": {
                "type": "base64", "media_type": "image/jpeg", "data": "/9j/" } }
        ] },
        "parent_tool_use_id": Value::Null,
    }));
    let ended = host.finish();
    assert_eq!(ended.code, Some(0), "stderr: {}", ended.err);
    assert_eq!(ended.results()[0]["result"], "Seen.");
    let parts = first_ask(dir.path());
    assert_eq!(parts[0]["text"], "and this one");
    assert_eq!(parts[1]["type"], "image");
    assert_eq!(parts[1]["mediaType"], "image/jpeg");
}

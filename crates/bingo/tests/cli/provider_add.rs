//! `bingo provider add` (ADR-0017 §6): the four questions a person answers
//! to gain an endpoint, and the two files the answers land in — the instance
//! in the user settings layer, the key in `auth.json` and nowhere else.

use super::*;

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn settings_file(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".bingo/settings.json")
}

fn auth_json(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".bingo/data/auth.json")
}

fn add(home: &std::path::Path) -> Command {
    let mut cmd = bingo();
    cmd.env("HOME", home)
        .args(["provider", "add", "--cwd"])
        .arg(home);
    cmd
}

fn read(path: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The whole command: four answers, two files, and a run that uses what they
/// wrote — with the key nowhere near the settings.
#[tokio::test]
async fn an_added_provider_is_written_down_and_the_next_run_uses_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", "Bearer sk-added"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(responses_fixture("text.sse"), "text/event-stream"),
        )
        .mount(&server)
        .await;
    let home = tempfile::tempdir().unwrap();

    let (h, uri) = (home.path().to_path_buf(), server.uri());
    let out = tokio::task::spawn_blocking(move || {
        typed(&mut add(&h), &["proxy1", "openai", &uri, "sk-added"])
    })
    .await
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!(
            "proxy1 is openai.instances.proxy1 in {}.\n\
             Its key is in {}, never in the settings.\n\
             bingo --provider proxy1\n",
            settings_file(home.path()).display(),
            auth_json(home.path()).display()
        )
    );
    let err = stderr(&out);
    assert!(
        err.contains("wire protocol") && err.contains("[openai/anthropic]"),
        "the protocol is asked for as a protocol: {err}"
    );
    assert!(
        !err.contains("sk-added"),
        "the key this process was handed is not printed back: {err}"
    );

    let written = std::fs::read_to_string(settings_file(home.path())).unwrap();
    assert!(
        !written.contains("sk-added"),
        "a settings file is committable; a key is not: {written}"
    );
    assert_eq!(
        read(&settings_file(home.path()))["openai"]["instances"]["proxy1"],
        serde_json::json!({ "baseUrl": server.uri() })
    );
    assert_eq!(read(&auth_json(home.path()))["proxy1"]["key"], "sk-added");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(auth_json(home.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "auth.json is the person's alone: {mode:o}");
    }

    // No `--settings`: what was written is the user layer the next run reads.
    let h = home.path().to_path_buf();
    let out = tokio::task::spawn_blocking(move || {
        run(bingo()
            .env("HOME", &h)
            .env_remove("OPENAI_API_KEY")
            .args([
                "--print",
                "--provider",
                "proxy1",
                "--model",
                "gpt-5.4",
                "--cwd",
            ])
            .arg(&h)
            .arg("hi"))
    })
    .await
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Hello, world.\n");
}

/// An endpoint of the other shape, with no key and no base url of its own:
/// the two optional answers stay optional.
#[test]
fn an_anthropic_instance_needs_neither_an_endpoint_nor_a_key() {
    let home = tempfile::tempdir().unwrap();
    let out = typed(
        &mut add(home.path()),
        &["claude-proxy", "anthropic", "", ""],
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        stdout(&out),
        format!(
            "claude-proxy is anthropic.instances.claude-proxy in {}.\n\
             bingo --provider claude-proxy\n",
            settings_file(home.path()).display()
        )
    );
    assert_eq!(
        read(&settings_file(home.path()))["anthropic"]["instances"]["claude-proxy"],
        serde_json::json!({})
    );
    assert!(
        !auth_json(home.path()).exists(),
        "no key was given, so no credential was written"
    );
}

/// The settings file is a person's, and this command will not rewrite one it
/// cannot read whole: JSONC is read at startup, and a round trip would drop
/// the comments in it.
#[test]
fn a_settings_file_that_is_not_plain_json_is_left_byte_for_byte() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".bingo")).unwrap();
    let before = "// mine\n{ \"openai\": { \"apiKey\": \"sk-mine\" } }\n";
    std::fs::write(settings_file(home.path()), before).unwrap();

    let out = typed(
        &mut add(home.path()),
        &["proxy1", "openai", "http://127.0.0.1:8080", ""],
    );
    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout(&out));
    assert_eq!(stdout(&out), "");
    let err = stderr(&out);
    assert!(
        err.starts_with("[error] code=INVALID_INPUT")
            && err.contains("not plain JSON")
            && err.contains(&settings_file(home.path()).display().to_string()),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(settings_file(home.path())).unwrap(),
        before,
        "the file is left exactly as it was"
    );
}

/// A name is an identity, and the refusal comes before anything is written.
#[test]
fn a_name_already_taken_is_refused_and_nothing_is_written() {
    let home = tempfile::tempdir().unwrap();
    let out = typed(&mut add(home.path()), &["codex", "openai", "", ""]);
    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout(&out));
    assert!(stderr(&out).contains("`codex`"), "{}", stderr(&out));
    assert!(!settings_file(home.path()).exists());

    let out = typed(&mut add(home.path()), &["two words", "openai", "", ""]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("one word"), "{}", stderr(&out));

    // The same name twice: the second run reads what the first wrote.
    let out = typed(&mut add(home.path()), &["proxy1", "openai", "", ""]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let out = typed(&mut add(home.path()), &["proxy1", "openai", "", ""]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("`proxy1`"), "{}", stderr(&out));
}

/// The protocol is a choice between two, and an answer that is neither stops
/// the command rather than guessing.
#[test]
fn a_protocol_that_is_neither_is_refused_by_name() {
    let home = tempfile::tempdir().unwrap();
    let out = typed(&mut add(home.path()), &["proxy1", "claude", "", ""]);
    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout(&out));
    let err = stderr(&out);
    assert!(
        err.contains("`claude`") && err.contains("openai") && err.contains("anthropic"),
        "{err}"
    );
    assert!(!settings_file(home.path()).exists());
}

//! Named provider instances (ADR-0017): a settings key names more endpoints
//! than one, each addressable as `--provider <name>` and `/model
//! <name>/<model>`, each with its own credential in `auth.json`.

use super::*;

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn settings(home: &std::path::Path, value: serde_json::Value) -> std::path::PathBuf {
    let file = home.join("instances.json");
    std::fs::write(&file, value.to_string()).unwrap();
    file
}

fn auth_json(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".bingo/data/auth.json")
}

fn stored(home: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(auth_json(home)).unwrap()).unwrap()
}

fn write_auth(home: &std::path::Path, value: serde_json::Value) {
    std::fs::create_dir_all(auth_json(home).parent().unwrap()).unwrap();
    std::fs::write(auth_json(home), value.to_string()).unwrap();
}

/// The binary against one home and one extra settings layer. `OPENAI_API_KEY`
/// is left as the test found it: a named instance must not read it either way.
fn bingo_with(home: &std::path::Path, file: &std::path::Path) -> Command {
    let mut cmd = bingo();
    cmd.env("HOME", home)
        .env("BINGO_NO_BROWSER", "1")
        .arg("--settings")
        .arg(file)
        .arg("--cwd")
        .arg(home);
    cmd
}

fn turn(home: &std::path::Path, file: &std::path::Path, provider: &str) -> Command {
    let mut cmd = bingo_with(home, file);
    cmd.args([
        "--print",
        "--provider",
        provider,
        "--model",
        "gpt-5.4",
        "hi",
    ]);
    cmd
}

/// One OpenAI-shaped endpoint that answers a turn for the key it expects.
async fn endpoint_for(key: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", format!("Bearer {key}").as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(responses_fixture("text.sse"), "text/event-stream"),
        )
        .mount(&server)
        .await;
    server
}

/// A subscription: an issuer that renews to `access`, and an endpoint that
/// takes that bearer and no other.
async fn subscription(access: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": access, "expires_in": 3600
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", format!("Bearer {access}").as_str()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(responses_fixture("text.sse"), "text/event-stream"),
        )
        .mount(&server)
        .await;
    server
}

async fn calls(server: &MockServer, path: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == path)
        .count()
}

/// Two subscriptions side by side: two entries in one `auth.json`, each
/// refreshed against its own issuer, neither reaching for the other's token.
#[tokio::test]
async fn two_codex_instances_hold_two_credentials_and_refresh_apart() {
    let work = subscription("at-work-fresh").await;
    let personal = subscription("at-personal-fresh").await;
    let home = tempfile::tempdir().unwrap();
    let file = settings(
        home.path(),
        serde_json::json!({ "codex": { "instances": {
            "work": { "baseUrl": work.uri(), "issuer": work.uri() },
            "personal": { "baseUrl": personal.uri(), "issuer": personal.uri() },
        }}}),
    );
    write_auth(
        home.path(),
        serde_json::json!({
            "work": { "type": "oauth", "access": "at-work-stale", "refresh": "rt-work", "expires": 1 },
            "personal": { "type": "oauth", "access": "at-personal-stale", "refresh": "rt-personal", "expires": 1 },
        }),
    );

    for name in ["work", "personal"] {
        let (h, f) = (home.path().to_path_buf(), file.clone());
        let out = tokio::task::spawn_blocking(move || {
            run(turn(&h, &f, name).env_remove("OPENAI_API_KEY"))
        })
        .await
        .unwrap();
        assert_eq!(out.status.code(), Some(0), "{name}: {}", stderr(&out));
        assert_eq!(stdout(&out), "Hello, world.\n");
    }

    let stored = stored(home.path());
    assert_eq!(stored["work"]["access"], "at-work-fresh");
    assert_eq!(stored["personal"]["access"], "at-personal-fresh");
    assert_eq!(
        stored["work"]["refresh"], "rt-work",
        "each entry keeps its own refresh token"
    );
    assert_eq!(stored["personal"]["refresh"], "rt-personal");
    assert_eq!(calls(&work, "/oauth/token").await, 1);
    assert_eq!(calls(&personal, "/oauth/token").await, 1);
    assert_eq!(calls(&work, "/codex/responses").await, 1);
    assert_eq!(calls(&personal, "/codex/responses").await, 1);
}

/// The whole life of an instance's key: pasted in, sent on the wire, taken
/// out again, and the turn that follows says how to put it back.
#[tokio::test]
async fn a_pasted_key_reaches_the_wire_and_a_logout_takes_it_away() {
    let server = endpoint_for("sk-proxy1").await;
    let home = tempfile::tempdir().unwrap();
    let file = settings(
        home.path(),
        serde_json::json!({ "openai": { "instances": { "proxy1": { "baseUrl": server.uri() } } } }),
    );

    let (h, f) = (home.path().to_path_buf(), file.clone());
    let out = tokio::task::spawn_blocking(move || {
        typed(
            bingo_with(&h, &f).args(["login", "proxy1", "--paste"]),
            &["sk-proxy1"],
        )
    })
    .await
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Signed in to proxy1 with a pasted key.\n");
    assert_eq!(stored(home.path())["proxy1"]["type"], "api");
    assert_eq!(stored(home.path())["proxy1"]["key"], "sk-proxy1");
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

    let (h, f) = (home.path().to_path_buf(), file.clone());
    let out = tokio::task::spawn_blocking(move || run(&mut turn(&h, &f, "proxy1")))
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Hello, world.\n");
    assert_eq!(
        calls(&server, "/v1/responses").await,
        1,
        "the endpoint took the pasted key, which is what it matched on"
    );

    let (h, f) = (home.path().to_path_buf(), file.clone());
    let out =
        tokio::task::spawn_blocking(move || run(bingo_with(&h, &f).args(["logout", "proxy1"])))
            .await
            .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Signed out of proxy1.\n");
    assert!(stored(home.path()).get("proxy1").is_none());

    let (h, f) = (home.path().to_path_buf(), file);
    let out = tokio::task::spawn_blocking(move || run(&mut turn(&h, &f, "proxy1")))
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let err = stderr(&out);
    assert!(
        err.starts_with("[error] code=AUTH_REQUIRED") && err.contains("/login proxy1"),
        "{err}"
    );
}

/// An exported key belongs to the default instance and to nothing else: a
/// proxy with a key of its own must never be reached with someone else's.
#[tokio::test]
async fn an_exported_key_does_not_feed_a_named_instance() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    let home = tempfile::tempdir().unwrap();
    let file = settings(
        home.path(),
        serde_json::json!({ "openai": { "instances": { "proxy1": { "baseUrl": server.uri() } } } }),
    );

    let (h, f) = (home.path().to_path_buf(), file);
    let out = tokio::task::spawn_blocking(move || {
        run(turn(&h, &f, "proxy1").env("OPENAI_API_KEY", "sk-ambient"))
    })
    .await
    .unwrap();
    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout(&out));
    let err = stderr(&out);
    assert!(
        err.starts_with("[error] code=AUTH_REQUIRED") && err.contains("/login proxy1"),
        "{err}"
    );
    assert!(
        !err.contains("OPENAI_API_KEY"),
        "the variable is not one of this instance's places: {err}"
    );
    assert_eq!(
        calls(&server, "/v1/responses").await,
        0,
        "no turn was spent on a key that is not this instance's"
    );
}

/// `--provider <name>` is one way to reach an instance; `/model
/// <name>/<model>` is the other, and the receipt names the pair.
#[tokio::test]
async fn an_instance_is_reachable_by_provider_and_by_model() {
    let server = endpoint_for("sk-one").await;
    let home = tempfile::tempdir().unwrap();
    let file = settings(
        home.path(),
        serde_json::json!({ "openai": { "instances": {
            "proxy1": { "baseUrl": server.uri(), "apiKey": "sk-one" }
        }}}),
    );

    let (h, f) = (home.path().to_path_buf(), file.clone());
    let out = tokio::task::spawn_blocking(move || {
        let script = script(r#"{"responses":[]}"#);
        run(bingo_with(&h, &f)
            .env("BINGO_FAKE_SCRIPT", script.path())
            .args(["--print", "--provider", "fake"])
            .arg("/model proxy1/gpt-5.4"))
    })
    .await
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "model: proxy1/gpt-5.4\n");

    let (h, f) = (home.path().to_path_buf(), file);
    let out = tokio::task::spawn_blocking(move || run(&mut turn(&h, &f, "proxy1")))
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Hello, world.\n");
    let request = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.url.path() == "/v1/responses")
        .expect("the turn reached the instance's endpoint");
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["model"], "gpt-5.4");
}

/// A name is an identity: taking one that is already a provider's stops the
/// boot, whether the collision is inside one plugin's settings or across two.
#[test]
fn a_colliding_instance_name_stops_the_boot_and_is_named() {
    let home = tempfile::tempdir().unwrap();
    let refused = |value: serde_json::Value| {
        let file = settings(home.path(), value);
        let out = run(bingo_with(home.path(), &file).args(["--print", "--provider", "fake", "hi"]));
        assert_ne!(out.status.code(), Some(0), "stdout: {}", stdout(&out));
        stderr(&out)
    };
    let err = refused(serde_json::json!({ "openai": { "instances": { "codex": {} } } }));
    assert!(
        err.contains("`codex`") && err.starts_with("[error]"),
        "{err}"
    );

    let err = refused(serde_json::json!({
        "openai": { "instances": { "proxy1": {}, "shared": {} } },
        "anthropic": { "instances": { "shared": {} } },
    }));
    assert!(
        err.contains("shared") && err.starts_with("[error]"),
        "two plugins cannot both answer to one name: {err}"
    );
}

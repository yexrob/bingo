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

// --- codex: a fake issuer and a fake subscription endpoint, one wiremock ---

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A signature-less JWT: the provider reads claims, it never verifies.
fn jwt(claims: serde_json::Value) -> String {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    format!(
        "{}.{}.sig",
        URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#),
        URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}

fn access_token(tag: &str) -> String {
    jwt(serde_json::json!({
        "https://api.openai.com/auth": { "chatgpt_account_id": "acc_42" },
        "tag": tag,
    }))
}

fn id_token() -> String {
    jwt(serde_json::json!({ "email": "me@example.com" }))
}

/// The issuer: a device code, one pending poll, then a grant; the token
/// endpoint answers every exchange and refresh with `access`; revoke says ok.
async fn issuer(server: &MockServer, access: &str) {
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/usercode"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "device_auth_id": "dev_1", "user_code": "ABCD-EFGH", "interval": 1
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(403))
        .up_to_n_times(1)
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/accounts/deviceauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "authorization_code": "code_1", "code_verifier": "v_1", "code_challenge": "c_1"
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": access, "refresh_token": "r_1", "id_token": id_token(), "expires_in": 3600
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/oauth/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .mount(server)
        .await;
}

/// The subscription endpoint answering one turn for the bearer it expects.
async fn codex_endpoint(server: &MockServer, bearer: &str, status: u16, fixture: Option<&str>) {
    let mut response = ResponseTemplate::new(status);
    if let Some(name) = fixture {
        response = response.set_body_raw(responses_fixture(name), "text/event-stream");
    }
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", format!("Bearer {bearer}").as_str()))
        .respond_with(response)
        .up_to_n_times(1)
        .mount(server)
        .await;
}

/// `--settings` pointing both the issuer and the endpoint at the mock.
fn settings_for(home: &std::path::Path, server: &MockServer) -> std::path::PathBuf {
    let file = home.join("codex-settings.json");
    std::fs::write(
        &file,
        serde_json::json!({ "codex": { "baseUrl": server.uri(), "issuer": server.uri() } })
            .to_string(),
    )
    .unwrap();
    file
}

fn auth_json(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".bingo/data/auth.json")
}

fn codex(home: &std::path::Path, settings: &std::path::Path) -> Command {
    let mut cmd = bingo();
    cmd.env("HOME", home)
        .env("BINGO_NO_BROWSER", "1")
        .arg("--settings")
        .arg(settings)
        .arg("--cwd")
        .arg(home);
    cmd
}

#[tokio::test]
async fn a_device_sign_in_stores_the_token_and_the_next_turn_uses_it() {
    let server = MockServer::start().await;
    let access = access_token("first");
    issuer(&server, &access).await;
    codex_endpoint(&server, &access, 200, Some("text.sse")).await;
    let home = tempfile::tempdir().unwrap();
    let settings = settings_for(home.path(), &server);

    let (h, s) = (home.path().to_path_buf(), settings.clone());
    let out = tokio::task::spawn_blocking(move || {
        run_within(
            codex(&h, &s).args(["login", "codex", "--device"]),
            Duration::from_secs(30),
        )
    })
    .await
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Signed in to codex as me@example.com.\n");
    let err = stderr(&out);
    assert!(
        err.contains("ABCD-EFGH") && err.contains("/codex/device"),
        "the code and where to enter it are on stderr: {err}"
    );
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(auth_json(home.path())).unwrap()).unwrap();
    assert_eq!(stored["codex"]["type"], "oauth");
    assert_eq!(stored["codex"]["access"], access);
    assert_eq!(stored["codex"]["refresh"], "r_1");
    assert_eq!(stored["codex"]["accountId"], "acc_42");
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

    let (h, s) = (home.path().to_path_buf(), settings.clone());
    let out = tokio::task::spawn_blocking(move || {
        run(codex(&h, &s).args(["--print", "--provider", "codex", "--model", "gpt-5.4", "hi"]))
    })
    .await
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Hello, world.\n");
    let request = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| r.url.path() == "/codex/responses")
        .expect("the turn reached the subscription endpoint");
    assert_eq!(
        request
            .headers
            .get("chatgpt-account-id")
            .map(|v| v.to_str().unwrap()),
        Some("acc_42")
    );
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["store"], false);
    assert!(
        body.get("max_output_tokens").is_none(),
        "the subscription endpoint refuses a budget: {body}"
    );

    let (h, s) = (home.path().to_path_buf(), settings);
    let out = tokio::task::spawn_blocking(move || run(codex(&h, &s).args(["logout", "codex"])))
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Signed out of codex.\n");
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(auth_json(home.path())).unwrap()).unwrap();
    assert!(stored.get("codex").is_none(), "{stored}");
    assert!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|r| r.url.path() == "/oauth/revoke"),
        "signing out revokes the token"
    );
}

#[tokio::test]
async fn before_any_sign_in_a_codex_turn_is_refused_by_name() {
    let server = MockServer::start().await;
    let home = tempfile::tempdir().unwrap();
    let settings = settings_for(home.path(), &server);
    let out = run(codex(home.path(), &settings).args(["--print", "--provider", "codex", "hi"]));
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "");
    let err = stderr(&out);
    assert!(
        err.starts_with("[error] code=AUTH_REQUIRED") && err.contains("bingo login codex"),
        "{err}"
    );
}

/// A stored token the endpoint no longer takes: one refresh, one retry, and
/// the turn completes on the new token.
#[tokio::test]
async fn a_401_is_followed_by_one_refresh_and_the_turn_completes() {
    let server = MockServer::start().await;
    let (stale, fresh) = (access_token("stale"), access_token("fresh"));
    issuer(&server, &fresh).await;
    codex_endpoint(&server, &stale, 401, None).await;
    codex_endpoint(&server, &fresh, 200, Some("text.sse")).await;
    let home = tempfile::tempdir().unwrap();
    let settings = settings_for(home.path(), &server);
    std::fs::create_dir_all(auth_json(home.path()).parent().unwrap()).unwrap();
    std::fs::write(
        auth_json(home.path()),
        serde_json::json!({ "codex": {
            "type": "oauth", "access": stale, "refresh": "r_0",
            "expires": 4_102_444_800i64, "accountId": "acc_42"
        }})
        .to_string(),
    )
    .unwrap();

    let (h, s) = (home.path().to_path_buf(), settings);
    let out = tokio::task::spawn_blocking(move || {
        run(codex(&h, &s).args(["--print", "--provider", "codex", "--model", "gpt-5.4", "hi"]))
    })
    .await
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Hello, world.\n");
    let refreshes = server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/oauth/token")
        .count();
    assert_eq!(refreshes, 1);
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(auth_json(home.path())).unwrap()).unwrap();
    assert_eq!(
        stored["codex"]["access"], fresh,
        "the refreshed token is kept"
    );
}

//! `bingo mcp list|get|add|remove|login|logout` (ADR-0050 §4): the servers
//! this machine dials, from a terminal. The answer is the one thing on
//! stdout; every diagnostic is on stderr and a failure is one `[error]` line.

use super::*;

fn user_settings(home: &std::path::Path) -> std::path::PathBuf {
    home.join(".bingo/settings.json")
}

fn settings_json(home: &std::path::Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(user_settings(home)).expect("the user settings");
    serde_json::from_str(&raw).expect("json")
}

fn mcp(home: &std::path::Path) -> Command {
    let mut cmd = bingo();
    cmd.envs(home_env(home)).arg("--cwd").arg(home).arg("mcp");
    cmd
}

/// A server written, read back, listed and taken away again.
#[test]
fn a_server_is_added_read_back_listed_and_removed() {
    let home = tempfile::tempdir().unwrap();

    let added = run(mcp(home.path()).args([
        "add",
        "--transport",
        "http",
        "-H",
        "X-Tenant: acme",
        "remote",
        "https://mcp.example.com/mcp",
    ]));
    assert_eq!(added.status.code(), Some(0), "stderr: {}", stderr(&added));
    assert!(stdout(&added).starts_with("Added the mcp server `remote`"));
    assert_eq!(stderr(&added), "");

    // The user layer, and only the user layer.
    assert_eq!(
        settings_json(home.path())["mcpServers"]["remote"],
        serde_json::json!({
            "type": "http",
            "url": "https://mcp.example.com/mcp",
            "headers": { "X-Tenant": "acme" },
        })
    );
    assert!(
        !home.path().join(".bingo/settings.local.json").exists(),
        "a project file is never written"
    );

    let got = run(mcp(home.path()).args(["get", "remote"]));
    assert_eq!(got.status.code(), Some(0), "stderr: {}", stderr(&got));
    let said = stdout(&got);
    assert!(said.contains("type: http"), "{said}");
    assert!(said.contains("url: https://mcp.example.com/mcp"), "{said}");
    assert!(said.contains("headers: X-Tenant"), "{said}");

    let listed = run(mcp(home.path()).arg("list"));
    assert_eq!(
        stdout(&listed),
        "remote\thttp\thttps://mcp.example.com/mcp\n"
    );

    let removed = run(mcp(home.path()).args(["remove", "remote"]));
    assert_eq!(removed.status.code(), Some(0));
    assert!(stdout(&removed).starts_with("Removed the mcp server `remote`"));
    assert_eq!(
        stdout(&run(mcp(home.path()).arg("list"))),
        "No mcp servers are configured.\n"
    );
}

/// A stdio server keeps the arguments after its command, and a neighbour
/// already in the file is left where it was.
#[test]
fn a_stdio_server_joins_the_ones_that_are_already_there() {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".bingo")).unwrap();
    std::fs::write(
        user_settings(home.path()),
        serde_json::json!({
            "model": "fake/one",
            "mcpServers": { "kept": { "command": "true" } },
        })
        .to_string(),
    )
    .unwrap();

    let added = run(mcp(home.path()).args([
        "add",
        "-e",
        "TOKEN=s3cret",
        "files",
        "npx",
        "-y",
        "mcp-files",
    ]));
    assert_eq!(added.status.code(), Some(0), "stderr: {}", stderr(&added));
    let settings = settings_json(home.path());
    assert_eq!(
        settings["mcpServers"]["files"],
        serde_json::json!({
            "type": "stdio",
            "command": "npx",
            "args": ["-y", "mcp-files"],
            "env": { "TOKEN": "s3cret" },
        })
    );
    assert_eq!(settings["mcpServers"]["kept"]["command"], "true");
    assert_eq!(
        settings["model"], "fake/one",
        "a neighbour key is untouched"
    );

    assert_eq!(
        stdout(&run(mcp(home.path()).arg("list"))),
        "files\tstdio\tnpx -y mcp-files\nkept\tstdio\ttrue\n"
    );
}

/// ADR-0050 §4: `get` prints the names of a person's headers and environment
/// and never what is in them.
#[test]
fn get_never_prints_a_header_or_an_environment_value() {
    let home = tempfile::tempdir().unwrap();
    run(mcp(home.path()).args([
        "add",
        "-t",
        "http",
        "-H",
        "Authorization: Bearer ghp_live",
        "remote",
        "https://mcp.example.com/mcp",
    ]));
    run(mcp(home.path()).args(["add", "-e", "GITHUB_TOKEN=ghp_alive", "files", "npx"]));

    for name in ["remote", "files"] {
        let got = run(mcp(home.path()).args(["get", name]));
        let said = stdout(&got);
        assert!(!said.contains("ghp_live"), "{said}");
        assert!(!said.contains("ghp_alive"), "{said}");
    }
    let remote = stdout(&run(mcp(home.path()).args(["get", "remote"])));
    assert!(remote.contains("Authorization"), "{remote}");
    let files = stdout(&run(mcp(home.path()).args(["get", "files"])));
    assert!(files.contains("GITHUB_TOKEN"), "{files}");
    // The listing carries no secret either.
    let listed = stdout(&run(mcp(home.path()).arg("list")));
    assert!(!listed.contains("ghp_"), "{listed}");
}

#[test]
fn a_name_nobody_configured_is_one_error_line_and_exit_1() {
    let home = tempfile::tempdir().unwrap();
    for verb in ["get", "remove", "login", "logout"] {
        let out = run(mcp(home.path()).args([verb, "nope"]));
        assert_eq!(out.status.code(), Some(1), "{verb}: {}", stderr(&out));
        assert_eq!(stdout(&out), "", "{verb} says nothing on stdout");
        let err = stderr(&out);
        assert!(
            err.starts_with("[error] code=INVALID_INPUT"),
            "{verb}: {err}"
        );
        assert!(err.contains("nope"), "{verb}: {err}");
    }
}

#[test]
fn a_row_the_plugin_would_refuse_is_refused_before_it_is_written() {
    let home = tempfile::tempdir().unwrap();
    // Headers belong to an http server.
    let out = run(mcp(home.path()).args(["add", "-H", "X: y", "files", "npx"]));
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("http server"), "{}", stderr(&out));
    assert!(
        !user_settings(home.path()).exists(),
        "a refused row writes nothing"
    );

    let out = run(mcp(home.path()).args(["add", "-t", "http", "-H", "nonsense", "r", "https://a"]));
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("nonsense"), "{}", stderr(&out));
}

/// A stdio server signs in to nothing: its credentials are its own
/// environment, and there is no `401` bingo could answer for it.
#[test]
fn a_child_process_cannot_be_signed_in_to() {
    let home = tempfile::tempdir().unwrap();
    run(mcp(home.path()).args(["add", "files", "npx"]));
    let out = run(mcp(home.path()).args(["login", "files"]));
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).contains("signs in to nothing"),
        "{}",
        stderr(&out)
    );
}

/// A server that was never signed in to is signed out of by forgetting: the
/// verb says so and changes nothing.
#[test]
fn signing_out_of_a_server_nobody_signed_in_to_is_a_no_op() {
    let home = tempfile::tempdir().unwrap();
    run(mcp(home.path()).args(["add", "-t", "http", "remote", "https://mcp.example.com/mcp"]));
    let out = run(mcp(home.path()).args(["logout", "remote"]));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Signed out of remote.\n");
    assert!(
        !home.path().join(".bingo/data/auth.json").exists(),
        "nothing was stored, so nothing was written"
    );
}

/// The whole flow from a terminal: a server that answers `401`, discovery,
/// a registration, the address on stderr, the code typed back, and the token
/// in `auth.json` at 0600 under `mcp:<name>` — never in the settings.
#[tokio::test]
async fn a_pasted_code_signs_in_and_the_token_lands_in_auth_json() {
    let server = authorization_server().await;
    let uri = server.uri();
    let home = tempfile::tempdir().unwrap();
    let (endpoint, at_home) = (format!("{uri}/mcp"), home.path().to_path_buf());

    let signed = tokio::task::spawn_blocking(move || {
        let added = run(mcp(&at_home).args(["add", "-t", "http", "remote", &endpoint]));
        assert_eq!(added.status.code(), Some(0), "stderr: {}", stderr(&added));
        // A bare code is what a person reads off the page when the redirect
        // could not reach this machine; there is no nonce to check because
        // there was no redirect to forge.
        typed(
            mcp(&at_home)
                .env("BINGO_NO_BROWSER", "1")
                .args(["login", "remote", "--paste"]),
            &["ac-1"],
        )
    })
    .await
    .unwrap();
    assert_eq!(signed.status.code(), Some(0), "stderr: {}", stderr(&signed));
    assert_eq!(stdout(&signed), "Signed in to remote.\n");

    let err = stderr(&signed);
    assert!(err.contains("/authorize?"), "the address is shown: {err}");
    assert!(err.contains("code_challenge_method=S256"), "{err}");
    assert!(
        err.contains("scope=mcp%3Atools"),
        "the scope it asked for: {err}"
    );
    assert!(
        err.contains("resource=http"),
        "the audience rides along: {err}"
    );
    assert!(!err.contains("at_1"), "no token on any stream: {err}");

    let auth = home.path().join(".bingo/data/auth.json");
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&auth).unwrap()).unwrap();
    assert_eq!(stored["mcp:remote"]["type"], "mcpOauth");
    assert_eq!(stored["mcp:remote"]["access"], "at_1");
    assert_eq!(stored["mcp:remote"]["refresh"], "rt_1");
    assert_eq!(stored["mcp:remote"]["clientId"], "cl_1");
    assert_eq!(stored["mcp:remote"]["issuer"], uri);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&auth).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "auth.json is the person's alone: {mode:o}");
    }

    // ADR-0012 §2: a token never enters a settings file.
    let settings = std::fs::read_to_string(user_settings(home.path())).unwrap();
    assert!(!settings.contains("at_1"), "{settings}");
    assert!(!settings.contains("rt_1"), "{settings}");

    let at_home = home.path().to_path_buf();
    let out = tokio::task::spawn_blocking(move || run(mcp(&at_home).args(["logout", "remote"])))
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Signed out of remote.\n");
    let stored: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&auth).unwrap()).unwrap();
    assert!(stored.get("mcp:remote").is_none(), "{stored}");
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|request| request.url.path() == "/revoke")
            .count(),
        1,
        "signing out revokes where the issuer offers it"
    );
}

/// ADR-0050 §4: the print surface renders no sign-in, so `/mcp login` there
/// is refused in words that name the headless twin.
#[tokio::test]
async fn the_print_surface_refuses_a_sign_in_and_names_the_way_through() {
    let server = authorization_server().await;
    let home = tempfile::tempdir().unwrap();
    let settings = home.path().join("mcp-settings.json");
    std::fs::write(
        &settings,
        serde_json::json!({
            "mcpServers": {
                "remote": { "type": "http", "url": format!("{}/mcp", server.uri()) }
            }
        })
        .to_string(),
    )
    .unwrap();

    let at_home = home.path().to_path_buf();
    let out = tokio::task::spawn_blocking(move || {
        let empty = script(r#"{"responses":[]}"#);
        run(bingo()
            .env("BINGO_FAKE_SCRIPT", empty.path())
            .env("BINGO_NO_BROWSER", "1")
            .envs(home_env(&at_home))
            .arg("--settings")
            .arg(&settings)
            .args(["--print", "--cwd"])
            .arg(&at_home)
            .arg("/mcp login remote"))
    })
    .await
    .unwrap();

    assert_eq!(out.status.code(), Some(1), "stdout: {}", stdout(&out));
    let err = stderr(&out);
    assert!(err.contains("bingo mcp login remote"), "{err}");
    assert!(
        !home.path().join(".bingo/data/auth.json").exists(),
        "a refused sign-in stores nothing"
    );
}

/// A resource server that wants a bearer, and the authorization server it
/// names: the whole ladder, mounted on one origin.
async fn authorization_server() -> wiremock::MockServer {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;
    let uri = server.uri();
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(
            ResponseTemplate::new(401).insert_header(
                "www-authenticate",
                format!(
                    r#"Bearer resource_metadata="{uri}/.well-known/oauth-protected-resource/mcp""#
                )
                .as_str(),
            ),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-protected-resource/mcp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "resource": format!("{uri}/mcp"),
            "authorization_servers": [uri.clone()],
            "scopes_supported": ["mcp:tools"],
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/.well-known/oauth-authorization-server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "issuer": uri,
            "authorization_endpoint": format!("{uri}/authorize"),
            "token_endpoint": format!("{uri}/token"),
            "registration_endpoint": format!("{uri}/register"),
            "revocation_endpoint": format!("{uri}/revoke"),
            "code_challenge_methods_supported": ["S256"],
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/register"))
        .respond_with(
            ResponseTemplate::new(201).set_body_json(serde_json::json!({ "client_id": "cl_1" })),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "at_1",
            "refresh_token": "rt_1",
            "expires_in": 3600,
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/revoke"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    server
}

/// `--mcp-config` is what the next run would dial, so it is what the listing
/// shows.
#[test]
fn a_run_configured_by_flag_lists_what_that_run_would_dial() {
    let home = tempfile::tempdir().unwrap();
    let file = script(r#"{"mcpServers": {"flagged": {"command": "true"}}}"#);
    let listed = run(bingo()
        .envs(home_env(home.path()))
        .arg("--cwd")
        .arg(home.path())
        .arg("--mcp-config")
        .arg(file.path())
        .args(["mcp", "list"]));
    assert_eq!(listed.status.code(), Some(0), "stderr: {}", stderr(&listed));
    assert_eq!(stdout(&listed), "flagged\tstdio\ttrue\n");
}

//! Black-box: the binary as a host would run it. Stdout carries prose (or
//! frames) and nothing else; every diagnostic is on stderr; a failure is
//! one `[error] code=… msg=…` line and a non-zero exit.

// An integration test is not `cfg(test)`; the test-only lint relief is spelled out.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use bingo_sdk::{Event, Frame, TurnStatus};

/// A HOME no test shares with the developer. The settings, stores and locks
/// of the person running the suite must never reach a black-box run — a real
/// configured channel once turned every bare run into a listener that fought
/// the developer's own gateway for its lock. Tests that write set their own
/// HOME as before; this is the floor under the ones that only read.
fn isolated_home() -> &'static std::path::Path {
    static HOME: std::sync::LazyLock<tempfile::TempDir> =
        std::sync::LazyLock::new(|| tempfile::tempdir().expect("a home for the suite"));
    HOME.path()
}

fn bingo() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_bingo"));
    // Nor may a run inherit the developer's credentials: a host asks every
    // endpoint it can sign in to what it serves (ADR-0026 §4), and a suite
    // must not reach the network because whoever ran it has a key exported.
    // A test that wants one sets it itself, and still wins — this is a floor.
    cmd.env("HOME", isolated_home())
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("BINGO_FAKE_SCRIPT")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

fn run(cmd: &mut Command) -> Output {
    cmd.output().expect("the binary runs")
}

/// `run`, for a scenario whose failure is a hang: past `limit` the process is
/// killed and the test fails, instead of the suite waiting on it. The output
/// is read once the process has exited, so it must fit the pipe.
fn run_within(cmd: &mut Command, limit: Duration) -> Output {
    let mut child = cmd.spawn().expect("the binary runs");
    let started = Instant::now();
    loop {
        if child.try_wait().expect("wait").is_some() {
            return child.wait_with_output().expect("output");
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            panic!("bingo did not exit within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The binary with a person at the keyboard: every line is answered in order,
/// then stdin closes, so a prompt that wanted more sees the end of it.
fn typed(cmd: &mut Command, answers: &[&str]) -> Output {
    let mut child = cmd.stdin(Stdio::piped()).spawn().expect("the binary runs");
    let mut stdin = child.stdin.take().expect("stdin");
    for answer in answers {
        writeln!(stdin, "{answer}").expect("the answer is typed");
    }
    drop(stdin);
    child.wait_with_output().expect("output")
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
    let script = script(r#"{"responses":[{"steps":[{"text":"Hello from the fake provider."}]}]}"#);
    let out = run(bingo().env("BINGO_FAKE_SCRIPT", script.path()).args([
        "--print",
        "--provider",
        "fake",
        "hello",
    ]));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Hello from the fake provider.\n");
    assert_eq!(stderr(&out), "");
}

#[test]
fn json_output_is_one_frame_per_line_ending_in_turn_completed() {
    let script = script(r#"{"responses":[{"steps":[{"text":"Hello from the fake provider."}]}]}"#);
    let out = run(bingo().env("BINGO_FAKE_SCRIPT", script.path()).args([
        "--print",
        "--output-format",
        "json",
        "hello",
    ]));
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

/// The terminal interface needs a terminal at both ends; a pipe keeps the
/// headless one, with or without `--print`.
#[test]
fn a_pipe_without_print_still_runs_headlessly() {
    let script = script(r#"{"responses":[{"steps":[{"text":"Hello from the fake provider."}]}]}"#);
    let out = run(bingo().env("BINGO_FAKE_SCRIPT", script.path()).arg("hello"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Hello from the fake provider.\n");
}

#[test]
fn a_question_is_declined_when_nobody_is_at_the_keyboard() {
    let script = script(
        r#"{"responses":[
            {"steps":[{"text":"Asking."},{"toolCall":{"name":"AskUserQuestion","input":{"questions":[
                {"question":"Which store?","header":"Store","options":[
                    {"label":"Postgres","description":"relational"},{"label":"Redis","description":"key-value"}]}]}}}]},
            {"steps":[{"text":"Fine."}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "pick one"]));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Asking.Fine.\n");
    let err = stderr(&out);
    assert!(err.contains("[tool] AskUserQuestion error"), "{err}");
}

#[test]
fn an_edit_is_asked_and_denied_off_a_tty_under_the_default_policy() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("greeting.txt");
    std::fs::write(&file, "alpha\n").unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Edit","input":{"file_path":"greeting.txt","old_string":"alpha","new_string":"beta"}}}]},
            {"steps":[{"text":"Could not."}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .args(["--print", "--output-format", "json", "--cwd"])
        .arg(dir.path())
        .arg("change it"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "alpha\n",
        "the file was edited"
    );
    let frames: Vec<Frame> = stdout(&out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let opened = frames.iter().any(|f| {
        matches!(&f.event, Event::InteractionOpened { interaction }
            if matches!(&interaction.kind, bingo_sdk::InteractionKind::Permission { tool, .. } if tool == "Edit"))
    });
    assert!(opened, "the gate asked for the edit");
    let denied = frames.iter().any(|f| {
        matches!(
            &f.event,
            Event::InteractionResolved {
                answer: bingo_sdk::Answer::Deny { .. },
                ..
            }
        )
    });
    assert!(denied, "the surface refused off a TTY");
    let failed = frames.iter().any(|f| {
        matches!(&f.event, Event::ItemCompleted { item }
            if matches!(&item.body, bingo_sdk::ItemBody::ToolCall { output: Some(o), .. } if o.is_error))
    });
    assert!(failed, "the tool result carries the denial");
}

#[test]
fn anthropic_without_credentials_fails_before_any_turn() {
    let out = run(bingo()
        .env_remove("ANTHROPIC_API_KEY")
        .env("HOME", tempfile::tempdir().unwrap().path())
        .args(["--print", "--provider", "anthropic", "hello"]));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "");
    let err = stderr(&out);
    assert!(err.starts_with("[error] code=AUTH_REQUIRED msg="), "{err}");
    assert_eq!(err.lines().count(), 1, "{err}");
}

#[test]
fn plan_mode_denies_a_write_and_the_turn_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Write","input":{"file_path":"new.txt","content":"x"}}}]},
            {"steps":[{"text":"Blocked."}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", dir.path())
        .args(["--print", "--permission-mode", "plan", "--cwd"])
        .arg(dir.path())
        .arg("write it"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        !dir.path().join("new.txt").exists(),
        "plan mode wrote a file"
    );
    assert!(
        stderr(&out).contains("[tool] Write error"),
        "{}",
        stderr(&out)
    );
    assert_eq!(stdout(&out), "Blocked.\n");
}

#[test]
fn bash_runs_only_when_the_flags_allow_it() {
    let dir = tempfile::tempdir().unwrap();
    let script_json = r#"{"responses":[
        {"steps":[{"toolCall":{"name":"Bash","input":{"command":"echo hi"}}}]},
        {"steps":[{"text":"Ran."}]}
    ]}"#;
    let cases: [(&[&str], &str); 3] = [
        (&[], "[tool] Bash error"),
        (&["--dangerously-skip-permissions"], "[tool] Bash ok"),
        (&["--allowed-tools", "Bash(echo:*)"], "[tool] Bash ok"),
    ];
    for (flags, expected) in cases {
        let script = script(script_json);
        let out = run(bingo()
            .env("BINGO_FAKE_SCRIPT", script.path())
            .env("HOME", dir.path())
            .args(["--print", "--cwd"])
            .arg(dir.path())
            .args(flags)
            .arg("run it"));
        assert_eq!(out.status.code(), Some(0), "{flags:?}: {}", stderr(&out));
        assert!(
            stderr(&out).contains(expected),
            "{flags:?}: {}",
            stderr(&out)
        );
    }
}

#[test]
fn a_slow_bash_command_streams_its_tail_as_deltas() {
    let dir = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Bash","input":{"command":"for i in 1 2 3 4; do echo line$i; sleep 0.12; done"}}}]},
            {"steps":[{"text":"Streamed."}]}
        ]}"#,
    );
    let out = run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", dir.path())
        .args([
            "--print",
            "--output-format",
            "json",
            "--dangerously-skip-permissions",
            "--cwd",
        ])
        .arg(dir.path())
        .arg("count"));
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let frames: Vec<Frame> = stdout(&out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let tails: Vec<&str> = frames
        .iter()
        .filter_map(|f| match &f.event {
            Event::ItemDelta {
                kind: bingo_sdk::DeltaKind::Tail,
                data,
                ..
            } => Some(data.as_str()),
            _ => None,
        })
        .collect();
    assert!(!tails.is_empty(), "no live tail reached the wire");
    assert!(tails.last().unwrap().contains("line"), "{tails:?}");
    let done = frames.iter().any(|f| matches!(&f.event, Event::ItemCompleted { item }
        if matches!(&item.body, bingo_sdk::ItemBody::ToolCall { output: Some(o), .. }
            if !o.is_error && o.parts.iter().any(|p| p.as_text().is_some_and(|t| t.contains("line4"))))));
    assert!(done, "the final result carries every line");
}

/// The page a scripted turn fetches, served by wiremock on the loopback.
async fn page_server() -> wiremock::MockServer {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/guide"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            "<html><head><title>Guide</title></head><body><nav>menu</nav>\
             <article><h1>Installing</h1><p>Run the <a href=\"/i\">installer</a> \
             first, then read the rest of this page carefully because it says \
             what the installer leaves for you to do by hand.</p></article>\
             <script>track()</script></body></html>",
            "text/html; charset=utf-8",
        ))
        .expect(1)
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn web_fetch_hands_the_model_the_page_as_markdown() {
    let server = page_server().await;
    let url = format!("{}/guide", server.uri());
    let script = script(&format!(
        r#"{{"responses":[
            {{"steps":[{{"toolCall":{{"name":"WebFetch","input":{{"url":"{url}"}}}}}}]}},
            {{"steps":[{{"text":"Read it."}}]}}
        ]}}"#
    ));
    let out = tokio::task::spawn_blocking(move || {
        run(bingo()
            .env("BINGO_FAKE_SCRIPT", script.path())
            .env("HOME", tempfile::tempdir().unwrap().path())
            .args([
                "--print",
                "--output-format",
                "json",
                "--allowed-tools",
                "WebFetch(domain:127.0.0.1)",
                "fetch the guide",
            ]))
    })
    .await
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let frames: Vec<Frame> = stdout(&out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect();
    let markdown = frames
        .iter()
        .find_map(|f| match &f.event {
            Event::ItemCompleted { item } => match &item.body {
                bingo_sdk::ItemBody::ToolCall {
                    name,
                    output: Some(output),
                    ..
                } if name == "WebFetch" => Some(output.clone()),
                _ => None,
            },
            _ => None,
        })
        .expect("the WebFetch call completed with an output");
    assert!(!markdown.is_error, "{markdown:?}");
    let text = markdown
        .parts
        .iter()
        .filter_map(|p| p.as_text())
        .collect::<String>();
    assert!(text.contains("# Installing"), "{text}");
    assert!(text.contains("[installer]("), "{text}");
    assert!(
        !text.contains("track()") && !text.contains("menu"),
        "{text}"
    );
}

/// A recorded Responses API stream from the provider crate's fixtures.
fn responses_fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../bingo-provider-openai/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The Responses endpoint answering each `POST /v1/responses` with the next
/// fixture in order, then refusing any further call.
async fn responses_server(fixtures: &[&str]) -> wiremock::MockServer {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, ResponseTemplate};
    let server = wiremock::MockServer::start().await;
    for name in fixtures {
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer sk-test"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(responses_fixture(name), "text/event-stream"),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }
    server
}

fn openai(server: &wiremock::MockServer, cwd: &std::path::Path, prompt: &str) -> Command {
    let mut cmd = bingo();
    cmd.env("OPENAI_API_KEY", "sk-test")
        .env("OPENAI_BASE_URL", server.uri())
        .env("HOME", cwd)
        .args([
            "--print",
            "--provider",
            "openai",
            "--model",
            "gpt-5.4",
            "--cwd",
        ])
        .arg(cwd)
        .arg(prompt);
    cmd
}

#[tokio::test]
async fn openai_streams_a_text_turn_through_the_same_loop() {
    let server = responses_server(&["text.sse"]).await;
    let dir = tempfile::tempdir().unwrap();
    let out = tokio::task::spawn_blocking(move || run(&mut openai(&server, dir.path(), "hi")))
        .await
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(stdout(&out), "Hello, world.\n");
    assert_eq!(stderr(&out), "");
}

#[tokio::test]
async fn openai_runs_a_tool_round_and_feeds_the_result_back() {
    let server = responses_server(&["tools.sse", "text.sse"]).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
    let out = tokio::task::spawn_blocking(move || {
        run(openai(&server, dir.path(), "what is in the manifest?")
            .args(["--output-format", "json"]))
    })
    .await
    .unwrap();
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let frames: Vec<Frame> = stdout(&out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect();
    let read_completed = frames.iter().any(|f| {
        matches!(
            &f.event,
            Event::ItemCompleted { item }
                if matches!(&item.body, bingo_sdk::ItemBody::ToolCall { name, output: Some(o), .. }
                    if name == "Read" && !o.is_error)
        )
    });
    assert!(read_completed, "the Read call completed: {}", stdout(&out));
    assert!(matches!(
        frames.last().map(|f| &f.event),
        Some(Event::TurnCompleted {
            status: TurnStatus::Completed,
            ..
        })
    ));
}

#[test]
fn openai_without_credentials_names_the_variable_before_any_turn() {
    let out = run(bingo()
        .env_remove("OPENAI_API_KEY")
        .env("HOME", tempfile::tempdir().unwrap().path())
        .args([
            "--print",
            "--provider",
            "openai",
            "--model",
            "gpt-5.4",
            "hello",
        ]));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), "");
    let err = stderr(&out);
    assert!(err.starts_with("[error] code=AUTH_REQUIRED msg="), "{err}");
    assert!(err.contains("OPENAI_API_KEY"), "{err}");
    assert_eq!(err.lines().count(), 1, "{err}");
}

/// The frames a `--output-format json` run wrote to stdout.
fn frames_of(out: &Output) -> Vec<Frame> {
    stdout(out)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

fn scripted_run(
    home: &std::path::Path,
    script: &tempfile::NamedTempFile,
    extra: &[&str],
    prompt: &str,
) -> Output {
    run(bingo()
        .env("BINGO_FAKE_SCRIPT", script.path())
        .env("HOME", home)
        .args(["--print", "--output-format", "json", "--cwd"])
        .arg(home)
        .args(extra)
        .arg(prompt))
}

mod agents;
mod batch;
mod board;
mod context;
mod experience;
mod gateway;
mod hooks;
mod instances;
mod jobs;
mod login;
mod mentions;
mod models;
mod peers;
mod provider_add;
mod rooms;
mod schedule;
mod sessions;
mod skills;
mod stream_json;
mod tasks;
mod views;

/// `--mcp-config` names a file whose `mcpServers` join the settings for this
/// run; a file that is not there, or has none, stops the run before a turn.
#[test]
fn mcp_config_must_exist_and_carry_servers() {
    let missing = run(bingo().args(["--print", "--mcp-config", "/no/such/mcp.json", "hi"]));
    assert_eq!(missing.status.code(), Some(1));
    assert!(stderr(&missing).contains("code=INVALID_INPUT"));
    assert!(stderr(&missing).contains("does not exist"));

    let file = script(r#"{"somethingElse": {}}"#);
    let empty = run(bingo()
        .args(["--print", "--mcp-config"])
        .arg(file.path())
        .arg("hi"));
    assert_eq!(empty.status.code(), Some(1));
    assert!(
        stderr(&empty).contains("has no mcpServers"),
        "{}",
        stderr(&empty)
    );
}

//! Black-box: the binary as a host drives it over JSON-RPC (ADR-0007). A
//! `RemoteKernel` talks to a spawned `bingo serve --stdio`; what it folds is
//! what a GUI would show.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use bingo_sdk::{
    Activation, Answer, Attachment, CatalogKind, Effort, ErrorCode, Event, Frame, HistoryPage,
    HostApi, Input, IntentId, InterruptScope, OpenOptions, Origin, SessionFilter, SessionId,
    SessionSelector, SessionSpec, SessionState, TurnStatus,
};
use futures::StreamExt;
use tokio::io::{AsyncBufReadExt, BufReader};

mod support;

use support::{LIMIT, Server, ack_for, create, ready, send, until_completed, who};

const TEXT_TURN: &str = r#"{"responses":[{"steps":[{"text":"Hello over the wire."}]}]}"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_method_before_initialize_is_refused() {
    let mut server = Server::spawn(TEXT_TURN);
    let kernel = server.kernel();
    let err = kernel
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .err()
        .unwrap();
    assert_eq!(err.code, ErrorCode::NotInitialized);
    kernel.initialize(who()).await.unwrap();
    kernel.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn stdout_carries_json_rpc_lines_only_and_an_unknown_method_is_refused() {
    let mut server = Server::spawn(TEXT_TURN);
    let mut stdin = server.child.stdin.take().unwrap();
    let mut lines = BufReader::new(server.child.stdout.take().unwrap()).lines();
    let mut next = async |n: u32| -> serde_json::Value {
        let line = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap_or_else(|| panic!("line {n} never came"));
        let value: serde_json::Value =
            serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"));
        assert_eq!(value["jsonrpc"], "2.0", "{line}");
        value
    };
    send(&mut stdin, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"client":{"name":"raw","surface":"test"},"protocol":1}}"#).await;
    assert_eq!(next(1).await["id"], 1);
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":2,"method":"no/such","params":{}}"#,
    )
    .await;
    let refused = next(2).await;
    assert_eq!(refused["error"]["code"], -32601, "{refused}");
    send(&mut stdin, "this is not json").await;
    let parse = next(3).await;
    assert_eq!(parse["error"]["code"], -32700, "{parse}");
    assert!(parse["id"].is_null());
    let open = format!(
        r#"{{"jsonrpc":"2.0","id":4,"method":"session/open","params":{{"selector":{{"kind":"create","spec":{{"cwd":"{}"}}}}}}}}"#,
        server.cwd().display()
    );
    send(&mut stdin, &open).await;
    let opened = next(4).await;
    let session = opened["result"]["session"].as_str().unwrap().to_string();
    let submit = format!(
        r#"{{"jsonrpc":"2.0","id":5,"method":"session/submit","params":{{"session":"{session}","intent":"req_01HARNESS0000000000000001","input":{{"kind":"text","text":"hi","origin":{{"surface":"raw"}}}}}}}}"#
    );
    send(&mut stdin, &submit).await;
    let mut n = 5;
    loop {
        n += 1;
        let line = next(n).await;
        if line.get("id") == Some(&serde_json::json!(5)) {
            assert!(line["result"].is_object());
            continue;
        }
        assert_eq!(line["method"], "event", "{line}");
        assert_eq!(line["params"]["session"], session);
        if line["params"]["event"]["type"] == "turnCompleted" {
            assert_eq!(line["params"]["event"]["status"]["kind"], "completed");
            break;
        }
    }
    send(
        &mut stdin,
        r#"{"jsonrpc":"2.0","id":9,"method":"shutdown","params":{}}"#,
    )
    .await;
    assert!(next(99).await["result"].is_object());
    assert!(server.child.wait().await.unwrap().success());
}

#[tokio::test(flavor = "multi_thread")]
async fn open_submit_and_the_events_arrive_in_seq_order_with_the_clients_intent() {
    let mut server = Server::spawn(TEXT_TURN);
    let kernel = ready(&mut server).await;
    let mut attachment = kernel
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    let opened_at = attachment.snapshot.seq;
    let intent = IntentId::mint();
    attachment
        .handle
        .submit(intent.clone(), Input::text("hi", Origin::surface("test")));
    let frames = until_completed(&mut attachment).await;
    assert!(frames[0].seq > opened_at, "the snapshot comes first");
    assert!(
        frames.windows(2).all(|w| w[1].seq == w[0].seq.next()),
        "no gap, no repeat"
    );
    assert!(
        frames
            .iter()
            .any(|f| matches!(&f.event, Event::IntentAck { intent: i, .. } if i == &intent))
    );
    let state: &SessionState = &attachment.snapshot;
    assert!(state.items.iter().any(|i| matches!(&i.body, bingo_sdk::ItemBody::Assistant { text } if text == "Hello over the wire.")));
    assert_eq!(state.last_turn, Some(TurnStatus::Completed));
    kernel.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_reaches_a_running_turn() {
    let mut server = Server::spawn(
        r#"{"responses":[{"steps":[{"text":"slow"},{"delay":{"ms":30000}},{"text":"never"}]}]}"#,
    );
    let kernel = ready(&mut server).await;
    let mut attachment = kernel
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    attachment
        .handle
        .submit(IntentId::mint(), Input::text("go", Origin::surface("test")));
    loop {
        let frame = attachment.events.next().await.unwrap();
        attachment.snapshot.apply(&frame);
        if matches!(frame.event, Event::ItemDelta { .. }) {
            break;
        }
    }
    let started = std::time::Instant::now();
    attachment
        .handle
        .interrupt(IntentId::mint(), InterruptScope::Head);
    let frames = until_completed(&mut attachment).await;
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the interrupt did not wait for the delay"
    );
    assert!(
        matches!(
            frames.last().map(|f| &f.event),
            Some(Event::TurnCompleted {
                status: TurnStatus::Interrupted { .. },
                ..
            })
        ),
        "{:?}",
        frames.iter().map(|f| &f.event).collect::<Vec<_>>()
    );
    kernel.shutdown().await.unwrap();
}

/// The whole of what one `esc` promises, end to end: the turn a person
/// stopped is over, and the shell command it was waiting on is gone —
/// grandchildren included, which is what a process group buys and what
/// `KillOnDrop` on its own would not.
///
/// The probe is a file a backgrounded loop writes to: what a killed group
/// leaves behind is a file that stops growing. Unix only — the `Bash` tool
/// spawns no shell on Windows at all, so there is nothing there to stop.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn one_interrupt_ends_the_turn_and_the_command_it_was_running() {
    let dir = tempfile::tempdir().unwrap();
    let ticks = dir.path().join("ticks");
    let command = format!(
        "(while true; do echo tick >> '{}'; sleep 0.05; done) & sleep 30",
        ticks.display()
    );
    let script = serde_json::json!({
        "responses": [
            { "steps": [{ "toolCall": { "name": "Bash", "input": { "command": command } } }] },
            { "steps": [{ "text": "stopped" }] },
        ]
    });
    let mut server = Server::spawn_with(&script.to_string(), &["--dangerously-skip-permissions"]);
    let kernel = ready(&mut server).await;
    let mut attachment = kernel
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    attachment.handle.submit(
        IntentId::mint(),
        Input::text("run it", Origin::surface("test")),
    );
    // Wait for the command to be at work rather than for a clock: a loaded
    // box makes a guess of any fixed wait, and the subject here is a command
    // that is definitely running.
    let wrote = wait_for_file(&ticks).await;
    assert!(wrote > 0, "the command never started");

    let started = std::time::Instant::now();
    attachment
        .handle
        .interrupt(IntentId::mint(), InterruptScope::Head);
    let frames = until_completed(&mut attachment).await;
    assert!(
        matches!(
            frames.last().map(|f| &f.event),
            Some(Event::TurnCompleted {
                status: TurnStatus::Interrupted { .. },
                ..
            })
        ),
        "{:?}",
        frames.iter().map(|f| &f.event).collect::<Vec<_>>()
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the interrupt waited for the command"
    );

    // Nothing in the group is writing any more, the loop the shell put in the
    // background included.
    tokio::time::sleep(SETTLE).await;
    let after = std::fs::metadata(&ticks).map(|m| m.len()).unwrap_or(0);
    tokio::time::sleep(SETTLE).await;
    let later = std::fs::metadata(&ticks).map(|m| m.len()).unwrap_or(0);
    assert_eq!(after, later, "the process group outlived the interrupt");
    kernel.shutdown().await.unwrap();
}

/// Long enough that a killed group has certainly stopped writing, short
/// enough to wait twice.
#[cfg(unix)]
const SETTLE: Duration = Duration::from_millis(400);

/// Poll until the file has something in it, bounded generously.
#[cfg(unix)]
async fn wait_for_file(path: &std::path::Path) -> u64 {
    for _ in 0..400 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        let written = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if written > 0 {
            return written;
        }
    }
    0
}

#[tokio::test(flavor = "multi_thread")]
async fn a_permission_is_answered_over_the_wire_and_the_tool_runs() {
    let mut server = Server::spawn(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Write","input":{"file_path":"made.txt","content":"by the wire\n"}}}]},
            {"steps":[{"text":"Written."}]}
        ]}"#,
    );
    let kernel = ready(&mut server).await;
    let mut attachment = kernel
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    attachment.handle.submit(
        IntentId::mint(),
        Input::text("write it", Origin::surface("test")),
    );
    let interaction = loop {
        let frame = attachment.events.next().await.unwrap();
        attachment.snapshot.apply(&frame);
        if let Event::InteractionOpened { interaction } = frame.event {
            break interaction;
        }
    };
    attachment.handle.answer(
        IntentId::mint(),
        interaction.id,
        Answer::AllowOnce,
        Activation::Pointer,
    );
    let frames = until_completed(&mut attachment).await;
    assert!(
        frames
            .iter()
            .any(|f| matches!(f.event, Event::InteractionResolved { .. }))
    );
    assert_eq!(attachment.snapshot.last_turn, Some(TurnStatus::Completed));
    assert_eq!(
        std::fs::read_to_string(server.cwd().join("made.txt")).unwrap(),
        "by the wire\n"
    );
    kernel.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retry_is_visible_on_the_wire() {
    let mut server = Server::spawn(
        r#"{"responses":[
            {"steps":[{"error":{"kind":"server","status":529,"message":"overloaded"}}]},
            {"steps":[{"text":"Second try."}]}
        ]}"#,
    );
    let kernel = ready(&mut server).await;
    let mut attachment = kernel
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    attachment
        .handle
        .submit(IntentId::mint(), Input::text("go", Origin::surface("test")));
    let frames = until_completed(&mut attachment).await;
    assert!(
        frames
            .iter()
            .any(|f| matches!(f.event, Event::TurnRetrying { attempt: 1, .. }))
    );
    assert_eq!(attachment.snapshot.last_turn, Some(TurnStatus::Completed));
    kernel.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_session_written_by_a_print_run_reopens_by_id_with_its_items() {
    let mut server = Server::spawn(TEXT_TURN);
    let cwd = server.cwd();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_bingo"))
        .env("BINGO_FAKE_SCRIPT", cwd.join("script.json"))
        .env("HOME", &cwd)
        .args(["--print", "--output-format", "json", "--cwd"])
        .arg(&cwd)
        .arg("first")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let first: Frame = serde_json::from_str(
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    let id: SessionId = first.session.clone();

    let kernel = ready(&mut server).await;
    let listed = kernel
        .sessions(SessionFilter {
            cwd: Some(cwd.clone()),
            ..SessionFilter::default()
        })
        .await
        .unwrap();
    assert_eq!(listed.iter().map(|s| &s.id).collect::<Vec<_>>(), [&id]);
    let attachment = kernel
        .open(
            SessionSelector::ById { id: id.clone() },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(attachment.session, id);
    assert!(attachment.snapshot.items.iter().any(|i| matches!(&i.body, bingo_sdk::ItemBody::Assistant { text } if text == "Hello over the wire.")));
    let page = attachment
        .handle
        .history(HistoryPage {
            before: None,
            limit: 1,
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert!(page.next.is_some(), "an earlier page exists");
    let earlier = attachment
        .handle
        .history(HistoryPage {
            before: page.next,
            limit: 10,
        })
        .await
        .unwrap();
    assert!(!earlier.items.is_empty());
    kernel.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_catalogue_lists_the_fake_provider_the_tools_and_the_model_facts() {
    let mut server = Server::spawn(TEXT_TURN);
    let kernel = ready(&mut server).await;
    let providers = kernel.catalog(CatalogKind::Providers).await.unwrap();
    assert!(
        providers.entries.iter().any(|e| e.id == "fake"),
        "{providers:?}"
    );
    let tools = kernel.catalog(CatalogKind::Tools).await.unwrap();
    assert!(tools.entries.iter().any(|e| e.id == "Read"), "{tools:?}");
    // The meta a client reads a model's facts from crosses the wire whole
    // (ADR-0026 §1); the keys themselves are pinned in the kernel.
    let models = kernel.catalog(CatalogKind::Models).await.unwrap();
    let known = models
        .entries
        .iter()
        .find(|e| e.id == "anthropic/claude-sonnet-4-5")
        .unwrap_or_else(|| panic!("{models:?}"));
    assert!(known.meta["context"].is_u64(), "{known:?}");
    assert!(known.meta["output"].is_u64(), "{known:?}");
    assert!(known.meta["reasoning"].is_boolean(), "{known:?}");
    assert!(known.meta["images"].is_boolean(), "{known:?}");
    kernel.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_removes_the_session_from_disk_and_shutdown_exits_zero() {
    let mut server = Server::spawn(TEXT_TURN);
    let kernel = ready(&mut server).await;
    let mut attachment = kernel
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    attachment
        .handle
        .submit(IntentId::mint(), Input::text("hi", Origin::surface("test")));
    until_completed(&mut attachment).await;
    let dir = server.sessions_dir().join(attachment.session.to_string());
    assert!(dir.join("journal.jsonl").is_file());
    kernel.delete(&attachment.session).await.unwrap();
    assert!(!dir.exists(), "the directory is gone");
    kernel.shutdown().await.unwrap();
    assert!(server.child.wait().await.unwrap().success());
}

/// A shell line is one shell item in the transcript, carrying the code it came
/// to and where it ran (M65); `/permission` changes what the gate does for this
/// session; both are commands the kernel dispatches (ADR-0008).
#[tokio::test(flavor = "multi_thread")]
async fn a_shell_line_and_a_permission_mode_dispatch_as_commands() {
    let mut server = Server::spawn(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"Write","input":{"file_path":"quiet.txt","content":"no prompt\n"}}}]},
            {"steps":[{"text":"Written."}]}
        ]}"#,
    );
    let kernel = ready(&mut server).await;
    let mut attachment = kernel
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();

    let shell = IntentId::mint();
    attachment.handle.submit(
        shell.clone(),
        Input::text("!echo hi over the wire", Origin::surface("test")),
    );
    let bingo_sdk::IntentOutcome::Applied { result } = ack_for(&mut attachment, &shell).await
    else {
        panic!("a shell line is applied");
    };
    let item = bingo_sdk::ItemId::from_raw(result["item"].as_str().unwrap());
    let recorded = attachment.snapshot.item(&item).unwrap();
    assert!(
        matches!(
            &recorded.body,
            bingo_sdk::ItemBody::Shell { command, output, exit, cwd }
                if command == "echo hi over the wire"
                    && output == "hi over the wire\n"
                    && *exit == Some(0)
                    && *cwd == server.cwd()
        ),
        "{recorded:?}"
    );

    let mode = IntentId::mint();
    attachment.handle.submit(
        mode.clone(),
        Input::text("/permission acceptEdits", Origin::surface("test")),
    );
    let ack = ack_for(&mut attachment, &mode).await;
    assert!(
        matches!(&ack, bingo_sdk::IntentOutcome::Applied { result } if result["message"] == "permission mode: acceptEdits"),
        "{ack:?}"
    );

    let unknown = IntentId::mint();
    attachment.handle.submit(
        unknown.clone(),
        Input::text("/nope", Origin::surface("test")),
    );
    assert!(matches!(
        ack_for(&mut attachment, &unknown).await,
        bingo_sdk::IntentOutcome::Rejected { error } if error.code == ErrorCode::InvalidInput
    ));

    attachment.handle.submit(
        IntentId::mint(),
        Input::text("write it", Origin::surface("test")),
    );
    let frames = until_completed(&mut attachment).await;
    assert!(
        frames
            .iter()
            .all(|f| !matches!(f.event, Event::InteractionOpened { .. })),
        "acceptEdits asks nothing for a Write"
    );
    assert_eq!(attachment.snapshot.last_turn, Some(TurnStatus::Completed));
    assert_eq!(
        std::fs::read_to_string(server.cwd().join("quiet.txt")).unwrap(),
        "no prompt\n"
    );
    kernel.shutdown().await.unwrap();
}

/// The MCP test server `bingo-mcp` ships as an example. `cargo test
/// --workspace` builds it; a run of this binary alone builds it here.
fn echo_server() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile = exe.parent().unwrap().parent().unwrap();
    let server = profile.join("examples").join("echo_server");
    if !server.exists() {
        let built = std::process::Command::new(env!("CARGO"))
            .args(["build", "-p", "bingo-mcp", "--example", "echo_server"])
            .status()
            .unwrap();
        assert!(built.success(), "building the MCP example server");
    }
    server
}

/// A host's `--mcp-config` names a server; its tool arrives in the catalogue
/// once the dial lands, reaches the model untrusted, and runs on approval.
#[tokio::test(flavor = "multi_thread")]
async fn an_mcp_server_from_mcp_config_offers_its_tool_through_the_gate() {
    let server = echo_server();
    let config = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        config.path(),
        serde_json::json!({ "mcpServers": { "test": { "command": server } } }).to_string(),
    )
    .unwrap();
    let config_path = config.path().to_string_lossy().into_owned();
    let mut host = Server::spawn_with(
        r#"{"responses":[
            {"steps":[{"toolCall":{"name":"mcp__test__echo","input":{"text":"over mcp"}}}]},
            {"steps":[{"text":"echoed"}]}
        ]}"#,
        &["--mcp-config", &config_path],
    );
    let kernel = ready(&mut host).await;

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let entry = loop {
        let catalog = kernel.catalog(CatalogKind::Tools).await.unwrap();
        if let Some(entry) = catalog
            .entries
            .into_iter()
            .find(|e| e.id == "mcp__test__echo")
        {
            break entry;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the echo server's tool never reached the catalogue"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(entry.meta["server"], "test");

    let mut attachment = kernel
        .open(create(host.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    attachment.handle.submit(
        IntentId::mint(),
        Input::text("echo it", Origin::surface("test")),
    );
    let interaction = loop {
        let frame = attachment.events.next().await.unwrap();
        attachment.snapshot.apply(&frame);
        if let Event::InteractionOpened { interaction } = frame.event {
            break interaction;
        }
    };
    assert!(
        matches!(&interaction.kind, bingo_sdk::InteractionKind::Permission { tool, .. } if tool == "mcp__test__echo"),
        "an MCP tool is untrusted, so the gate asks: {interaction:?}"
    );
    attachment.handle.answer(
        IntentId::mint(),
        interaction.id,
        Answer::AllowOnce,
        Activation::Pointer,
    );
    until_completed(&mut attachment).await;
    assert_eq!(attachment.snapshot.last_turn, Some(TurnStatus::Completed));
    let echoed = attachment
        .snapshot
        .items
        .iter()
        .find_map(|item| match &item.body {
            bingo_sdk::ItemBody::ToolCall {
                name,
                output: Some(output),
                ..
            } if name == "mcp__test__echo" => output.parts[0].as_text().map(str::to_owned),
            _ => None,
        });
    assert_eq!(echoed.as_deref(), Some("over mcp"));
    kernel.shutdown().await.unwrap();
}

/// The root calls `SpawnAgent` and waits; the child answers; the root reports.
/// One script serves both sessions — the fake provider hands its responses out
/// in the order they are asked for, and a foreground spawn makes that order
/// the tree's: root, child, root.
const FOREGROUND_AGENT: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"prompt":"say hi","background":false}}}]},
    {"steps":[{"text":"hi from the child"}]},
    {"steps":[{"text":"the child said hi"}]}
]}"#;

/// The same spawn left in the background. Both middle responses say the same
/// thing because which of the two sessions asks first is a race no script can
/// settle.
const BACKGROUND_AGENT: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"SpawnAgent","input":{"prompt":"say hi","name":"reviewer"}}}]},
    {"steps":[{"text":"working"}]},
    {"steps":[{"text":"working"}]},
    {"steps":[{"text":"noted"}]}
]}"#;

/// A server driven by raw JSON-RPC lines. `RemoteKernel` files a frame under
/// its own session, so a child's frames wait for a client that claims them by
/// id; reading the wire is how a test sees everything a tree attachment
/// forwards.
struct Raw {
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u32,
}

impl Raw {
    fn new(server: &mut Server) -> Raw {
        Raw {
            stdin: server.child.stdin.take().unwrap(),
            lines: BufReader::new(server.child.stdout.take().unwrap()).lines(),
            id: 0,
        }
    }

    /// A request, and the id its answer will carry.
    async fn call(&mut self, method: &str, params: serde_json::Value) -> u32 {
        self.id += 1;
        let request = serde_json::json!({
            "jsonrpc": "2.0", "id": self.id, "method": method, "params": params
        });
        send(&mut self.stdin, &request.to_string()).await;
        self.id
    }

    async fn message(&mut self) -> serde_json::Value {
        let line = tokio::time::timeout(Duration::from_secs(20), self.lines.next_line())
            .await
            .expect("the server answers within the timeout")
            .unwrap()
            .expect("the server is still there");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("{e}: {line}"))
    }

    /// One request's result, over whatever notifications arrive first.
    async fn result(&mut self, id: u32) -> serde_json::Value {
        loop {
            let message = self.message().await;
            if message.get("id") == Some(&serde_json::json!(id)) {
                assert!(message["result"].is_object(), "{message}");
                return message["result"].clone();
            }
        }
    }

    /// Every frame the server sends until the root's turn ends.
    async fn frames_until_idle(&mut self, root: &str) -> Vec<Frame> {
        let mut frames = Vec::new();
        loop {
            let message = self.message().await;
            if message["method"] != "event" {
                continue;
            }
            let frame: Frame = serde_json::from_value(message["params"].clone())
                .unwrap_or_else(|e| panic!("{e}: {message}"));
            let idle = frame.session.as_str() == root
                && matches!(frame.event, Event::TurnCompleted { .. });
            frames.push(frame);
            if idle {
                return frames;
            }
        }
    }
}

/// The item a completed call of `name` left, and the text it returned.
fn tool_output(frames: &[Frame], name: &str) -> (bingo_sdk::ItemId, String) {
    frames
        .iter()
        .rev()
        .find_map(|frame| match &frame.event {
            Event::ItemCompleted { item } => match &item.body {
                bingo_sdk::ItemBody::ToolCall {
                    name: called,
                    output: Some(output),
                    ..
                } if called == name => Some((
                    item.id.clone(),
                    output
                        .parts
                        .iter()
                        .filter_map(bingo_sdk::ContentPart::as_text)
                        .collect(),
                )),
                _ => None,
            },
            _ => None,
        })
        .unwrap_or_else(|| panic!("no completed {name} call in {} frames", frames.len()))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_foreground_agent_is_a_child_session_on_the_root_s_attachment() {
    let mut server = Server::spawn(FOREGROUND_AGENT);
    let cwd = server.cwd();
    let mut raw = Raw::new(&mut server);
    let hello = raw
        .call(
            "initialize",
            serde_json::json!({"client": {"name": "harness", "surface": "test"}, "protocol": 1}),
        )
        .await;
    raw.result(hello).await;

    let opened = raw
        .call(
            "session/open",
            serde_json::json!({
                "selector": {"kind": "create", "spec": {"cwd": cwd}},
                "options": {"children": true}
            }),
        )
        .await;
    let root = raw.result(opened).await["session"]
        .as_str()
        .unwrap()
        .to_string();
    raw.call(
        "session/submit",
        serde_json::json!({
            "session": root,
            "intent": "req_01HARNESS0000000000000001",
            "input": {"kind": "text", "text": "spawn one", "origin": {"surface": "test"}}
        }),
    )
    .await;

    let frames = raw.frames_until_idle(&root).await;
    let (call, result) = tool_output(&frames, "SpawnAgent");
    assert!(
        result.contains("hi from the child"),
        "the child's own text is the call's result: {result}"
    );

    let child = frames
        .iter()
        .find_map(|frame| match &frame.event {
            Event::SessionUpdated { summary } if frame.session.as_str() != root => {
                summary.parent.clone().map(|link| (summary.clone(), link))
            }
            _ => None,
        })
        .expect("a child's own frames arrive on a tree attachment");
    assert_eq!(child.1.session.as_str(), root);
    assert_eq!(
        child.1.item.as_ref(),
        Some(&call),
        "the child hangs under the call that made it"
    );

    let listed = raw
        .call(
            "session/list",
            serde_json::json!({"filter": {"parent": root}}),
        )
        .await;
    let sessions = raw.result(listed).await["sessions"].clone();
    let sessions = sessions.as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(sessions[0]["id"], child.0.id.as_str());
    assert_eq!(sessions[0]["title"], "agent");

    let bye = raw.call("shutdown", serde_json::json!({})).await;
    raw.result(bye).await;
    assert!(server.child.wait().await.unwrap().success());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_background_agent_wakes_the_root_and_says_who_it_is() {
    let mut server = Server::spawn(BACKGROUND_AGENT);
    let kernel = ready(&mut server).await;
    let mut attachment = kernel
        .open(create(server.cwd()), who(), OpenOptions::with_children())
        .await
        .unwrap();
    let root = attachment.session.clone();
    attachment.handle.submit(
        IntentId::mint(),
        Input::text("spawn one", Origin::surface("test")),
    );

    // The root's own frames: the turn that spawns, then the turn the agent's
    // reply opens. A tree attachment carries the child's too, and they belong
    // to no state here.
    let mut turns = 0;
    let mut mine = Vec::new();
    let deadline = tokio::time::sleep(Duration::from_secs(30));
    tokio::pin!(deadline);
    while turns < 2 {
        tokio::select! {
            frame = attachment.events.next() => {
                let frame = frame.expect("the stream stays open");
                if frame.session != root {
                    continue;
                }
                if matches!(frame.event, Event::TurnCompleted { .. }) {
                    turns += 1;
                }
                attachment.snapshot.apply(&frame);
                mine.push(frame);
            }
            _ = &mut deadline => panic!("the agent never woke its parent: {turns} turns"),
        }
    }

    let (_, started) = tool_output(&mine, "SpawnAgent");
    let named: serde_json::Value = serde_json::from_str(&started).expect("a name and a session");
    assert_eq!(named["name"], "reviewer");

    // The root's second turn ran the agent's message rather than leaving it in
    // the queue. What is asserted is the item and the turn that carried it,
    // never the turn's `origin`: that says which door the turn came through —
    // `Peer` on an idle root, `Queue` on one still finishing its own round —
    // and the two race for the next scripted response. Who wrote the message
    // is `origin.principal` on the item, which does not move.
    let second = mine
        .iter()
        .filter_map(|frame| match &frame.event {
            Event::TurnStarted { turn, .. } => Some(turn.clone()),
            _ => None,
        })
        .nth(1)
        .expect("a second turn on the root");
    let (turn, origin, text) = attachment
        .snapshot
        .items
        .iter()
        .find_map(|item| match &item.body {
            bingo_sdk::ItemBody::User { parts, origin } if origin.principal.is_some() => Some((
                item.turn.clone(),
                origin.clone(),
                parts[0].as_text().unwrap_or_default().to_owned(),
            )),
            _ => None,
        })
        .expect("the agent's reply is a user item on the root");
    assert_eq!(origin.principal.as_deref(), Some("reviewer"));
    assert_eq!(origin.surface, "agent");
    assert_eq!(
        turn.as_ref(),
        Some(&second),
        "the message the agent sent is what the root's second turn ran"
    );
    assert!(text.starts_with("finished."), "{text}");
    assert!(
        attachment.snapshot.queue.is_empty(),
        "nothing was left waiting"
    );
    kernel.shutdown().await.unwrap();
}

/// A home whose settings put the fake model on `high` and declare that it
/// reasons at all: without the declaration every level is filtered out of
/// every request (ADR-0004) and the view would say `null` three times.
fn thinking_home() -> (tempfile::TempDir, String) {
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("settings.json");
    std::fs::write(
        &path,
        r#"{"thinking": "high", "models": {"fake/fake-1": {"reasoning": true}}}"#,
    )
    .unwrap();
    let settings = path.display().to_string();
    (home, settings)
}

/// The level the config view says this session's next turn will ask for: off
/// the snapshot when the cut already carried it, else the first
/// `ConfigChanged` after it.
async fn thinking_of(attachment: &mut Attachment) -> serde_json::Value {
    if !attachment.snapshot.config.kernel.is_null() {
        return attachment.snapshot.config.kernel["thinking"].clone();
    }
    let folded = async {
        while let Some(frame) = attachment.events.next().await {
            attachment.snapshot.apply(&frame);
            if matches!(frame.event, Event::ConfigChanged { .. }) {
                return attachment.snapshot.config.kernel["thinking"].clone();
            }
        }
        panic!("the stream ended before the config view said anything");
    };
    tokio::time::timeout(LIMIT, folded)
        .await
        .expect("the config view is published at start")
}

/// ADR-0047 §1 on the wire: `session/open` takes the level in the spec, with
/// no method change, and every client learns what the next turn will ask for
/// from the config view it already folds. The three answers are told apart —
/// absent inherits the settings, `null` is off, a word is that level.
#[tokio::test(flavor = "multi_thread")]
async fn session_open_takes_a_thinking_level_and_the_config_view_says_it() {
    let (home, settings) = thinking_home();
    let mut server = Server::spawn_at(home, TEXT_TURN, &["--settings", &settings]);
    let kernel = ready(&mut server).await;

    let asked = [
        (None, serde_json::json!("high")),
        (Some(None), serde_json::Value::Null),
        (Some(Some(Effort::Low)), serde_json::json!("low")),
    ];
    for (thinking, effective) in asked {
        let selector = SessionSelector::Create {
            spec: SessionSpec {
                cwd: server.cwd(),
                thinking,
                ..SessionSpec::default()
            },
        };
        let mut attachment = kernel
            .open(selector, who(), OpenOptions::default())
            .await
            .unwrap();
        assert_eq!(
            thinking_of(&mut attachment).await,
            effective,
            "a spec that says {thinking:?} runs at {effective}"
        );
    }
    kernel.shutdown().await.unwrap();
}

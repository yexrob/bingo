//! Black-box: the binary as a host drives it over JSON-RPC (ADR-0007). A
//! `RemoteKernel` talks to a spawned `bingo serve --stdio`; what it folds is
//! what a GUI would show.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::process::Stdio;
use std::time::Duration;

use bingo_sdk::{
    Activation, Answer, Attachment, CatalogKind, ClientIdentity, ErrorCode, Event, Frame,
    HistoryPage, HostApi, Input, IntentId, InterruptScope, Origin, SessionFilter, SessionId,
    SessionSelector, SessionSpec, SessionState, TurnStatus,
};
use bingo_surface_rpc::RemoteKernel;
use futures::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};

const TEXT_TURN: &str = r#"{"responses":[{"steps":[{"text":"Hello over the wire."}]}]}"#;

struct Server {
    child: Child,
    home: tempfile::TempDir,
}

impl Server {
    /// `bingo serve --stdio` in a fresh home, the fake provider on `script`.
    fn spawn(script: &str) -> Server {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("script.json");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_bingo"))
            .args(["serve", "--stdio", "--cwd"])
            .arg(home.path())
            .env("BINGO_FAKE_SCRIPT", &path)
            .env("HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Server { child, home }
    }

    fn kernel(&mut self) -> RemoteKernel {
        RemoteKernel::connect(
            self.child.stdout.take().unwrap(),
            self.child.stdin.take().unwrap(),
        )
    }

    fn cwd(&self) -> std::path::PathBuf {
        self.home.path().to_path_buf()
    }

    fn sessions_dir(&self) -> std::path::PathBuf {
        self.home.path().join(".bingo/data/sessions")
    }
}

async fn send(stdin: &mut tokio::process::ChildStdin, line: &str) {
    stdin
        .write_all(format!("{line}\n").as_bytes())
        .await
        .unwrap();
}

fn who() -> ClientIdentity {
    ClientIdentity {
        name: "harness".into(),
        surface: "test".into(),
    }
}

fn create(cwd: std::path::PathBuf) -> SessionSelector {
    SessionSelector::Create {
        spec: SessionSpec {
            cwd,
            ..SessionSpec::default()
        },
    }
}

/// Fold frames into `state` until the turn completes; the frames seen.
async fn until_completed(attachment: &mut Attachment) -> Vec<Frame> {
    let mut seen = Vec::new();
    let deadline = tokio::time::sleep(Duration::from_secs(20));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            frame = attachment.events.next() => {
                let frame = frame.expect("the stream stays open");
                attachment.snapshot.apply(&frame);
                let done = matches!(frame.event, Event::TurnCompleted { .. });
                seen.push(frame);
                if done {
                    return seen;
                }
            }
            _ = &mut deadline => panic!("the turn never completed: {:?}", seen.iter().map(|f| &f.event).collect::<Vec<_>>()),
        }
    }
}

async fn ready(server: &mut Server) -> RemoteKernel {
    let kernel = server.kernel();
    let hello = kernel.initialize(who()).await.unwrap();
    assert_eq!(hello.protocol, 1);
    kernel
}

#[tokio::test(flavor = "multi_thread")]
async fn a_method_before_initialize_is_refused() {
    let mut server = Server::spawn(TEXT_TURN);
    let kernel = server.kernel();
    let err = kernel
        .open(create(server.cwd()), who())
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
    let mut attachment = kernel.open(create(server.cwd()), who()).await.unwrap();
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
    let mut attachment = kernel.open(create(server.cwd()), who()).await.unwrap();
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
    assert!(matches!(
        frames.last().map(|f| &f.event),
        Some(Event::TurnCompleted {
            status: TurnStatus::Interrupted { .. },
            ..
        })
    ));
    kernel.shutdown().await.unwrap();
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
    let mut attachment = kernel.open(create(server.cwd()), who()).await.unwrap();
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
    let mut attachment = kernel.open(create(server.cwd()), who()).await.unwrap();
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
        .open(SessionSelector::ById { id: id.clone() }, who())
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
async fn the_catalogue_lists_the_fake_provider_and_the_tools() {
    let mut server = Server::spawn(TEXT_TURN);
    let kernel = ready(&mut server).await;
    let providers = kernel.catalog(CatalogKind::Providers).await.unwrap();
    assert!(
        providers.entries.iter().any(|e| e.id == "fake"),
        "{providers:?}"
    );
    let tools = kernel.catalog(CatalogKind::Tools).await.unwrap();
    assert!(tools.entries.iter().any(|e| e.id == "Read"), "{tools:?}");
    kernel.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_removes_the_session_from_disk_and_shutdown_exits_zero() {
    let mut server = Server::spawn(TEXT_TURN);
    let kernel = ready(&mut server).await;
    let mut attachment = kernel.open(create(server.cwd()), who()).await.unwrap();
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

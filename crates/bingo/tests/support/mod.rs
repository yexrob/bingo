//! What the JSON-RPC black-box tests share: a spawned `bingo serve --stdio`
//! with the fake provider on a script, and the folds a client does against it.
//! Every wait is bounded, so a scenario that stalls fails instead of hanging
//! the suite.

// An integration test is not `cfg(test)`; the test-only lint relief is spelled
// out. Each test binary uses a slice of this module, so what the other one
// uses is not dead.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::io::Write;
use std::process::Stdio;
use std::time::Duration;

use bingo_sdk::{
    Attachment, ClientIdentity, Event, Frame, IntentId, IntentOutcome, SessionSelector, SessionSpec,
};
use bingo_surface_rpc::RemoteKernel;
use futures::StreamExt;
use tokio::process::{Child, Command};

/// How long any one wait may take before the scenario is called stalled.
pub const LIMIT: Duration = Duration::from_secs(20);

pub struct Server {
    pub child: Child,
    home: tempfile::TempDir,
}

impl Server {
    /// `bingo serve --stdio` in a fresh home, the fake provider on `script`.
    pub fn spawn(script: &str) -> Server {
        Server::spawn_with(script, &[])
    }

    /// The same, with extra command-line arguments after `--cwd`.
    pub fn spawn_with(script: &str, extra: &[&str]) -> Server {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("script.json");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_bingo"))
            .args(["serve", "--stdio", "--cwd"])
            .arg(home.path())
            .args(extra)
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

    pub fn kernel(&mut self) -> RemoteKernel {
        RemoteKernel::connect(
            self.child.stdout.take().unwrap(),
            self.child.stdin.take().unwrap(),
        )
    }

    pub fn cwd(&self) -> std::path::PathBuf {
        self.home.path().to_path_buf()
    }

    pub fn sessions_dir(&self) -> std::path::PathBuf {
        self.home.path().join(".bingo/data/sessions")
    }
}

pub async fn send(stdin: &mut tokio::process::ChildStdin, line: &str) {
    use tokio::io::AsyncWriteExt;
    stdin
        .write_all(format!("{line}\n").as_bytes())
        .await
        .unwrap();
}

pub fn who() -> ClientIdentity {
    ClientIdentity {
        name: "harness".into(),
        surface: "test".into(),
    }
}

pub fn create(cwd: std::path::PathBuf) -> SessionSelector {
    SessionSelector::Create {
        spec: SessionSpec {
            cwd,
            ..SessionSpec::default()
        },
    }
}

/// A connected client that has said hello.
pub async fn ready(server: &mut Server) -> RemoteKernel {
    let kernel = server.kernel();
    let hello = kernel.initialize(who()).await.unwrap();
    assert_eq!(hello.protocol, 1);
    kernel
}

/// Fold frames into `state` until the turn completes; the frames seen.
pub async fn until_completed(attachment: &mut Attachment) -> Vec<Frame> {
    let mut seen = Vec::new();
    let deadline = tokio::time::sleep(LIMIT);
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

/// Fold frames until the ack for `intent`; the outcome.
pub async fn ack_for(attachment: &mut Attachment, intent: &IntentId) -> IntentOutcome {
    let deadline = tokio::time::sleep(LIMIT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            frame = attachment.events.next() => {
                let frame = frame.expect("the stream stays open");
                attachment.snapshot.apply(&frame);
                if let Event::IntentAck { intent: i, outcome } = frame.event
                    && &i == intent
                {
                    return outcome;
                }
            }
            _ = &mut deadline => panic!("no ack for {intent}"),
        }
    }
}

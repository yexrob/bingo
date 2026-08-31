//! Black-box: a plugin's three lanes (ADR-0013 §2) over the wire and through
//! the journal, with the demo plugin turned on by `--demo-ui`. A panel is
//! durable and comes back for the next run; a signal is on the live stream
//! and nowhere else; a button fires a command and the table changes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use bingo_sdk::{
    Action, Attachment, Event, HostApi, Input, IntentId, IntentOutcome, OpenOptions, Origin, Seq,
    SessionSelector, View,
};
use futures::StreamExt;

mod support;

use support::{LIMIT, Server, ack_for, create, ready, who};

/// One turn that calls the demo's tool, and one that says so.
const PROGRESS_TURN: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"DemoProgress","input":{"label":"cargo test"}}}]},
    {"steps":[{"text":"The bar has run."}]}
]}"#;

const NOTHING: &str = r#"{"responses":[{"steps":[{"text":"nothing to do"}]}]}"#;

fn intent(n: u8) -> IntentId {
    IntentId::from_raw(format!("req_01HVIEWS00000000000000000{n}"))
}

/// Submit and wait for the ack: a command is answered without a turn.
async fn command(attachment: &mut Attachment, n: u8, text: &str) -> IntentOutcome {
    let intent = intent(n);
    attachment
        .handle
        .submit(intent.clone(), Input::text(text, Origin::surface("test")));
    ack_for(attachment, &intent).await
}

/// Fire a button, which is what a surface does with a `View::Actions` item.
async fn fire(attachment: &mut Attachment, n: u8, name: &str) -> IntentOutcome {
    let intent = intent(n);
    attachment.handle.submit(
        intent.clone(),
        Input::Action {
            action: Action {
                name: name.into(),
                args: serde_json::Value::Null,
            },
        },
    );
    ack_for(attachment, &intent).await
}

/// The board as the session has it, read as the vocabulary it is published in.
fn board(attachment: &Attachment) -> View {
    let payload = attachment
        .snapshot
        .extensions
        .get("bingo.demo.ui")
        .and_then(|kinds| kinds.get("board"))
        .unwrap_or_else(|| panic!("no board in {:?}", attachment.snapshot.extensions));
    serde_json::from_value(payload.clone()).expect("a panel published in the vocabulary")
}

/// What the board's table says, row by row.
fn rows(view: &View) -> Vec<Vec<String>> {
    let View::Panel { child, .. } = view else {
        panic!("a board is a panel: {view:?}");
    };
    let View::Stack { children } = child.as_ref() else {
        panic!("a table and its buttons");
    };
    children
        .iter()
        .find_map(|child| match child {
            View::Table { rows, .. } => Some(rows.clone()),
            _ => None,
        })
        .expect("a table")
}

/// The names of the buttons the board offers.
fn buttons(view: &View) -> Vec<String> {
    let View::Panel { child, .. } = view else {
        panic!("a board is a panel");
    };
    let View::Stack { children } = child.as_ref() else {
        panic!("a table and its buttons");
    };
    children
        .iter()
        .find_map(|child| match child {
            View::Actions { items } => {
                Some(items.iter().map(|item| item.action.name.clone()).collect())
            }
            _ => None,
        })
        .expect("buttons")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_button_fires_its_command_and_the_table_changes() {
    let mut server = Server::spawn_with(NOTHING, &["--demo-ui"]);
    let kernel = ready(&mut server).await;
    let mut attachment = kernel
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();

    let published = command(&mut attachment, 1, "/board").await;
    assert!(
        matches!(published, IntentOutcome::Applied { .. }),
        "{published:?}"
    );
    let first = board(&attachment);
    assert_eq!(
        buttons(&first),
        ["board.tick", "board.reset"],
        "a button names the command a surface fires"
    );
    assert_eq!(rows(&first)[0][2], "pending");

    let ticked = fire(&mut attachment, 2, "board.tick").await;
    assert!(
        matches!(ticked, IntentOutcome::Applied { .. }),
        "{ticked:?}"
    );
    assert_eq!(
        rows(&board(&attachment))[0][2],
        "running",
        "the row the button moved on"
    );

    kernel.shutdown().await.unwrap();
}

/// The one lane that is not durable: a signal reaches a client and is left
/// out of the journal the next run replays.
#[tokio::test(flavor = "multi_thread")]
async fn a_signal_is_on_the_live_stream_and_never_in_the_journal() {
    let mut server = Server::spawn_with(PROGRESS_TURN, &["--demo-ui"]);
    let kernel = ready(&mut server).await;
    let mut attachment = kernel
        .open(create(server.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    command(&mut attachment, 1, "/board").await;

    let intent = intent(2);
    attachment.handle.submit(
        intent.clone(),
        Input::text("run the bar", Origin::surface("test")),
    );
    let live = until_completed(&mut attachment).await;

    let bars: Vec<&Event> = live
        .iter()
        .map(|frame| &frame.event)
        .filter(|event| matches!(event, Event::Signal { kind, .. } if kind == "progress"))
        .collect();
    assert!(bars.len() > 1, "the bar moved: {} frames", bars.len());
    assert!(
        attachment.snapshot.signals.contains_key("bingo.demo.ui"),
        "and the latest frame is what a client shows"
    );

    let journal = replayed(&attachment).await;
    assert!(
        !journal
            .iter()
            .any(|event| matches!(event, Event::Signal { .. })),
        "a signal is never journaled"
    );
    assert!(
        journal.iter().any(
            |event| matches!(event, Event::Extension { plugin, .. } if plugin == "bingo.demo.ui")
        ),
        "and the panel beside it is"
    );

    kernel.shutdown().await.unwrap();
}

/// What `--continue` finds: the board the last run left, and no trace of the
/// bar that was moving while it ran.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_run_reads_the_board_back_and_not_the_bar() {
    let mut first = Server::spawn_with(PROGRESS_TURN, &["--demo-ui"]);
    let kernel = ready(&mut first).await;
    let mut attachment = kernel
        .open(create(first.cwd()), who(), OpenOptions::default())
        .await
        .unwrap();
    command(&mut attachment, 1, "/board").await;
    fire(&mut attachment, 2, "board.tick").await;
    let intent = intent(3);
    attachment.handle.submit(
        intent.clone(),
        Input::text("run the bar", Origin::surface("test")),
    );
    until_completed(&mut attachment).await;
    assert_eq!(rows(&board(&attachment))[0][2], "running");
    kernel.shutdown().await.unwrap();
    drop(attachment);

    let home = first.into_home();
    let cwd = home.path().to_path_buf();
    let mut second = Server::spawn_at(home, NOTHING, &["--demo-ui"]);
    let kernel = ready(&mut second).await;
    let mut attachment = kernel
        .open(
            SessionSelector::Latest { cwd },
            who(),
            OpenOptions::default(),
        )
        .await
        .unwrap();

    assert_eq!(
        rows(&board(&attachment))[0][2],
        "running",
        "the board is what the last run left"
    );
    assert!(
        attachment.snapshot.signals.is_empty(),
        "and the bar is gone: {:?}",
        attachment.snapshot.signals
    );

    // A second tick moves the same row on, which it could not do if the
    // journal had handed this run a fresh board.
    fire(&mut attachment, 4, "board.tick").await;
    assert_eq!(rows(&board(&attachment))[0][2], "done");

    kernel.shutdown().await.unwrap();
}

/// What a client that reconnects is replayed: every frame the journal holds,
/// and then the live tail, which goes quiet. The replay is read until it
/// does, because `events_since` never ends on its own.
async fn replayed(attachment: &Attachment) -> Vec<Event> {
    let mut stream = attachment.handle.events_since(Seq::ZERO).await.unwrap();
    let mut out = Vec::new();
    while let Ok(Some(frame)) = tokio::time::timeout(QUIET, stream.next()).await {
        out.push(frame.event);
    }
    out
}

/// How long a replayed stream must say nothing before it is called done.
const QUIET: Duration = Duration::from_millis(500);

/// Fold frames until the turn completes; the frames seen. The demo's bar runs
/// for three seconds, so this waits longer than a text turn would.
async fn until_completed(attachment: &mut Attachment) -> Vec<bingo_sdk::Frame> {
    let mut seen = Vec::new();
    let deadline = tokio::time::sleep(LIMIT + Duration::from_secs(10));
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
            _ = &mut deadline => panic!("the turn never completed"),
        }
    }
}

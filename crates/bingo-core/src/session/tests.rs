use std::sync::Arc;

use futures::{FutureExt, StreamExt};
use serde_json::json;

use super::*;
use crate::test_support::*;

mod commands;
mod images;
mod invoke;
mod log;
mod naming;
mod peers;

fn who() -> ClientIdentity {
    ClientIdentity {
        name: "test".into(),
        surface: "test".into(),
    }
}

fn start(provider: Arc<ScriptedProvider>, tools: Vec<Arc<dyn Tool>>) -> Mailbox {
    spawn(summary("ses_1"), None, Services::none(), |_| {
        Arc::new(config(provider, tools, Arc::new(NoHost)))
    })
}

/// Fold frames until `stop` says so; returns the labels seen and the state.
async fn drive(
    events: &mut FrameStream,
    state: &mut SessionState,
    mut stop: impl FnMut(&Frame) -> bool,
) -> Vec<String> {
    let mut labels = Vec::new();
    while let Some(frame) = events.next().await {
        state.apply(&frame);
        labels.push(label(&frame.event));
        if stop(&frame) {
            break;
        }
    }
    labels
}

fn turn_completed(frame: &Frame) -> bool {
    matches!(frame.event, Event::TurnCompleted { .. })
}

/// A peer's text, as `deliver` carries it.
fn peer(text: &str, from: &str) -> Input {
    Input::text(
        text,
        Origin {
            surface: "agent".into(),
            principal: Some(from.into()),
            conversation: None,
        },
    )
}

/// Fold frames until `stop` says so; returns the frames seen.
async fn frames_until(
    events: &mut FrameStream,
    state: &mut SessionState,
    mut stop: impl FnMut(&Frame) -> bool,
) -> Vec<Frame> {
    let mut frames = Vec::new();
    while let Some(frame) = events.next().await {
        state.apply(&frame);
        let done = stop(&frame);
        frames.push(frame);
        if done {
            break;
        }
    }
    frames
}

fn turn_origin(frames: &[Frame]) -> Option<TurnOrigin> {
    frames.iter().find_map(|f| match &f.event {
        Event::TurnStarted { origin, .. } => Some(*origin),
        _ => None,
    })
}

fn user_texts(state: &SessionState) -> Vec<String> {
    state
        .items
        .iter()
        .filter_map(|i| match &i.body {
            ItemBody::User { parts, .. } => parts[0].as_text().map(str::to_string),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn submit_starts_a_turn_and_streams_it_to_the_end() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("hello"))]);
    let mailbox = start(provider, vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    assert_eq!(
        state.seq,
        Seq(2),
        "the journal head is the summary frame, then the config"
    );

    let intent = IntentId::mint();
    mailbox.submit(intent.clone(), Input::text("hi", Origin::surface("test")));
    let labels = drive(&mut events, &mut state, turn_completed).await;
    assert_eq!(
        labels,
        vec![
            "completed:user/completed",
            // The first ask names a session nobody named; the mint rides one
            // frame of its own and is never sent again.
            "SessionUpdated",
            "turnStarted",
            "ack:TurnStarted",
            "started:assistant/running",
            "delta",
            "completed:assistant/completed",
            "usage",
            "turnCompleted:Completed",
        ]
    );
    assert!(!state.busy());
    assert_eq!(state.last_turn, Some(TurnStatus::Completed));
    let user = &state.items[0];
    assert_eq!(user.intent.as_ref(), Some(&intent));
    assert_eq!(
        user.turn, state.items[1].turn,
        "inputs belong to the turn they opened"
    );
    assert_eq!(
        state.items[1].body,
        ItemBody::Assistant {
            text: "hello".into()
        }
    );

    let replay = events_of(&mailbox).await;
    assert!(
        replay.iter().all(|f| f.event.is_durable()),
        "replay is the durable journal only"
    );
    let mut folded = SessionState::new(summary("ses_1"));
    for frame in &replay {
        folded.apply(frame);
    }
    assert_eq!(
        folded.items, state.items,
        "the same reducer over the journal gives the same view"
    );
}

#[tokio::test]
async fn a_busy_session_queues_and_the_queue_opens_the_next_turn() {
    let provider = ScriptedProvider::new(vec![
        Script::Hang(vec![ModelEvent::TextStart { id: "b".into() }]),
        Script::Events(text("second")),
    ]);
    let mailbox = start(provider, vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();

    mailbox.submit(
        IntentId::mint(),
        Input::text("first", Origin::surface("test")),
    );
    drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::ItemStarted { .. })
    })
    .await;
    assert!(state.busy());

    let queued = IntentId::mint();
    mailbox.submit(
        queued.clone(),
        Input::text("second", Origin::surface("test")),
    );
    let labels = drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::IntentAck { .. })
    })
    .await;
    assert_eq!(labels, vec!["queue:1", "ack:Queued"]);
    assert_eq!(state.queue[0].intent, queued);
    assert_eq!(state.queue[0].preview, "second");

    mailbox.interrupt(IntentId::mint(), InterruptScope::Head);
    let labels = drive(&mut events, &mut state, turn_completed).await;
    assert!(labels.contains(&"ack:Applied".to_string()));
    assert!(
        matches!(state.last_turn, Some(TurnStatus::Interrupted { .. })),
        "{:?}",
        state.last_turn
    );

    let labels = drive(&mut events, &mut state, turn_completed).await;
    assert_eq!(
        &labels[..4],
        [
            "completed:user/completed",
            "turnStarted",
            "ack:TurnStarted",
            "queue:0",
        ],
        "the queued intent learns its turn, and the queue empties only once that turn is open"
    );
    assert_eq!(
        labels.last().map(String::as_str),
        Some("turnCompleted:Completed")
    );
    let started = state
        .items
        .iter()
        .find(|i| i.intent.as_ref() == Some(&queued))
        .unwrap();
    assert!(started.turn.is_some());
    assert_eq!(
        state.items.last().unwrap().body,
        ItemBody::Assistant {
            text: "second".into()
        }
    );
}

#[tokio::test]
async fn a_permission_is_answered_once_and_late_answers_are_rejected() {
    let provider = ScriptedProvider::new(vec![
        Script::Events(tool_call("Echo", json!({"v": 1}))),
        Script::Events(text("done")),
    ]);
    let mailbox = start(provider, vec![Arc::new(EchoTool { read_only: false })]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();

    mailbox.submit(IntentId::mint(), Input::text("go", Origin::surface("test")));
    drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::InteractionOpened { .. })
    })
    .await;
    let interaction = state.interactions[0].clone();
    assert!(
        matches!(interaction.kind, InteractionKind::Permission { ref tool, .. } if tool == "Echo")
    );
    assert!(interaction.answers.contains(&AnswerSpec::AllowOnce));
    assert!(state.attention());

    // Not an accepted answer here.
    let bad = IntentId::mint();
    mailbox.answer(
        bad.clone(),
        interaction.id.clone(),
        Answer::Confirm,
        Activation::Programmatic,
        who(),
    );
    drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::IntentAck { .. })
    })
    .await;
    assert!(matches!(
        state_ack(&events_of(&mailbox).await, &bad),
        Some(IntentOutcome::Rejected { error }) if error.code == ErrorCode::InvalidInput
    ));

    let ok = IntentId::mint();
    mailbox.answer(
        ok.clone(),
        interaction.id.clone(),
        Answer::AllowOnce,
        Activation::Programmatic,
        who(),
    );
    let labels = drive(&mut events, &mut state, turn_completed).await;
    assert_eq!(labels[0], "interactionResolved");
    assert_eq!(labels[1], "ack:Applied");
    assert!(state.interactions.is_empty());
    assert_eq!(state.last_turn, Some(TurnStatus::Completed));
    let tool = state
        .items
        .iter()
        .find(|i| matches!(i.body, ItemBody::ToolCall { .. }))
        .unwrap();
    assert_eq!(tool.status, ItemStatus::Completed);

    let late = IntentId::mint();
    mailbox.answer(
        late.clone(),
        interaction.id.clone(),
        Answer::AllowOnce,
        Activation::Programmatic,
        who(),
    );
    drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::IntentAck { .. })
    })
    .await;
    assert!(matches!(
        state_ack(&events_of(&mailbox).await, &late),
        Some(IntentOutcome::Rejected { error }) if error.code == ErrorCode::InteractionClosed
    ));
}

/// The durable journal, read back through `events_since`. The replay is
/// ready at once; the first pending live frame ends the read.
async fn events_of(mailbox: &Mailbox) -> Vec<Frame> {
    let mut stream = mailbox.events_since(Seq::ZERO).await.unwrap();
    let mut out = Vec::new();
    while let Some(Some(frame)) = stream.next().now_or_never() {
        out.push(frame);
    }
    out
}

fn state_ack(frames: &[Frame], intent: &IntentId) -> Option<IntentOutcome> {
    frames.iter().find_map(|f| match &f.event {
        Event::IntentAck { intent: i, outcome } if i == intent => Some(outcome.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn interrupting_an_idle_session_is_rejected() {
    let mailbox = start(ScriptedProvider::new(vec![]), vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    let intent = IntentId::mint();
    mailbox.interrupt(intent.clone(), InterruptScope::Head);
    drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::IntentAck { .. })
    })
    .await;
    let frames = events_of(&mailbox).await;
    assert!(matches!(
        state_ack(&frames, &intent),
        Some(IntentOutcome::Rejected { error }) if error.code == ErrorCode::NotReady
    ));
}

#[tokio::test]
async fn an_unknown_command_an_unknown_action_and_empty_text_are_rejected() {
    let mailbox = start(ScriptedProvider::new(vec![]), vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    let inputs = vec![
        Input::text("/help", Origin::surface("test")),
        Input::text("   ", Origin::surface("test")),
        Input::Action {
            action: Action {
                name: "x".into(),
                args: Value::Null,
            },
        },
    ];
    for input in inputs {
        let intent = IntentId::mint();
        mailbox.submit(intent.clone(), input);
        let labels = drive(&mut events, &mut state, |f| {
            matches!(f.event, Event::IntentAck { .. })
        })
        .await;
        assert_eq!(labels, vec!["ack:Rejected"]);
    }
    assert!(!state.busy());
    assert!(state.items.is_empty());
}

#[tokio::test]
async fn a_lagging_subscriber_is_told_and_can_resync() {
    let mailbox = start(ScriptedProvider::new(vec![]), vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    let total = SUBSCRIBER_CAPACITY + 50;
    for i in 0..total {
        mailbox
            .record(ItemBody::Notice {
                level: Level::Info,
                code: "n".into(),
                text: i.to_string(),
            })
            .await
            .unwrap();
    }
    let mut lagged = None;
    while let Some(frame) = events.next().await {
        let applied = state.apply(&frame);
        if applied == Applied::Lagged {
            lagged = Some(frame);
            break;
        }
    }
    let lagged = lagged.expect("a marker after the channel filled");
    let Event::Lagged { from, to } = lagged.event else {
        unreachable!()
    };
    assert!(from > state.seq && to > from);
    assert!(state.items.len() < total);
    assert!(
        events.next().await.is_none(),
        "a lagged stream ends at its marker"
    );

    let mut resync = mailbox.events_since(state.seq).await.unwrap();
    while state.items.len() < total {
        let frame = resync.next().await.unwrap();
        assert_ne!(state.apply(&frame), Applied::Stale);
    }
    assert_eq!(state.items.len(), total);
    let texts: Vec<String> = state
        .items
        .iter()
        .map(|i| match &i.body {
            ItemBody::Notice { text, .. } => text.clone(),
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(texts, (0..total).map(|i| i.to_string()).collect::<Vec<_>>());
}

#[tokio::test]
async fn closing_cancels_the_turn_and_ends_the_journal() {
    let provider = ScriptedProvider::new(vec![Script::Hang(vec![])]);
    let mailbox = start(provider, vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(
        IntentId::mint(),
        Input::text("first", Origin::surface("test")),
    );
    drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::TurnStarted { .. })
    })
    .await;
    let queued = IntentId::mint();
    mailbox.submit(
        queued.clone(),
        Input::text("second", Origin::surface("test")),
    );

    mailbox.close(CloseReason::Client);
    let labels = drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::SessionClosed { .. })
    })
    .await;
    assert!(labels.contains(&"ack:Rejected".to_string()), "{labels:?}");
    assert!(matches!(
        state.last_turn,
        Some(TurnStatus::Interrupted { .. })
    ));
    assert!(state.closed);
    assert!(
        events.next().await.is_none(),
        "nothing follows SessionClosed"
    );
    assert_eq!(
        mailbox.attach().await.err().map(|e| e.code),
        Some(ErrorCode::SessionClosed)
    );
}

#[tokio::test]
async fn a_panicking_turn_is_reported_as_lost_not_hung() {
    let provider = ScriptedProvider::new(vec![Script::Events(tool_call("Panic", json!({})))]);
    let mailbox = start(provider, vec![Arc::new(PanicTool)]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(
        IntentId::mint(),
        Input::text("boom", Origin::surface("test")),
    );
    drive(&mut events, &mut state, turn_completed).await;
    assert!(matches!(
        &state.last_turn,
        Some(TurnStatus::Failed { error }) if error.code == ErrorCode::TurnLost && error.message.contains("tool exploded")
    ));
    assert!(!state.busy());
    // The session is still usable.
    let intent = IntentId::mint();
    mailbox.interrupt(intent, InterruptScope::Head);
    let labels = drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::IntentAck { .. })
    })
    .await;
    assert_eq!(labels, vec!["ack:Rejected"]);
}

#[tokio::test]
async fn history_pages_backwards_from_the_newest_item() {
    let mailbox = start(ScriptedProvider::new(vec![]), vec![]);
    for i in 0..5 {
        mailbox
            .record(ItemBody::Notice {
                level: Level::Info,
                code: "n".into(),
                text: i.to_string(),
            })
            .await
            .unwrap();
    }
    let page = mailbox
        .history(HistoryPage {
            before: None,
            limit: 2,
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 2);
    let next = page.next.clone().expect("older items remain");
    assert_eq!(next, page.items[0].id);
    let page = mailbox
        .history(HistoryPage {
            before: Some(next),
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(page.items.len(), 3);
    assert!(page.next.is_none());
    let all = mailbox.history(HistoryPage::default()).await.unwrap();
    assert_eq!(all.items.len(), 5);
}

#[tokio::test]
async fn a_journal_cut_inside_a_turn_resumes_with_that_turn_lost() {
    let head = summary("ses_1");
    let ts = jiff::Timestamp::from_second(0).unwrap();
    let frame = |seq: u64, event: Event| Frame {
        seq: Seq(seq),
        ts,
        session: SessionId::from_raw("ses_1"),
        cause: None,
        event,
    };
    let frames = vec![
        frame(
            1,
            Event::SessionUpdated {
                summary: head.clone(),
            },
        ),
        frame(
            2,
            Event::TurnStarted {
                turn: TurnId::from_raw("trn_old"),
                inputs: vec![],
                origin: TurnOrigin::Submit,
            },
        ),
    ];
    let provider = ScriptedProvider::new(vec![Script::Events(text("back"))]);
    let mailbox = resume(frames, None, Services::none(), |_| {
        Arc::new(config(provider, vec![], Arc::new(NoHost)))
    })
    .unwrap();
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    assert_eq!(
        state.seq,
        Seq(5),
        "a new head and its config, then the old turn closed"
    );
    assert!(state.turn.is_none() && !state.busy());
    assert!(
        matches!(&state.last_turn, Some(TurnStatus::Failed { error }) if error.code == ErrorCode::TurnLost),
        "{:?}",
        state.last_turn
    );

    mailbox.submit(IntentId::mint(), Input::text("hi", Origin::surface("test")));
    let labels = drive(&mut events, &mut state, turn_completed).await;
    assert_eq!(
        labels.last().map(String::as_str),
        Some("turnCompleted:Completed")
    );
    assert!(state.seq > Seq(4), "the seq goes on from the journal");
}

#[test]
fn a_journal_without_its_head_cannot_be_resumed() {
    let err = head_summary(&[]).unwrap_err();
    assert_eq!(err.code, ErrorCode::Storage);
}

#[tokio::test]
async fn a_journal_that_ends_closed_resumes_open() {
    let head = summary("ses_1");
    let ts = jiff::Timestamp::from_second(0).unwrap();
    let frames = vec![
        Frame {
            seq: Seq(1),
            ts,
            session: SessionId::from_raw("ses_1"),
            cause: None,
            event: Event::SessionUpdated {
                summary: head.clone(),
            },
        },
        Frame {
            seq: Seq(2),
            ts,
            session: SessionId::from_raw("ses_1"),
            cause: None,
            event: Event::SessionClosed {
                reason: CloseReason::Shutdown,
            },
        },
    ];
    let provider = ScriptedProvider::new(vec![Script::Events(text("open again"))]);
    let mailbox = resume(frames, None, Services::none(), |_| {
        Arc::new(config(provider, vec![], Arc::new(NoHost)))
    })
    .unwrap();
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    assert!(!state.closed);
    mailbox.submit(IntentId::mint(), Input::text("hi", Origin::surface("test")));
    let labels = drive(&mut events, &mut state, turn_completed).await;
    assert_eq!(
        labels.last().map(String::as_str),
        Some("turnCompleted:Completed")
    );
}

/// A start hook that takes its time, and says when it was done.
struct Slow(std::sync::Mutex<Option<std::time::Instant>>);

#[async_trait::async_trait]
impl Hook for Slow {
    fn id(&self) -> &str {
        "slow"
    }
    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::Session],
            tool: None,
        }
    }
    async fn on_session(&self, phase: Phase, _: &HookContext) {
        if phase == Phase::Start {
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            *self.0.lock().unwrap() = Some(std::time::Instant::now());
        }
    }
}

/// What a start hook seats or injects is there before the first message is
/// read: the hook is awaited, not spawned beside the session.
#[tokio::test]
async fn a_start_hook_finishes_before_the_first_turn_opens() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("hello"))]);
    let slow = Arc::new(Slow(std::sync::Mutex::new(None)));
    let hook = slow.clone();
    let mailbox = spawn(summary("ses_1"), None, Services::none(), move |_| {
        let mut cfg = config(provider, vec![], Arc::new(NoHost));
        cfg.hooks = HookSet::fixed(vec![hook as Arc<dyn Hook>]);
        Arc::new(cfg)
    });
    mailbox.submit(IntentId::mint(), Input::text("hi", Origin::surface("test")));
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    let opened_at = std::time::Instant::now();
    if state.turn.is_none() {
        frames_until(&mut events, &mut state, |f| {
            matches!(f.event, Event::TurnStarted { .. })
        })
        .await;
    }
    let done = slow
        .0
        .lock()
        .unwrap()
        .expect("the hook ran to its end first");
    assert!(
        done <= opened_at || state.turn.is_some(),
        "the turn waited for the hook"
    );
}

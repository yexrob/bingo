//! A `Log` session (ADR-0011 §1): nothing answers, and everything it is told
//! is the journal's at once. And the journal as a plugin's state (§2).

use super::*;

fn log_session() -> Mailbox {
    let provider = ScriptedProvider::new(vec![]);
    let mut summary = summary("ses_log");
    summary.driver = Driver::Log;
    spawn(summary, None, Services::none(), |_| {
        let mut cfg = config(provider, vec![], Arc::new(NoHost));
        cfg.model = None;
        Arc::new(cfg)
    })
}

fn acked(frame: &Frame) -> bool {
    matches!(frame.event, Event::IntentAck { .. })
}

#[tokio::test]
async fn a_submit_is_recorded_at_once_and_opens_no_turn() {
    let mailbox = log_session();
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    let intent = IntentId::mint();
    mailbox.submit(
        intent.clone(),
        Input::text("hello, everyone", Origin::surface("tui")),
    );
    let frames = frames_until(&mut events, &mut state, acked).await;
    assert!(matches!(
        &frames[0].event,
        Event::ItemCompleted { item }
            if matches!(item.body, ItemBody::User { .. }) && item.turn.is_none()
    ));
    assert!(matches!(
        &frames[1].event,
        Event::IntentAck { intent: i, outcome: IntentOutcome::Applied { result } }
            if i == &intent && result.get("item").is_some()
    ));
    assert!(state.turn.is_none(), "nothing answers");
    assert!(state.queue.is_empty(), "nothing waits");
    assert!(!state.busy());
}

#[tokio::test]
async fn a_delivery_of_either_kind_is_recorded_the_same_way() {
    let mailbox = log_session();
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.deliver(IntentId::mint(), peer("held", "a"), Delivery::Hold);
    mailbox.deliver(IntentId::mint(), peer("woken", "b"), Delivery::Wake);
    let mut acks = 0;
    frames_until(&mut events, &mut state, |f| {
        acks += usize::from(acked(f));
        acks == 2
    })
    .await;
    assert_eq!(user_texts(&state), ["held", "woken"]);
    assert!(state.queue.is_empty() && state.turn.is_none());
    let ItemBody::User { origin, .. } = &state.items[0].body else {
        panic!("a user item first");
    };
    assert_eq!(origin.principal.as_deref(), Some("a"), "who spoke is kept");
}

#[tokio::test]
async fn nothing_compacts_and_nothing_is_there_to_interrupt() {
    let mailbox = log_session();
    let err = mailbox
        .compact(None)
        .await
        .expect_err("a log has nothing to compact");
    assert_eq!(err.code, ErrorCode::InvalidInput);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.interrupt(IntentId::mint(), InterruptScope::Head);
    let frames = frames_until(&mut events, &mut state, acked).await;
    assert!(matches!(
        &frames[0].event,
        Event::IntentAck { outcome: IntentOutcome::Rejected { error }, .. }
            if error.code == ErrorCode::NotReady
    ));
}

#[tokio::test]
async fn an_extension_is_durable_folded_and_the_latest_payload_is_the_state() {
    let mailbox = log_session();
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.extend("bingo.test".into(), "things".into(), json!({ "n": 1 }));
    mailbox.extend("bingo.test".into(), "things".into(), json!({ "n": 2 }));
    let mut seen = 0;
    frames_until(&mut events, &mut state, |f| {
        seen += usize::from(matches!(f.event, Event::Extension { .. }));
        seen == 2
    })
    .await;
    assert_eq!(state.extensions["bingo.test"]["things"], json!({ "n": 2 }));

    let (snapshot, _) = mailbox.attach().await.unwrap();
    assert_eq!(
        snapshot.extensions["bingo.test"]["things"],
        json!({ "n": 2 }),
        "a later attachment folds it from the head"
    );
    let mut replay = mailbox.events_since(Seq::ZERO).await.unwrap();
    let mut journaled = 0;
    while let Some(Some(frame)) = replay.next().now_or_never() {
        journaled += usize::from(matches!(frame.event, Event::Extension { .. }));
    }
    assert_eq!(journaled, 2, "durable: both are in the journal");
}

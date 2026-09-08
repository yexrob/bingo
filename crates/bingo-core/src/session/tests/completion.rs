//! Exactly one `TurnCompleted` closes a turn, however that turn ends.
//!
//! The kernel says it at two sites — `recover`, for a turn the last process
//! left open, and `turn_finished`, for one this process ran — and the two are
//! guarded by different facts. A second one would fold into a client that has
//! already put the turn away: `last_turn` would be overwritten, `unread` set
//! again, and a surface would draw a turn ending twice. So the journal is read
//! back and the turns it opened are compared with the turns it closed.

use super::*;

fn opened(frames: &[Frame]) -> Vec<TurnId> {
    frames
        .iter()
        .filter_map(|f| match &f.event {
            Event::TurnStarted { turn, .. } => Some(turn.clone()),
            _ => None,
        })
        .collect()
}

fn closed(frames: &[Frame]) -> Vec<TurnId> {
    frames
        .iter()
        .filter_map(|f| match &f.event {
            Event::TurnCompleted { turn, .. } => Some(turn.clone()),
            _ => None,
        })
        .collect()
}

/// Every turn the journal opened was closed once, in the order it opened.
async fn assert_closed_once(mailbox: &Mailbox, turns: usize) {
    let frames = events_of(mailbox).await;
    let opened = opened(&frames);
    assert_eq!(opened.len(), turns, "{opened:?}");
    assert_eq!(
        closed(&frames),
        opened,
        "one `TurnCompleted` per turn, and no other"
    );
}

#[tokio::test]
async fn a_turn_that_runs_to_its_end_is_completed_once() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("hello"))]);
    let mailbox = start(provider, vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(IntentId::mint(), Input::text("hi", Origin::surface("test")));
    drive(&mut events, &mut state, turn_completed).await;
    assert_eq!(state.last_status(), Some(&TurnStatus::Completed));
    assert_closed_once(&mailbox, 1).await;
}

#[tokio::test]
async fn an_interrupted_turn_is_completed_once() {
    let provider = ScriptedProvider::new(vec![Script::Hang(vec![])]);
    let mailbox = start(provider, vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(IntentId::mint(), Input::text("go", Origin::surface("test")));
    drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::TurnStarted { .. })
    })
    .await;

    mailbox.interrupt(IntentId::mint(), InterruptScope::Head);
    drive(&mut events, &mut state, turn_completed).await;
    assert!(matches!(
        state.last_status(),
        Some(TurnStatus::Interrupted { .. })
    ));
    assert_closed_once(&mailbox, 1).await;
}

#[tokio::test]
async fn a_turn_whose_tool_panics_is_completed_once() {
    let provider = ScriptedProvider::new(vec![Script::Events(tool_call("Panic", json!({})))]);
    let mailbox = start(provider, vec![Arc::new(PanicTool)]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(
        IntentId::mint(),
        Input::text("boom", Origin::surface("test")),
    );
    drive(&mut events, &mut state, turn_completed).await;
    assert!(matches!(
        state.last_status(),
        Some(TurnStatus::Failed { error }) if error.code == ErrorCode::TurnLost
    ));
    assert_closed_once(&mailbox, 1).await;
}

#[tokio::test]
async fn a_turn_whose_provider_panics_is_completed_once() {
    let mailbox = start(ScriptedProvider::new(vec![Script::Panic]), vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(
        IntentId::mint(),
        Input::text("boom", Origin::surface("test")),
    );
    drive(&mut events, &mut state, turn_completed).await;
    assert!(matches!(
        state.last_status(),
        Some(TurnStatus::Failed { error }) if error.code == ErrorCode::TurnLost
    ));
    assert_closed_once(&mailbox, 1).await;
}

/// The recovered turn is closed by `recover`, the new one by `turn_finished`:
/// the two sites in one journal, one `TurnCompleted` each.
#[tokio::test]
async fn a_recovered_turn_and_the_turn_after_it_are_each_completed_once() {
    let frames = vec![
        journal_frame(
            1,
            Event::SessionUpdated {
                summary: summary("ses_1"),
            },
        ),
        journal_frame(
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
    assert!(
        state.turn.is_none(),
        "the lost turn was closed on the way in"
    );

    mailbox.submit(IntentId::mint(), Input::text("hi", Origin::surface("test")));
    drive(&mut events, &mut state, turn_completed).await;
    assert_closed_once(&mailbox, 2).await;
}

/// A close cancels the running turn, and the turn it cancels is closed by the
/// one site that closes any other — not again by the close itself.
#[tokio::test]
async fn a_turn_a_close_cancels_is_completed_once() {
    let provider = ScriptedProvider::new(vec![Script::Hang(vec![])]);
    let mailbox = start(provider, vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(IntentId::mint(), Input::text("go", Origin::surface("test")));
    let frames = frames_until(&mut events, &mut state, |f| {
        matches!(f.event, Event::TurnStarted { .. })
    })
    .await;
    let turn = opened(&frames);

    mailbox.close(CloseReason::Client);
    let frames = frames_until(&mut events, &mut state, |f| {
        matches!(f.event, Event::SessionClosed { .. })
    })
    .await;
    assert_eq!(closed(&frames), turn, "the cancelled turn, closed once");
}

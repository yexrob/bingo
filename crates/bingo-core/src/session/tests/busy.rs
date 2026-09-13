//! `SessionSummary::busy` is derived, once, by the actor that owns the turn.
//!
//! A summary travels where no `SessionState` goes — a host listing, a stored
//! `summary.json` — so the field has to be on the wire; the reducer writes it
//! nowhere, so nothing can drift from what `SessionState::busy` says.

use super::*;

fn summaries(frames: &[Frame]) -> Vec<SessionSummary> {
    frames
        .iter()
        .filter_map(|f| match &f.event {
            Event::SessionUpdated { summary } => Some(summary.clone()),
            _ => None,
        })
        .collect()
}

fn other_config() -> Arc<crate::turn::TurnConfig> {
    Arc::new(config(
        ScriptedProvider::new(vec![]),
        vec![],
        Arc::new(NoHost),
    ))
}

/// A reconfigure publishes the summary whenever it lands, so it is the one
/// publish that can fall on either side of a turn.
async fn restated(mailbox: &Mailbox, events: &mut FrameStream, state: &mut SessionState) -> bool {
    mailbox.reconfigure(other_config());
    let frames = frames_until(events, state, |f| {
        matches!(f.event, Event::SessionUpdated { .. })
    })
    .await;
    summaries(&frames).pop().expect("a summary").busy
}

#[tokio::test]
async fn a_summary_says_busy_while_the_turn_runs_and_idle_once_it_is_over() {
    let provider = ScriptedProvider::new(vec![Script::Hang(vec![])]);
    let mailbox = start(provider, vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(IntentId::mint(), Input::text("go", Origin::surface("test")));
    drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::TurnStarted { .. })
    })
    .await;

    assert!(restated(&mailbox, &mut events, &mut state).await);
    assert!(
        mailbox.summary().await.unwrap().busy,
        "the listing reads the same fact as the frame"
    );
    assert!(state.busy(), "and so does the state the frames fold to");

    mailbox.interrupt(IntentId::mint(), InterruptScope::Head);
    drive(&mut events, &mut state, turn_completed).await;

    assert!(!restated(&mailbox, &mut events, &mut state).await);
    assert!(!mailbox.summary().await.unwrap().busy);
    assert!(!state.busy());
}

/// A process that died inside a turn left `busy: true` in the head of its
/// journal. Nothing this process publishes may repeat it: not the head of the
/// new segment, and not the frame that mints the session's name.
#[tokio::test]
async fn a_resumed_session_publishes_no_busy_it_inherited() {
    let head = SessionSummary {
        busy: true,
        ..summary("ses_1")
    };
    let frames = vec![journal_frame(1, Event::SessionUpdated { summary: head })];
    let provider = ScriptedProvider::new(vec![Script::Events(text("back"))]);
    let mailbox = resume(frames, None, Services::none(), |_| {
        Arc::new(config(provider, vec![], Arc::new(NoHost)))
    })
    .unwrap();
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    assert!(!state.summary.busy, "the new segment's head says idle");

    mailbox.submit(
        IntentId::mint(),
        Input::text("name me", Origin::surface("test")),
    );
    let frames = frames_until(&mut events, &mut state, turn_completed).await;
    let minted = summaries(&frames);
    assert_eq!(
        minted.iter().map(|s| s.title.clone()).collect::<Vec<_>>(),
        vec![Some("name me".to_string())],
        "one summary went out, the mint's"
    );
    assert!(!minted[0].busy, "the mint lands before the turn opens");
}

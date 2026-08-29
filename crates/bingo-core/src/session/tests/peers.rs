//! Peer delivery and redirect (ADR-0010 §1–2): prose from another session,
//! past the command parser and the submit hooks.

use super::*;

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
async fn a_wake_delivery_to_an_idle_session_opens_a_peer_turn_that_says_who_spoke() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("ok"))]);
    let mailbox = start(provider.clone(), vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();

    mailbox.deliver(IntentId::mint(), peer("hi", "reviewer"), Delivery::Wake);
    let frames = frames_until(&mut events, &mut state, turn_completed).await;
    assert_eq!(turn_origin(&frames), Some(TurnOrigin::Peer));
    let labels: Vec<String> = frames.iter().map(|f| label(&f.event)).collect();
    assert_eq!(
        &labels[..3],
        ["completed:user/completed", "turnStarted", "ack:TurnStarted"]
    );
    let ItemBody::User { origin, .. } = &state.items[0].body else {
        panic!("a user item first");
    };
    assert_eq!(origin.principal.as_deref(), Some("reviewer"));
    let request = &provider.requests()[0];
    let first = request.messages[0].parts[0].as_text();
    assert_eq!(
        first,
        Some("[from reviewer]"),
        "the model is told who wrote"
    );
    assert_eq!(request.messages[0].parts[1].as_text(), Some("hi"));
}

#[tokio::test]
async fn a_held_delivery_waits_in_the_queue_and_the_next_submit_carries_it_first() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("ok"))]);
    let mailbox = start(provider, vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();

    let held = IntentId::mint();
    mailbox.deliver(held.clone(), peer("held", "reviewer"), Delivery::Hold);
    let frames = frames_until(&mut events, &mut state, |f| {
        matches!(f.event, Event::IntentAck { .. })
    })
    .await;
    let labels: Vec<String> = frames.iter().map(|f| label(&f.event)).collect();
    assert_eq!(labels, ["queue:1", "ack:Queued"]);
    assert!(state.turn.is_none(), "hold opens no turn");

    let mine = IntentId::mint();
    mailbox.submit(mine.clone(), Input::text("go", Origin::surface("test")));
    let frames = frames_until(&mut events, &mut state, turn_completed).await;
    assert_eq!(turn_origin(&frames), Some(TurnOrigin::Submit));
    assert_eq!(user_texts(&state), ["held", "go"], "held prose goes first");
    let Some(Event::TurnStarted { inputs, .. }) = frames
        .iter()
        .map(|f| &f.event)
        .find(|e| matches!(e, Event::TurnStarted { .. }))
    else {
        panic!("a turn started");
    };
    assert_eq!(inputs.len(), 2, "one turn carries both");
    assert!(state.queue.is_empty());
}

#[tokio::test]
async fn a_delivery_to_a_busy_session_is_queued_whatever_its_kind() {
    let provider = ScriptedProvider::new(vec![Script::Hang(vec![ModelEvent::TextStart {
        id: "b".into(),
    }])]);
    let mailbox = start(provider, vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(IntentId::mint(), Input::text("go", Origin::surface("test")));
    frames_until(&mut events, &mut state, |f| {
        matches!(f.event, Event::ItemStarted { .. })
    })
    .await;
    assert!(state.busy());

    mailbox.deliver(IntentId::mint(), peer("later", "reviewer"), Delivery::Wake);
    let frames = frames_until(&mut events, &mut state, |f| {
        matches!(f.event, Event::IntentAck { .. })
    })
    .await;
    let labels: Vec<String> = frames.iter().map(|f| label(&f.event)).collect();
    assert_eq!(labels, ["queue:1", "ack:Queued"]);
    assert_eq!(state.queue[0].origin.principal.as_deref(), Some("reviewer"));
}

#[tokio::test]
async fn a_peer_may_not_deliver_an_action() {
    let provider = ScriptedProvider::new(vec![]);
    let mailbox = start(provider, vec![]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    let intent = IntentId::mint();
    mailbox.deliver(
        intent.clone(),
        Input::Action {
            action: Action {
                name: "compact".into(),
                args: Value::Null,
            },
        },
        Delivery::Wake,
    );
    let frames = frames_until(&mut events, &mut state, |f| {
        matches!(f.event, Event::IntentAck { .. })
    })
    .await;
    assert!(matches!(
        &frames[0].event,
        Event::IntentAck { outcome: IntentOutcome::Rejected { error }, .. }
            if error.code == ErrorCode::InvalidInput
    ));
}

/// Two sessions on one routing host: `a` carries a hook that sends `@b …` to `b`.
fn pair() -> (Mailbox, Mailbox, Arc<ScriptedProvider>) {
    let provider = ScriptedProvider::new(vec![Script::Events(text("ok"))]);
    let routes = RoutingHost::new();
    let b = spawn(summary("ses_b"), None, Services::none(), {
        let provider = provider.clone();
        let routes = routes.clone();
        move |_| Arc::new(config(provider, vec![], routes))
    });
    routes.route(b.clone());
    let a = spawn(summary("ses_a"), None, Services::none(), {
        let provider = provider.clone();
        let routes = routes.clone();
        let to = b.id().clone();
        move |_| {
            let mut cfg = config(provider, vec![], routes);
            cfg.hooks = vec![Arc::new(RedirectHook {
                name: "b".into(),
                to,
            })];
            Arc::new(cfg)
        }
    });
    (a, b, provider)
}

#[tokio::test]
async fn a_redirect_hook_sends_the_line_elsewhere_and_acks_where_it_went() {
    let (a, b, _) = pair();
    let (mut state_a, mut events_a) = a.attach().await.unwrap();
    let (mut state_b, mut events_b) = b.attach().await.unwrap();

    let intent = IntentId::mint();
    a.submit(
        intent.clone(),
        Input::text("@b hello", Origin::surface("test")),
    );
    let frames = frames_until(&mut events_a, &mut state_a, |f| {
        matches!(f.event, Event::IntentAck { .. })
    })
    .await;
    assert!(matches!(
        &frames[0].event,
        Event::IntentAck { intent: i, outcome: IntentOutcome::Applied { result } }
            if i == &intent && result == &json!({ "redirected": b.id() })
    ));
    assert!(state_a.items.is_empty(), "nothing was recorded here");

    let frames = frames_until(&mut events_b, &mut state_b, turn_completed).await;
    assert_eq!(turn_origin(&frames), Some(TurnOrigin::Peer));
    assert_eq!(user_texts(&state_b), ["hello"], "the address is stripped");
    let ItemBody::User { origin, .. } = &state_b.items[0].body else {
        panic!("a user item first");
    };
    assert_eq!(
        origin.surface, "test",
        "the origin is the person's, not a peer's"
    );
}

#[tokio::test]
async fn a_redirect_to_a_session_that_is_gone_is_rejected() {
    let provider = ScriptedProvider::new(vec![]);
    let routes = RoutingHost::new();
    let a = spawn(summary("ses_a"), None, Services::none(), move |_| {
        let mut cfg = config(provider, vec![], routes);
        cfg.hooks = vec![Arc::new(RedirectHook {
            name: "ghost".into(),
            to: SessionId::from_raw("ses_ghost"),
        })];
        Arc::new(cfg)
    });
    let (mut state, mut events) = a.attach().await.unwrap();
    let intent = IntentId::mint();
    a.submit(intent, Input::text("@ghost boo", Origin::surface("test")));
    let frames = frames_until(&mut events, &mut state, |f| {
        matches!(f.event, Event::IntentAck { .. })
    })
    .await;
    assert!(matches!(
        &frames[0].event,
        Event::IntentAck { outcome: IntentOutcome::Rejected { error }, .. }
            if error.code == ErrorCode::SessionNotFound
    ));
}

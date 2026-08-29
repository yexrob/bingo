//! Command dispatch through the actor (ADR-0008).

use super::*;

fn with_commands(provider: Arc<ScriptedProvider>, commands: Vec<Arc<dyn Command>>) -> Mailbox {
    spawn(summary("ses_1"), None, services(commands), |_| {
        Arc::new(config(provider, vec![], Arc::new(NoHost)))
    })
}

fn ack_of(frames: &[Frame], intent: &IntentId) -> Option<IntentOutcome> {
    frames.iter().find_map(|f| match &f.event {
        Event::IntentAck { intent: i, outcome } if i == intent => Some(outcome.clone()),
        _ => None,
    })
}

#[tokio::test]
async fn an_instant_command_runs_during_a_turn_and_is_applied() {
    let provider = ScriptedProvider::new(vec![Script::Hang(vec![ModelEvent::TextStart {
        id: "b".into(),
    }])]);
    let echo = ScriptedCommand::new(
        "echo",
        true,
        Ok(CommandOutcome::Applied {
            message: Some("done".into()),
        }),
    );
    let mailbox = with_commands(provider, vec![echo.clone()]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(IntentId::mint(), Input::text("go", Origin::surface("test")));
    drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::ItemStarted { .. })
    })
    .await;
    assert!(state.busy());

    let intent = IntentId::mint();
    mailbox.submit(
        intent.clone(),
        Input::text("/echo  hello there", Origin::surface("test")),
    );
    let frames = collect(
        &mut events,
        &mut state,
        |f| matches!(&f.event, Event::IntentAck { intent: i, .. } if i == &intent),
    )
    .await;
    assert_eq!(
        ack_of(&frames, &intent),
        Some(IntentOutcome::Applied {
            result: json!({ "message": "done" })
        })
    );
    assert_eq!(echo.calls(), vec!["hello there".to_string()]);
    assert!(state.busy(), "the turn went on underneath");
    assert!(state.queue.is_empty(), "instant means never queued");
}

#[tokio::test]
async fn a_command_that_is_not_instant_waits_and_holds_the_prose_behind_it() {
    let provider = ScriptedProvider::new(vec![
        Script::Hang(vec![ModelEvent::TextStart { id: "b".into() }]),
        Script::Events(text("after")),
    ]);
    let slow = ScriptedCommand::new("slow", false, Ok(CommandOutcome::Applied { message: None }));
    let gate = slow.gated();
    let mailbox = with_commands(provider, vec![slow.clone()]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(
        IntentId::mint(),
        Input::text("first", Origin::surface("test")),
    );
    drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::ItemStarted { .. })
    })
    .await;

    let command = IntentId::mint();
    mailbox.submit(
        command.clone(),
        Input::text("/slow", Origin::surface("test")),
    );
    let prose = IntentId::mint();
    mailbox.submit(
        prose.clone(),
        Input::text("second", Origin::surface("test")),
    );
    let frames = collect(
        &mut events,
        &mut state,
        |f| matches!(&f.event, Event::IntentAck { intent, .. } if intent == &prose),
    )
    .await;
    assert!(matches!(
        ack_of(&frames, &command),
        Some(IntentOutcome::Queued { position: 1 })
    ));
    assert!(matches!(
        ack_of(&frames, &prose),
        Some(IntentOutcome::Queued { position: 2 })
    ));
    assert_eq!(state.queue.len(), 2);
    assert!(!state.queue[0].steerable, "a command is not steering");

    mailbox.interrupt(IntentId::mint(), InterruptScope::Head);
    let labels = drive(&mut events, &mut state, turn_completed).await;
    assert!(labels.contains(&"turnCompleted:Interrupted".to_string()));
    // The command runs now; until it finishes, the prose behind it waits.
    let frames = collect(&mut events, &mut state, |f| {
        matches!(f.event, Event::QueueChanged { .. })
    })
    .await;
    assert_eq!(
        state.queue.len(),
        1,
        "the command left the queue, the prose did not"
    );
    assert!(
        frames
            .iter()
            .all(|f| !matches!(f.event, Event::TurnStarted { .. }))
    );
    assert!(!state.busy(), "a held queue is not a running turn");

    gate.notify_one();
    let labels = drive(&mut events, &mut state, turn_completed).await;
    let applied = labels.iter().position(|l| l == "ack:Applied").unwrap();
    let started = labels.iter().position(|l| l == "turnStarted").unwrap();
    assert!(applied < started, "{labels:?}");
    assert_eq!(slow.calls(), vec![String::new()]);
    assert_eq!(
        state.items.last().map(|i| i.body.clone()),
        Some(ItemBody::Assistant {
            text: "after".into()
        })
    );
}

#[tokio::test]
async fn a_prompt_outcome_opens_a_turn_with_the_commands_own_intent() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("hi back"))]);
    let greet = ScriptedCommand::new(
        "greet",
        true,
        Ok(CommandOutcome::Prompt {
            text: "say hi".into(),
        }),
    );
    let mailbox = with_commands(provider, vec![greet]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    let intent = IntentId::mint();
    mailbox.submit(
        intent.clone(),
        Input::text("/greet", Origin::surface("test")),
    );
    let frames = collect(&mut events, &mut state, turn_completed).await;
    assert!(matches!(
        ack_of(&frames, &intent),
        Some(IntentOutcome::TurnStarted { .. })
    ));
    let user = &state.items[0];
    assert_eq!(user.intent.as_ref(), Some(&intent));
    assert!(
        matches!(&user.body, ItemBody::User { parts, origin } if parts[0].as_text() == Some("say hi") && origin.surface == "test")
    );
}

#[tokio::test]
async fn a_record_outcome_is_an_item_and_an_action_dispatches_by_name() {
    let note = ScriptedCommand::new(
        "note",
        true,
        Ok(CommandOutcome::Record {
            body: ItemBody::Action {
                name: "note".into(),
                args: json!("x"),
                result: Some(json!("ok")),
            },
        }),
    );
    let mailbox = with_commands(ScriptedProvider::new(vec![]), vec![note.clone()]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    let intent = IntentId::mint();
    mailbox.submit(
        intent.clone(),
        Input::Action {
            action: Action {
                name: "note".into(),
                args: json!({ "k": 1 }),
            },
        },
    );
    let frames = collect(&mut events, &mut state, |f| {
        matches!(f.event, Event::IntentAck { .. })
    })
    .await;
    let Some(IntentOutcome::Applied { result }) = ack_of(&frames, &intent) else {
        panic!("applied");
    };
    let item = ItemId::from_raw(result["item"].as_str().unwrap());
    assert!(
        matches!(&state.item(&item).unwrap().body, ItemBody::Action { name, .. } if name == "note")
    );
    assert_eq!(note.calls(), vec![r#"{"k":1}"#.to_string()]);
}

#[tokio::test]
async fn a_failing_command_is_rejected_with_its_error() {
    let bad = ScriptedCommand::new(
        "bad",
        true,
        Err(KernelError::new(ErrorCode::InvalidInput, "no such model")),
    );
    let mailbox = with_commands(ScriptedProvider::new(vec![]), vec![bad]);
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    let intent = IntentId::mint();
    mailbox.submit(
        intent.clone(),
        Input::text("/bad x", Origin::surface("test")),
    );
    let frames = collect(&mut events, &mut state, |f| {
        matches!(f.event, Event::IntentAck { .. })
    })
    .await;
    assert!(matches!(
        ack_of(&frames, &intent),
        Some(IntentOutcome::Rejected { error }) if error.message == "no such model"
    ));
}

#[tokio::test]
async fn a_compaction_turn_compacts_and_closes_and_is_refused_while_a_turn_runs() {
    let provider = ScriptedProvider::new(vec![
        Script::Events(text("one")),
        Script::Hang(vec![ModelEvent::TextStart { id: "b".into() }]),
    ]);
    let compactor = ScriptedCompactor::new(vec![ScriptedCompactor::cut("itm_none", 9_000, 100)]);
    let mailbox = spawn(summary("ses_1"), None, Services::none(), |_| {
        let mut cfg = config(provider, vec![], Arc::new(NoHost));
        cfg.compactor = Some(compactor);
        Arc::new(cfg)
    });
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(IntentId::mint(), Input::text("hi", Origin::surface("test")));
    drive(&mut events, &mut state, turn_completed).await;

    mailbox.compact(None).await.unwrap();
    let labels = drive(&mut events, &mut state, turn_completed).await;
    assert_eq!(
        labels,
        vec![
            "turnStarted",
            "completed:compaction/completed",
            "compacted",
            "turnCompleted:Completed",
        ]
    );
    assert_eq!(state.history_generation, 1);

    mailbox.submit(
        IntentId::mint(),
        Input::text("more", Origin::surface("test")),
    );
    drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::ItemStarted { .. })
    })
    .await;
    let refused = mailbox.compact(None).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::NotReady);
}

#[tokio::test]
async fn turn_end_hooks_run_after_the_completion_is_published() {
    let provider = ScriptedProvider::new(vec![Script::Events(text("hello"))]);
    let hook = GatedHook::new();
    let gate = hook.gate.clone();
    let fired = hook.fired.clone();
    let mailbox = spawn(summary("ses_1"), None, Services::none(), |_| {
        let mut cfg = config(provider, vec![], Arc::new(NoHost));
        cfg.hooks = vec![hook];
        Arc::new(cfg)
    });
    let (mut state, mut events) = mailbox.attach().await.unwrap();
    mailbox.submit(IntentId::mint(), Input::text("hi", Origin::surface("test")));
    drive(&mut events, &mut state, turn_completed).await;
    assert!(
        !fired.load(std::sync::atomic::Ordering::SeqCst),
        "the hook is still waiting when the completion is on the wire"
    );
    gate.notify_one();
    mailbox.close(CloseReason::Client);
    mailbox.wait_closed().await;
    assert!(
        fired.load(std::sync::atomic::Ordering::SeqCst),
        "the actor waited for its post-turn work"
    );
}

/// Fold frames until `stop`, keeping them.
async fn collect(
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

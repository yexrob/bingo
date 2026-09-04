//! The queue a running turn leaves behind (ADR-0008 §2, amended M68): a line
//! that asked to wait is not absorbed at a barrier, and a line still queued
//! can be taken back out by the surface that put it there.

use super::*;

static GATE: PluginManifest = PluginManifest {
    id: "test.gate",
    version: "0",
    sdk: "^0.1",
    provides: &["policy:gate"],
    requires: &[],
    config: None,
};

/// Asks about every call. Nothing here waits on a clock: an unanswered
/// question is a turn that is running and has not reached its barrier, which
/// is exactly the moment these tests queue a line in.
struct AskingPolicy;

#[async_trait]
impl PermissionPolicy for AskingPolicy {
    fn id(&self) -> &str {
        "gate"
    }
    async fn decide(&self, _: PolicyInput<'_>) -> Decision {
        Decision::Ask {
            reason: Reason::Default,
            scope: None,
        }
    }
}

async fn gated_host(scripts: Vec<Script>) -> (Arc<Host>, Arc<ScriptedProvider>) {
    let provider = ScriptedProvider::new(scripts);
    let plugins = vec![
        TestPlugin::boxed(&PROVIDER, vec![Contribution::Provider(provider.clone())]),
        TestPlugin::boxed(
            &TOOLS,
            vec![Contribution::Tool(Arc::new(EchoTool { read_only: true }))],
        ),
        TestPlugin::boxed(&GATE, vec![Contribution::Policy(Arc::new(AskingPolicy))]),
    ];
    let config = HostConfig::new(env()).with_layer("cli", json!({ "model": "m" }));
    (Host::build(plugins, config).await.unwrap(), provider)
}

async fn attached(host: &Host) -> Attachment {
    host.open(
        SessionSelector::Create {
            spec: spec("/work"),
        },
        who(),
        OpenOptions::default(),
    )
    .await
    .unwrap()
}

/// A line as a surface sends it, with the word it asked to be delivered by.
fn line(text: &str, surface: &str, delivery: Delivery) -> Input {
    Input::Text {
        text: text.into(),
        images: Vec::new(),
        origin: Origin::surface(surface),
        delivery,
    }
}

/// Fold until a question is open; its id.
async fn opened(a: &mut Attachment) -> InteractionId {
    while let Some(frame) = a.events.next().await {
        if let Event::InteractionOpened { interaction } = frame.event {
            return interaction.id;
        }
    }
    panic!("no question was ever opened");
}

async fn turn_started(a: &mut Attachment) {
    while let Some(frame) = a.events.next().await {
        if matches!(frame.event, Event::TurnStarted { .. }) {
            return;
        }
    }
    panic!("no turn ever started");
}

/// Fold until the queue holds `n` lines; the previews it then shows.
async fn queued(a: &mut Attachment, n: usize) -> Vec<String> {
    while let Some(frame) = a.events.next().await {
        if let Event::QueueChanged { entries, .. } = &frame.event
            && entries.len() == n
        {
            return entries.iter().map(|e| e.preview.clone()).collect();
        }
    }
    panic!("the queue never held {n} lines");
}

/// Fold until `n` turns have completed.
async fn turns_completed(a: &mut Attachment, n: usize) {
    let mut seen = 0;
    while let Some(frame) = a.events.next().await {
        if matches!(frame.event, Event::TurnCompleted { .. }) {
            seen += 1;
            if seen == n {
                return;
            }
        }
    }
    panic!("only {seen} of {n} turns completed");
}

/// Every word the model was sent, in order.
fn said(request: &ModelRequest) -> Vec<&str> {
    request
        .messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .filter_map(|part| part.as_text())
        .collect()
}

/// The barrier absorbs what steers and leaves what waits; the turn that opens
/// next is the one the held line was waiting for.
#[tokio::test]
async fn a_held_line_waits_out_the_barrier_and_the_next_turn_takes_it() {
    let (host, provider) = gated_host(vec![
        Script::Events(tool_call("Echo", json!({ "v": 1 }))),
        Script::Events(text("first")),
        Script::Events(text("second")),
    ])
    .await;
    let mut a = attached(&host).await;
    a.handle
        .submit(IntentId::mint(), Input::text("go", Origin::surface("test")));
    let gate = opened(&mut a).await;

    a.handle
        .submit(IntentId::mint(), line("steer me", "test", Delivery::Wake));
    a.handle
        .submit(IntentId::mint(), line("later this", "test", Delivery::Hold));
    let previews = queued(&mut a, 2).await;
    assert_eq!(previews, ["steer me", "later this"]);

    a.handle.answer(
        IntentId::mint(),
        gate,
        Answer::AllowOnce,
        Activation::Pointer,
    );
    turns_completed(&mut a, 2).await;

    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        3,
        "the call, the barrier, and the next turn"
    );
    let steered = said(&requests[1]);
    assert!(steered.contains(&"steer me"), "{steered:?}");
    assert!(
        !steered.contains(&"later this"),
        "a line that asked to wait does not steer: {steered:?}"
    );
    assert!(said(&requests[2]).contains(&"later this"), "it waited");
}

/// `withdraw` is how a queued line reaches an editor again: it is the
/// submitting surface's to take, once, and only while it is still queued.
#[tokio::test]
async fn a_queued_line_comes_back_out_to_the_surface_that_sent_it() {
    let (host, _) = gated_host(vec![Script::Hang(Vec::new())]).await;
    let mut a = attached(&host).await;
    let session = a.session.clone();
    a.handle
        .submit(IntentId::mint(), Input::text("go", Origin::surface("test")));
    // The scripted stream never ends, so the turn runs for the whole test.
    turn_started(&mut a).await;

    let mine = IntentId::mint();
    a.handle
        .submit(mine.clone(), line("mine to edit", "test", Delivery::Hold));
    let theirs = IntentId::mint();
    a.handle
        .submit(theirs.clone(), line("not mine", "rpc", Delivery::Wake));
    queued(&mut a, 2).await;

    assert_eq!(
        host.withdraw(&session, &theirs, who())
            .await
            .err()
            .map(|e| e.code),
        Some(ErrorCode::PermissionDenied),
        "another surface's line stays where it is"
    );
    assert_eq!(
        host.withdraw(&session, &IntentId::mint(), who())
            .await
            .err()
            .map(|e| e.code),
        Some(ErrorCode::NotFound),
        "and a line this session never held is nothing to take"
    );

    let back = host.withdraw(&session, &mine, who()).await.unwrap();
    assert_eq!(back, line("mine to edit", "test", Delivery::Hold));
    assert_eq!(
        queued(&mut a, 1).await,
        ["not mine"],
        "every client's fold loses the row"
    );
    assert_eq!(
        host.withdraw(&session, &mine, who())
            .await
            .err()
            .map(|e| e.code),
        Some(ErrorCode::NotFound),
        "once out, the person has it and the queue does not"
    );
}

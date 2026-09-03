//! The door into a running turn (ADR-0036 §2). Every test here holds the
//! model's stream open — `Script::Hang` never ends — so what is asserted is
//! that a call is served *beside* a turn that is still streaming, which is the
//! whole point of the door: whoever handed the call in is blocked on the
//! answer before it can go on.

use super::*;
use crate::executor::INTERRUPTED_MARKER;

/// A turn whose stream never ends: it has said its first word and is still
/// speaking, which is when a bridged call arrives.
fn streaming() -> Arc<ScriptedProvider> {
    ScriptedProvider::new(vec![Script::Hang(vec![
        ModelEvent::TextStart { id: "b".into() },
        ModelEvent::TextDelta {
            id: "b".into(),
            delta: "thinking".into(),
        },
    ])])
}

fn call(name: &str, v: i32) -> ToolCall {
    ToolCall {
        call_id: format!("bridged-{v}"),
        name: name.into(),
        input: json!({ "v": v }),
    }
}

/// Start a session and take it to the point where its turn is streaming.
async fn mid_turn(mailbox: &Mailbox) -> (SessionState, FrameStream) {
    let (mut state, mut events) = mailbox.attach().await.expect("attached");
    mailbox.submit(IntentId::mint(), Input::text("hi", Origin::surface("test")));
    drive(&mut events, &mut state, |f| {
        matches!(f.event, Event::ItemDelta { .. })
    })
    .await;
    assert!(state.busy(), "the model is still speaking");
    (state, events)
}

fn start_with(
    provider: Arc<ScriptedProvider>,
    tools: Vec<Arc<dyn Tool>>,
    shape: impl Fn(&mut crate::turn::TurnConfig) + Send + 'static,
) -> Mailbox {
    spawn(summary("ses_1"), None, Services::none(), move |_| {
        let mut cfg = config(provider, tools, Arc::new(NoHost));
        shape(&mut cfg);
        Arc::new(cfg)
    })
}

fn item_of(state: &SessionState, id: &ItemId) -> Item {
    state
        .items
        .iter()
        .find(|item| &item.id == id)
        .unwrap_or_else(|| panic!("no {id} in {:?}", state.items))
        .clone()
}

#[tokio::test]
async fn a_call_is_served_while_the_turn_s_stream_is_still_open() {
    let mailbox = start_with(
        streaming(),
        vec![Arc::new(EchoTool { read_only: true })],
        |_| {},
    );
    let (mut state, mut events) = mid_turn(&mailbox).await;

    let outcome = mailbox
        .invoke(call("Echo", 1))
        .await
        .expect("the running turn served it");

    assert_eq!(outcome.status, ItemStatus::Completed);
    assert_eq!(outcome.call_id, "bridged-1");
    assert_eq!(outcome.output.parts[0].as_text(), Some("echo:1"));

    let labels = drive(
        &mut events,
        &mut state,
        |f| matches!(&f.event, Event::ItemCompleted { item } if item.id == outcome.item),
    )
    .await;
    assert_eq!(
        labels,
        [
            "started:tool/pending",
            "updated:tool/running",
            "completed:tool/completed"
        ],
        "journaled as any call of the model's own is"
    );
    let item = item_of(&state, &outcome.item);
    assert_eq!(item.turn, state.turn.as_ref().map(|t| t.id.clone()));
    assert!(item.external(), "it is the caller's call, not the model's");
    assert!(state.last_turn.is_none(), "the turn is still streaming");
}

#[tokio::test]
async fn a_call_with_no_turn_in_flight_is_refused_with_a_reason() {
    let mailbox = start_with(
        ScriptedProvider::new(vec![]),
        vec![Arc::new(EchoTool { read_only: true })],
        |_| {},
    );
    let (state, _events) = mailbox.attach().await.expect("attached");
    assert!(!state.busy());

    let refused = mailbox
        .invoke(call("Echo", 1))
        .await
        .expect_err("nothing is in flight to serve it");
    assert_eq!(refused.code, ErrorCode::NotReady);
    assert!(refused.message.contains("no turn"), "{refused:?}");
}

/// The turn's own offer is the whole of what may be called: a tool the
/// session's set holds but this turn was not given is refused, and never run.
#[tokio::test]
async fn a_call_the_turn_was_not_offered_is_refused_and_never_runs() {
    let mailbox = start_with(
        streaming(),
        vec![Arc::new(EchoTool { read_only: true }), Arc::new(PanicTool)],
        |cfg| cfg.tools.only = Some(vec!["Echo".into()]),
    );
    let (mut state, mut events) = mid_turn(&mailbox).await;

    let refused = mailbox
        .invoke(call("Panic", 1))
        .await
        .expect_err("this turn was never given Panic");
    assert_eq!(refused.code, ErrorCode::ToolNotFound);
    assert!(refused.message.contains("Panic"), "{refused:?}");

    // Nothing was journaled for it: the door refused before it opened an item.
    let outcome = mailbox.invoke(call("Echo", 2)).await.expect("served");
    drive(
        &mut events,
        &mut state,
        |f| matches!(&f.event, Event::ItemCompleted { item } if item.id == outcome.item),
    )
    .await;
    assert_eq!(
        state
            .items
            .iter()
            .filter(|item| item.external())
            .map(kind)
            .collect::<Vec<_>>(),
        ["tool/completed"],
        "one bridged item, the one that was served"
    );
}

/// A tool that never ends on its own: only the interrupt can end this call.
struct Hanging;

#[async_trait::async_trait]
impl Tool for Hanging {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Hanging".into(),
            description: "never answers".into(),
            input_schema: json!({"type": "object"}),
            meta: Default::default(),
        }
    }
    fn traits(&self, _: &Value) -> ToolTraits {
        ToolTraits::read_only()
    }
    async fn call(&self, _input: Value, _cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn an_interrupt_drops_a_bridged_call_and_the_caller_is_told() {
    let mailbox = start_with(streaming(), vec![Arc::new(Hanging)], |_| {});
    let (mut state, mut events) = mid_turn(&mailbox).await;

    let asked = tokio::spawn({
        let mailbox = mailbox.clone();
        async move { mailbox.invoke(call("Hanging", 1)).await }
    });
    // The call is in flight once its item stands: no clock is waited on.
    drive(
        &mut events,
        &mut state,
        |f| matches!(&f.event, Event::ItemUpdated { item } if item.external()),
    )
    .await;

    mailbox.interrupt(IntentId::mint(), InterruptScope::Head);
    let outcome = asked
        .await
        .expect("the door's task lived")
        .expect("an outcome, not silence");
    assert_eq!(outcome.status, ItemStatus::Interrupted);
    assert_eq!(
        outcome.output.parts[0].as_text(),
        Some(INTERRUPTED_MARKER),
        "the caller is told why it stopped"
    );

    drive(&mut events, &mut state, turn_completed).await;
    assert!(matches!(
        state.last_turn,
        Some(TurnStatus::Interrupted { .. })
    ));
    assert_eq!(
        item_of(&state, &outcome.item).status,
        ItemStatus::Interrupted,
        "the journal says the same as the caller was told"
    );
}

/// A hook that refuses every call, which is one of the two ways the gate says
/// no without asking anybody.
struct Refusing;

#[async_trait::async_trait]
impl Hook for Refusing {
    fn id(&self) -> &str {
        "refusing"
    }
    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::BeforeTool],
            tool: None,
        }
    }
    async fn before_tool(&self, _: &mut ToolCall, _: &HookContext) -> HookOutcome {
        HookOutcome::Deny {
            reason: "not from out there".into(),
        }
    }
}

#[tokio::test]
async fn a_call_the_gate_denies_reports_the_denial_to_the_caller() {
    let mailbox = start_with(
        streaming(),
        vec![Arc::new(EchoTool { read_only: true })],
        |cfg| {
            cfg.hooks = crate::turn::HookSet::fixed(vec![Arc::new(Refusing)]);
        },
    );
    let (mut state, mut events) = mid_turn(&mailbox).await;

    let outcome = mailbox.invoke(call("Echo", 1)).await.expect("an outcome");
    assert_eq!(outcome.status, ItemStatus::Failed);
    assert!(outcome.output.is_error);
    let said = outcome.output.parts[0].as_text().unwrap_or_default();
    assert!(said.contains("refusing"), "{said}");
    assert!(said.contains("not from out there"), "{said}");

    drive(
        &mut events,
        &mut state,
        |f| matches!(&f.event, Event::ItemCompleted { item } if item.id == outcome.item),
    )
    .await;
    assert_eq!(item_of(&state, &outcome.item).status, ItemStatus::Failed);
}

//! What ADR-0030 opened last: a model, streaming from another process.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bingo_plugin_rpc::Manager;
use bingo_sdk::{
    CancellationToken, ContentPart, EndpointCapabilities, Message, ModelEvent, ModelRequest,
    ModelStream, Provider, ProviderError, Role, ToolSpec,
};
use futures::StreamExt;
use serde_json::json;

use crate::harness::{call, only_tool, said, started};

async fn only_provider(manager: &Manager) -> Arc<dyn Provider> {
    let mut providers = manager.providers().await;
    assert_eq!(providers.len(), 1, "the stub declares one provider");
    providers.remove(0)
}

/// One request. What the stub does with it is written in the last user text,
/// and the tools it is given decide whether it asks for one.
fn ask(said: &str, tools: Vec<ToolSpec>) -> ModelRequest {
    ModelRequest {
        model: "stub-1".into(),
        max_tokens: 1_000,
        system: Vec::new(),
        messages: vec![Message::text(Role::User, said)],
        tools,
        reasoning: None,
        session: None,
        provider_options: Default::default(),
    }
}

fn tool(name: &str) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: "a tool the stub may ask for".into(),
        input_schema: json!({ "type": "object" }),
        meta: serde_json::Map::new(),
    }
}

/// The streams the stub has been told to stop, as it read them off the pipe.
/// A drop cannot wait for the notice it sends, so a test that asks about one
/// polls, as it polls for a respawn.
async fn cancelled(manager: &Manager, cwd: &Path) -> String {
    for _ in 0..300 {
        let tool = only_tool(manager).await;
        let (_, answered) = call(&tool, json!({ "cancelled": true }), cwd).await;
        let seen = said(&answered.expect("an output"));
        if !seen.is_empty() {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the process was never told to stop");
}

/// Everything the stream yielded, in order.
async fn drain(stream: &mut ModelStream) -> Vec<Result<ModelEvent, ProviderError>> {
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

/// The text of a whole response, as the kernel's accumulator would fold it.
fn folded(events: &[Result<ModelEvent, ProviderError>]) -> String {
    events
        .iter()
        .filter_map(|event| match event {
            Ok(ModelEvent::TextDelta { delta, .. }) => Some(delta.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// And for the model: a provider is what the handshake declared, and a model
/// it never declared can do nothing (ADR-0015 §4).
#[tokio::test]
async fn a_plugin_s_provider_serves_the_models_it_declared() {
    let (manager, _home, _project) = started(&[]).await;
    let provider = only_provider(&manager).await;
    assert_eq!(provider.id(), "stub");
    assert_eq!(provider.family(), "stub", "it declared no other shelf");
    let models = provider.models().await.expect("the declared models");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "stub-1");
    assert_eq!(models[0].display.as_deref(), Some("Stub One"));
    assert!(provider.endpoint("stub-1").images);
    assert_eq!(
        provider.endpoint("stub-2"),
        EndpointCapabilities::default(),
        "an undeclared model is false all round"
    );
    manager.shutdown().await;
}

/// The exit criterion of ADR-0030 for providers: a whole response is written
/// in another process and arrives here as the sdk's own events, in order.
#[tokio::test]
async fn a_whole_response_crosses_the_pipe_event_by_event() {
    let (manager, _home, _project) = started(&[]).await;
    let provider = only_provider(&manager).await;
    let mut stream = provider
        .stream(ask("two words", Vec::new()), CancellationToken::new())
        .await
        .expect("the stream opened");
    let events = drain(&mut stream).await;
    assert_eq!(folded(&events), "two words");
    assert!(
        matches!(events.first(), Some(Ok(ModelEvent::TextStart { .. }))),
        "{events:?}"
    );
    assert!(
        matches!(
            events.last(),
            Some(Ok(ModelEvent::Finish { finish_reason, .. }))
                if finish_reason.unified == bingo_sdk::UnifiedFinish::Stop
        ),
        "the close came after the finish: {events:?}"
    );
    manager.shutdown().await;
}

/// A round trip is two streams: the model asks for a tool, the kernel answers
/// with a result in the next request, and the model says what it found.
#[tokio::test]
async fn a_tool_round_trip_crosses_as_two_streams() {
    let (manager, _home, _project) = started(&[]).await;
    let provider = only_provider(&manager).await;
    let mut asking = provider
        .stream(
            ask("look it up", vec![tool("Read")]),
            CancellationToken::new(),
        )
        .await
        .expect("the stream opened");
    let events = drain(&mut asking).await;
    let Some(Ok(ModelEvent::ToolCall { id, name, input })) = events.first() else {
        panic!("the tools invited a call: {events:?}");
    };
    assert_eq!(name, "Read");
    assert_eq!(input, &json!({ "text": "look it up" }).to_string());

    let mut request = ask("look it up", vec![tool("Read")]);
    request
        .messages
        .push(Message::user(vec![ContentPart::ToolResult {
            tool_use_id: id.clone(),
            parts: vec![ContentPart::text("the file said so")],
            is_error: false,
        }]));
    let mut answering = provider
        .stream(request, CancellationToken::new())
        .await
        .expect("the second stream opened");
    assert_eq!(folded(&drain(&mut answering).await), "done");
    manager.shutdown().await;
}

/// The defect the `call` key exists to prevent, over a real pipe: two streams
/// at once, and neither carries a word of the other's.
#[tokio::test]
async fn two_streams_at_once_never_mix_their_deltas() {
    let (manager, _home, _project) = started(&[]).await;
    let provider = only_provider(&manager).await;
    let mut first = provider
        .stream(ask("mine alone", Vec::new()), CancellationToken::new())
        .await
        .expect("one stream");
    let mut second = provider
        .stream(ask("yours only", Vec::new()), CancellationToken::new())
        .await
        .expect("and another");
    let (one, two) = tokio::join!(drain(&mut first), drain(&mut second));
    assert_eq!(folded(&one), "mine alone");
    assert_eq!(folded(&two), "yours only");
    manager.shutdown().await;
}

/// An interrupted turn: the token fires, the stream ends where the kernel
/// expects an interruption to end it, and the process is told to stop.
#[tokio::test]
async fn a_cancelled_stream_ends_and_the_process_is_told() {
    let (manager, _home, project) = started(&[]).await;
    let provider = only_provider(&manager).await;
    let cancel = CancellationToken::new();
    let mut stream = provider
        .stream(ask("hold", Vec::new()), cancel.clone())
        .await
        .expect("the stream opened");
    cancel.cancel();
    assert!(
        stream.next().await.is_none(),
        "a cancelled stream ends rather than erroring"
    );
    assert!(
        cancelled(&manager, project.path())
            .await
            .starts_with("call-"),
        "the process was told which stream to stop"
    );
    manager.shutdown().await;
}

/// Letting go is the other way a stream ends — nobody cancelled anything, the
/// reader simply dropped it — and the process is told just the same.
#[tokio::test]
async fn a_dropped_stream_tells_the_process_to_stop() {
    let (manager, _home, project) = started(&[]).await;
    let provider = only_provider(&manager).await;
    let stream = provider
        .stream(ask("hold", Vec::new()), CancellationToken::new())
        .await
        .expect("the stream opened");
    drop(stream);
    assert!(
        cancelled(&manager, project.path())
            .await
            .starts_with("call-")
    );
    manager.shutdown().await;
}

/// A process that dies mid-response: the stream ends in the kind a dropped
/// connection always speaks, which is retryable, so the kernel's ladder does
/// with it what it does with a 5xx.
#[tokio::test]
async fn a_process_that_dies_mid_stream_yields_the_error_the_trait_speaks() {
    let (manager, _home, _project) = started(&[]).await;
    let provider = only_provider(&manager).await;
    let mut stream = provider
        .stream(ask("die", Vec::new()), CancellationToken::new())
        .await
        .expect("the stream opened");
    let events = drain(&mut stream).await;
    assert!(
        matches!(events.first(), Some(Ok(ModelEvent::TextStart { .. }))),
        "what it managed to send arrives first: {events:?}"
    );
    let Some(Err(error)) = events.last() else {
        panic!("a stream whose process ended is an error: {events:?}");
    };
    assert!(
        matches!(error, ProviderError::Transport { message } if message.starts_with("stub: ")),
        "{error}"
    );
    assert!(error.retryable(), "the kernel retries this one");
    manager.shutdown().await;
}

/// A process that says the response failed says it in the trait's own error,
/// kind and all: the kernel waits the seconds the plugin named.
#[tokio::test]
async fn a_failed_response_crosses_as_the_error_the_trait_speaks() {
    let (manager, _home, _project) = started(&[]).await;
    let provider = only_provider(&manager).await;
    let mut stream = provider
        .stream(ask("fail", Vec::new()), CancellationToken::new())
        .await
        .expect("the stream opened");
    let events = drain(&mut stream).await;
    assert_eq!(
        events.first().and_then(|event| event.as_ref().err()),
        Some(&ProviderError::RateLimited {
            retry_after_ms: Some(1_500)
        })
    );
    manager.shutdown().await;
}

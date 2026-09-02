//! The Messages API end of the provider, against a local mock serving the
//! recorded fixtures in `fixtures/`. No live network call is made here, and
//! none is made anywhere in this crate's tests.

// An integration test is not `cfg(test)`; the test-only lint relief is spelled
// out, the way `crates/bingo/tests/cli.rs` spells it out.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use bingo_provider_anthropic::{AnthropicProvider, events};
use bingo_sdk::{
    CancellationToken, FinishReason, Message, ModelEvent, ModelRequest, Provider, ProviderError,
    Role, UnifiedFinish, Usage,
};
use futures::StreamExt;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "sk-ant-test";
const MODEL: &str = "claude-sonnet-4-5-20250929";

fn fixture(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn request() -> ModelRequest {
    ModelRequest {
        model: MODEL.into(),
        max_tokens: 1024,
        system: Vec::new(),
        messages: vec![Message::text(Role::User, "hello")],
        tools: Vec::new(),
        reasoning: None,
        session: None,
        provider_options: Default::default(),
    }
}

fn provider(server: &MockServer) -> AnthropicProvider {
    AnthropicProvider::with_endpoint(Some(KEY.into()), server.uri())
}

/// A mock that answers `POST /v1/messages` only when the request carries the
/// headers the API requires, so a missing header fails as a 404 mismatch.
async fn serve_stream(server: &MockServer, fixture_name: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", KEY))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header("content-type", "application/json"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(fixture(fixture_name), "text/event-stream"),
        )
        .mount(server)
        .await;
}

async fn serve_error(server: &MockServer, status: u16, body: &str, retry_after: Option<&str>) {
    let mut response =
        ResponseTemplate::new(status).set_body_raw(body.to_string(), "application/json");
    if let Some(value) = retry_after {
        response = response.insert_header("retry-after", value);
    }
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(response)
        .mount(server)
        .await;
}

async fn drain(
    provider: &AnthropicProvider,
    request: ModelRequest,
) -> Vec<Result<ModelEvent, ProviderError>> {
    provider
        .stream(request, CancellationToken::new())
        .await
        .expect("the mock accepted the request")
        .collect()
        .await
}

async fn events_of(fixture_name: &str) -> Vec<ModelEvent> {
    let server = MockServer::start().await;
    serve_stream(&server, fixture_name).await;
    drain(&provider(&server), request())
        .await
        .into_iter()
        .map(|item| item.expect("no failure"))
        .collect()
}

#[tokio::test]
async fn a_text_turn_streams_events_and_the_usage_the_wire_reported() {
    let events = events_of("text.sse").await;
    assert_eq!(
        events.first(),
        Some(&ModelEvent::StreamStart {
            warnings: Vec::new()
        })
    );
    assert_eq!(
        events.get(1),
        Some(&ModelEvent::ResponseMetadata {
            id: Some("msg_01Text".into()),
            model: Some(MODEL.into()),
        })
    );
    assert_eq!(
        events.last(),
        Some(&ModelEvent::Finish {
            usage: Usage {
                input_tokens: 12,
                output_tokens: 7,
                cache_read_tokens: 2048,
                cache_write_tokens: 320,
                reasoning_tokens: 0,
            },
            finish_reason: FinishReason {
                unified: UnifiedFinish::Stop,
                raw: Some("end_turn".into()),
            },
        })
    );
}

#[tokio::test]
async fn a_tool_turn_ends_with_the_call_and_a_tool_calls_finish() {
    let events = events_of("tools.sse").await;
    assert!(events.iter().any(|event| event
        == &ModelEvent::ToolCall {
            id: "toolu_01Read".into(),
            name: "Read".into(),
            input: r#"{"file_path":"Cargo.toml"}"#.into(),
        }));
    assert!(matches!(
        events.last(),
        Some(ModelEvent::Finish {
            finish_reason: FinishReason {
                unified: UnifiedFinish::ToolCalls,
                ..
            },
            ..
        })
    ));
}

#[tokio::test]
async fn a_truncated_turn_finishes_as_length() {
    assert!(matches!(
        events_of("max_tokens.sse").await.last(),
        Some(ModelEvent::Finish {
            finish_reason: FinishReason {
                unified: UnifiedFinish::Length,
                ..
            },
            ..
        })
    ));
}

#[tokio::test]
async fn a_signature_reaches_the_reasoning_end_over_the_wire() {
    let events = events_of("thinking.sse").await;
    let signature = events.iter().find_map(|event| match event {
        ModelEvent::ReasoningEnd {
            provider_metadata, ..
        } => provider_metadata
            .get(events::PROVIDER)
            .and_then(|m| m.get("signature"))
            .and_then(|v| v.as_str())
            .map(str::to_string),
        _ => None,
    });
    assert_eq!(signature.as_deref(), Some("ErUBCkYIBBgCIkA="));
}

#[tokio::test]
async fn a_mid_stream_error_event_ends_the_stream_as_a_retryable_failure() {
    let server = MockServer::start().await;
    serve_stream(&server, "error_mid_stream.sse").await;
    let items = drain(&provider(&server), request()).await;
    let last = items.last().expect("an item").as_ref();
    assert_eq!(
        last.err(),
        Some(&ProviderError::Server {
            status: 529,
            message: "overloaded_error: Overloaded".into(),
        })
    );
    assert!(items[..items.len() - 1].iter().all(Result::is_ok));
}

#[tokio::test]
async fn a_429_becomes_a_rate_limit_carrying_the_retry_after_delay() {
    let server = MockServer::start().await;
    let body = String::from_utf8(fixture("rate_limited.json")).expect("utf-8");
    serve_error(&server, 429, &body, Some("12")).await;
    let error = provider(&server)
        .stream(request(), CancellationToken::new())
        .await
        .err();
    assert_eq!(
        error,
        Some(ProviderError::RateLimited {
            retry_after_ms: Some(12_000)
        })
    );
    assert!(error.is_some_and(|e| e.retryable()));
}

#[tokio::test]
async fn a_400_naming_the_context_limit_becomes_an_overflow_the_loop_compacts() {
    let server = MockServer::start().await;
    let body = String::from_utf8(fixture("context_overflow.json")).expect("utf-8");
    serve_error(&server, 400, &body, None).await;
    let error = provider(&server)
        .stream(request(), CancellationToken::new())
        .await
        .err();
    assert!(
        matches!(&error, Some(ProviderError::ContextOverflow { message }) if message.contains("200000")),
        "{error:?}"
    );
    assert!(
        !error.is_some_and(|e| e.retryable()),
        "overflow is compacted"
    );
}

#[tokio::test]
async fn a_529_is_a_retryable_server_error_and_a_401_is_not() {
    for (status, retryable) in [(529, true), (500, true), (401, false), (403, false)] {
        let server = MockServer::start().await;
        serve_error(&server, status, r#"{"error":{"message":"nope"}}"#, None).await;
        let error = provider(&server)
            .stream(request(), CancellationToken::new())
            .await
            .err()
            .expect("a failure");
        assert_eq!(error.retryable(), retryable, "status {status}: {error:?}");
    }
}

#[tokio::test]
async fn the_body_on_the_wire_is_the_encoding_the_snapshots_pin() {
    let server = MockServer::start().await;
    serve_stream(&server, "text.sse").await;
    drain(&provider(&server), request()).await;
    let sent = server
        .received_requests()
        .await
        .expect("the server records requests");
    assert_eq!(sent.len(), 1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&sent[0].body).expect("json body"),
        serde_json::json!({
            "model": MODEL,
            "max_tokens": 1024,
            // A Claude 4 model caches, so the newest message takes a breakpoint.
            "messages": [{
                "role": "user",
                "content": [{
                    "type": "text",
                    "text": "hello",
                    "cache_control": { "type": "ephemeral" },
                }],
            }],
            "stream": true,
        })
    );
}

#[tokio::test]
async fn counting_tokens_reads_the_input_count() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages/count_tokens"))
        .and(header("x-api-key", KEY))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "input_tokens": 2095
        })))
        .mount(&server)
        .await;
    let counted = provider(&server)
        .count_tokens(&request())
        .await
        .expect("a count");
    assert_eq!(counted, 2095);
}

#[tokio::test]
async fn the_model_catalogue_comes_from_the_data_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("anthropic-version", "2023-06-01"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": [
                { "id": MODEL, "display_name": "Claude Sonnet 4.5" },
                { "id": "claude-3-5-haiku-20241022", "display_name": "Claude Haiku 3.5" },
            ]
        })))
        .mount(&server)
        .await;
    let models = provider(&server).models().await.expect("a catalogue");
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["claude-3-5-haiku-20241022", MODEL]
    );
    assert_eq!(models[1].display.as_deref(), Some("Claude Sonnet 4.5"));
}

#[tokio::test]
async fn a_failing_read_endpoint_is_classified_like_any_other_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            r#"{"error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;
    assert_eq!(
        provider(&server).models().await.err(),
        Some(ProviderError::Auth {
            message: "invalid x-api-key".into()
        })
    );
}

//! The Responses API end of the provider, against a local mock serving the
//! recorded fixtures in `fixtures/`. No live network call is made here, and
//! none is made anywhere in this crate's tests.

// An integration test is not `cfg(test)`; the test-only lint relief is spelled
// out, the way `crates/bingo/tests/cli.rs` spells it out.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Duration;

use bingo_provider_openai::variant::{ORIGINATOR, Variant};
use bingo_provider_openai::{OpenAiProvider, events};
use bingo_sdk::{
    CancellationToken, ContentPart, Effort, FinishReason, Message, ModelEvent, ModelRequest,
    Provider, ProviderError, Role, ToolSpec, UnifiedFinish, Usage,
};
use futures::StreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "sk-test";
const MODEL: &str = "gpt-5.4";

fn fixture(name: &str) -> Vec<u8> {
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(name);
    std::fs::read(&file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()))
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

fn provider(server: &MockServer) -> OpenAiProvider {
    OpenAiProvider::with_endpoint(Some(KEY.into()), server.uri())
}

/// A mock that answers `POST /v1/responses` only when the request carries the
/// headers the API requires, so a missing header fails as a 404 mismatch.
async fn serve_stream(server: &MockServer, fixture_name: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
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
        .and(path("/v1/responses"))
        .respond_with(response)
        .mount(server)
        .await;
}

async fn drain(
    provider: &OpenAiProvider,
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

async fn failure_of(status: u16, body: &str, retry_after: Option<&str>) -> ProviderError {
    let server = MockServer::start().await;
    serve_error(&server, status, body, retry_after).await;
    provider(&server)
        .stream(request(), CancellationToken::new())
        .await
        .err()
        .expect("a failure")
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
            id: Some("resp_01Text".into()),
            model: Some(MODEL.into()),
        })
    );
    assert_eq!(
        events.last(),
        Some(&ModelEvent::Finish {
            usage: Usage {
                // 3012 reported - 2048 cached: the sdk keeps the two apart.
                input_tokens: 964,
                output_tokens: 7,
                cache_read_tokens: 2048,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            },
            finish_reason: FinishReason {
                unified: UnifiedFinish::Stop,
                raw: Some("completed".into()),
            },
        })
    );
}

#[tokio::test]
async fn a_tool_turn_ends_with_the_call_and_a_tool_calls_finish() {
    let events = events_of("tools.sse").await;
    assert!(events.iter().any(|event| event
        == &ModelEvent::ToolCall {
            id: "call_01Read".into(),
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

/// Both reasoning delta names reach the same block, and the encrypted state
/// comes home so the next turn can replay it.
#[tokio::test]
async fn a_reasoning_turn_streams_both_delta_names_and_returns_its_encrypted_state() {
    let events = events_of("reasoning.sse").await;
    let deltas: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::ReasoningDelta { delta, .. } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        [
            "Summarising: ",
            "weigh the options.",
            "The raw chain of thought."
        ],
        "reading one delta name loses the other model's whole chain of thought"
    );
    let encrypted: Vec<&str> = events
        .iter()
        .filter_map(|event| match event {
            ModelEvent::ReasoningEnd {
                provider_metadata, ..
            } => provider_metadata
                .get(events::PROVIDER)
                .and_then(|mine| mine.get("encrypted_content"))
                .and_then(Value::as_str),
            _ => None,
        })
        .collect();
    assert_eq!(encrypted, ["gAAAAABsummary", "gAAAAABraw"]);
    assert_eq!(
        events.last().and_then(reasoning_tokens),
        Some(768),
        "the thinking tokens are billed apart from the answer"
    );
}

fn reasoning_tokens(event: &ModelEvent) -> Option<u64> {
    match event {
        ModelEvent::Finish { usage, .. } => Some(usage.reasoning_tokens),
        _ => None,
    }
}

#[tokio::test]
async fn a_truncated_turn_finishes_as_length() {
    assert_eq!(
        events_of("incomplete.sse").await.last(),
        Some(&ModelEvent::Finish {
            usage: Usage {
                input_tokens: 30,
                output_tokens: 1024,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            },
            finish_reason: FinishReason {
                unified: UnifiedFinish::Length,
                raw: Some("max_output_tokens".into()),
            },
        })
    );
}

#[tokio::test]
async fn a_failed_response_ends_the_stream_as_a_retryable_failure() {
    let server = MockServer::start().await;
    serve_stream(&server, "failed.sse").await;
    let items = drain(&provider(&server), request()).await;
    let last = items.last().expect("an item").as_ref();
    assert_eq!(
        last.err(),
        Some(&ProviderError::Server {
            status: 500,
            message: "server_error: The server had an error while processing your request.".into(),
        })
    );
    assert!(items[..items.len() - 1].iter().all(Result::is_ok));
}

#[tokio::test]
async fn a_429_reads_the_delay_from_the_header() {
    let body = String::from_utf8(fixture("rate_limited_header.json")).expect("utf-8");
    let error = failure_of(429, &body, Some("12")).await;
    assert_eq!(
        error,
        ProviderError::RateLimited {
            retry_after_ms: Some(12_000)
        }
    );
    assert!(error.retryable());
}

#[tokio::test]
async fn a_429_without_the_header_reads_the_delay_from_the_body() {
    let body = String::from_utf8(fixture("rate_limited_body.json")).expect("utf-8");
    assert_eq!(
        failure_of(429, &body, None).await,
        ProviderError::RateLimited {
            retry_after_ms: Some(2_500)
        }
    );
}

#[tokio::test]
async fn a_400_naming_the_context_length_becomes_an_overflow_the_loop_compacts() {
    let body = String::from_utf8(fixture("context_overflow.json")).expect("utf-8");
    let error = failure_of(400, &body, None).await;
    assert!(
        matches!(&error, ProviderError::ContextOverflow { message }
            if message.starts_with("This model's maximum context length is 400000 tokens.")),
        "{error:?}"
    );
    assert!(!error.retryable(), "overflow is compacted, not retried");
}

#[tokio::test]
async fn a_401_is_an_auth_failure_and_a_503_is_a_retryable_server_error() {
    let unauthorized = failure_of(
        401,
        r#"{"error":{"message":"Incorrect API key provided.","code":"invalid_api_key"}}"#,
        None,
    )
    .await;
    assert_eq!(
        unauthorized,
        ProviderError::Auth {
            message: "Incorrect API key provided.".into()
        }
    );
    assert!(!unauthorized.retryable());

    let unavailable = failure_of(
        503,
        r#"{"error":{"message":"Service temporarily unavailable."}}"#,
        None,
    )
    .await;
    assert_eq!(
        unavailable,
        ProviderError::Server {
            status: 503,
            message: "Service temporarily unavailable.".into()
        }
    );
    assert!(unavailable.retryable());
}

/// A server that accepts the connection and then says nothing must not hang a
/// headless run. Paused time, so the guard costs the suite no wall clock.
#[tokio::test(start_paused = true)]
async fn a_server_that_never_answers_times_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(fixture("text.sse"), "text/event-stream")
                .set_delay(Duration::from_secs(600)),
        )
        .mount(&server)
        .await;
    assert_eq!(
        provider(&server)
            .stream(request(), CancellationToken::new())
            .await
            .err(),
        Some(ProviderError::Timeout)
    );
}

#[tokio::test]
async fn the_body_on_the_wire_is_the_stateless_encoding_the_snapshots_pin() {
    let server = MockServer::start().await;
    serve_stream(&server, "text.sse").await;
    let mut request = request();
    request.tools = vec![ToolSpec {
        name: "Read".into(),
        description: "Read a file.".into(),
        input_schema: json!({ "type": "object" }),
        meta: Default::default(),
    }];
    request.reasoning = Some(Effort::Max);
    drain(&provider(&server), request).await;
    assert_eq!(
        sent_body(&server).await,
        json!({
            "model": MODEL,
            "stream": true,
            "store": false,
            "max_output_tokens": 1024,
            "input": [{
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": "hello" }],
            }],
            "tools": [{
                "type": "function",
                "name": "Read",
                "description": "Read a file.",
                "parameters": { "type": "object" },
                "strict": false,
            }],
            // gpt-5.4 stops at xhigh; `max` would be a 400.
            "reasoning": { "effort": "xhigh", "summary": "auto" },
            "include": ["reasoning.encrypted_content"],
        })
    );
}

/// A reasoning item goes back out exactly as it came in, which is the whole
/// point of a stateless turn.
#[tokio::test]
async fn an_encrypted_reasoning_item_round_trips_over_the_wire() {
    let first = MockServer::start().await;
    serve_stream(&first, "reasoning.sse").await;
    let events = drain(&provider(&first), request()).await;
    let replayed: Vec<ContentPart> = events
        .into_iter()
        .filter_map(|event| match event {
            Ok(ModelEvent::ReasoningEnd {
                provider_metadata, ..
            }) => Some(ContentPart::Reasoning {
                text: "…".into(),
                provider_metadata,
            }),
            _ => None,
        })
        .collect();

    let second = MockServer::start().await;
    serve_stream(&second, "text.sse").await;
    let mut request = request();
    request.messages.push(Message::assistant(replayed));
    drain(&provider(&second), request).await;
    assert_eq!(
        sent_body(&second).await["input"][1],
        json!({
            "type": "reasoning",
            "id": "rs_01",
            "summary": [],
            "encrypted_content": "gAAAAABsummary",
        })
    );
}

async fn sent_body(server: &MockServer) -> Value {
    let sent = server
        .received_requests()
        .await
        .expect("the server records requests");
    assert_eq!(sent.len(), 1);
    serde_json::from_slice(&sent[0].body).expect("json body")
}

/// The subscription endpoint: its own path, its own two headers, and no
/// output budget. Registered in M10 when OAuth exists; encoded now.
#[tokio::test]
async fn the_codex_variant_uses_its_own_path_and_headers() {
    let server = MockServer::start().await;
    let token = codex_token("acc_42");
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", format!("Bearer {token}").as_str()))
        .and(header("originator", ORIGINATOR))
        .and(header("chatgpt-account-id", "acc_42"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(fixture("text.sse"), "text/event-stream"),
        )
        .mount(&server)
        .await;
    let provider =
        OpenAiProvider::with_endpoint(Some(token), server.uri()).with_variant(Variant::Codex);
    drain(&provider, request()).await;
    let body = sent_body(&server).await;
    assert!(body.get("max_output_tokens").is_none());
    assert_eq!(body["store"], json!(false));
    assert_eq!(body["stream"], json!(true));
}

fn codex_token(account: &str) -> String {
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload = json!({ "https://api.openai.com/auth": { "chatgpt_account_id": account } });
    format!(
        "{}.{}.signature",
        URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#),
        URL_SAFE_NO_PAD.encode(payload.to_string())
    )
}

#[tokio::test]
async fn the_model_catalogue_comes_from_the_data_envelope() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .and(header("authorization", format!("Bearer {KEY}").as_str()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [
                { "id": MODEL, "object": "model" },
                { "id": "gpt-5.1", "object": "model" },
            ]
        })))
        .mount(&server)
        .await;
    let models = provider(&server).models().await.expect("a catalogue");
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["gpt-5.1", MODEL]
    );
    assert_eq!(models[0].display, None);
}

#[tokio::test]
async fn a_failing_read_endpoint_is_classified_like_any_other_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            r#"{"error":{"message":"Incorrect API key provided.","code":"invalid_api_key"}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;
    assert_eq!(
        provider(&server).models().await.err(),
        Some(ProviderError::Auth {
            message: "Incorrect API key provided.".into()
        })
    );
}

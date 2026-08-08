//! Anthropic Messages protocol adapter (D33). Absorbed from the pre-D33
//! `api::client` implementation byte-identically: same retry/backoff/timeouts,
//! same 400 context-overflow recompute, same SSE parsing, same error mapping.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::{Deserialize, Serialize};

use super::{backoff, retryable};
use crate::api::contract::{
    AuthStatus, BoxStream, Capabilities, ClientError, NeutralRequest, ProviderClient, StreamEvent,
    SystemBlock, ThinkingLevel,
};
use crate::api::sse::SseParser;
use crate::api::types::{DEFAULT_MAX_TOKENS, Message};

pub const API_BASE: &str = "https://api.anthropic.com";
pub const API_VERSION: &str = "2023-06-01";

pub const MAX_RETRIES: u32 = 5;

/// Overall request timeout (connection + first byte): ends the wait when the
/// server is silent instead of hanging forever. Used for **agent long turns**
/// (streaming) — the feedback-layer 10s/15s does not apply (a turn already
/// has continuous progress feedback); this transport-layer timeout plus user
/// interruption is the backstop (AC-53).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Streaming-body idle timeout: when the server hangs after connecting
/// (sends no events and does not disconnect), headless would block forever —
/// past this silence the stream is judged dead.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Short-sync **read** operation feedback-layer timeout (AC-12/14):
/// list_models / count_tokens, etc. At the deadline the future is dropped
/// (cancelling the underlying reqwest connection) → `TIMEOUT`, primary
/// action = retry.
const SHORT_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Short-sync **write** operation feedback-layer timeout (AC-13/14):
/// complete_text (non-streaming completion), etc. At the deadline the future
/// is dropped → `TIMEOUT`. Dropping a write is best-effort about "the server
/// already applied the write", so retries still want action-level idempotency
/// as a backstop (AC-15).
const SHORT_WRITE_TIMEOUT: Duration = Duration::from_secs(15);

/// Floor for recomputing the output budget on a 400 context overflow.
const FLOOR_OUTPUT_TOKENS: u32 = 3_000;

/// cfg(test) test hook (#14 R3b plan A): hang injection for the short-sync
/// feedback-layer timeouts, serving the AC-12/13/14 deadline behaviour plus
/// the AC-15 write-idempotency assertions. Default 0 = no hang, unrelated
/// tests are unaffected; after a test sets `set_hang`, the three short-sync
/// entry points hang for that duration before sending HTTP, and under
/// fake-timers (`start_paused`) advancing to the deadline triggers
/// `timeout` → `TIMEOUT`. In production builds `maybe_hang` passes through,
/// zero overhead.
#[cfg(test)]
pub(crate) mod test_hooks {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    /// Hang duration in ms (0 = pass through).
    static HANG_MS: AtomicU64 = AtomicU64::new(0);

    /// RAII guard: clears the hang on Drop (does not survive a panic, so it
    /// cannot pollute other tests).
    pub(crate) struct HangGuard;

    impl Drop for HangGuard {
        fn drop(&mut self) {
            HANG_MS.store(0, Ordering::Relaxed);
        }
    }

    /// Set the hang and return a guard.
    pub(crate) fn hang_guard(ms: u64) -> HangGuard {
        HANG_MS.store(ms, Ordering::Relaxed);
        HangGuard
    }

    pub(crate) fn hang() -> Duration {
        Duration::from_millis(HANG_MS.load(Ordering::Relaxed))
    }
}

/// Hang wrapper for the short-sync entry points: hangs first in test builds
/// (simulating a slow network / slow server), passes through in production.
#[cfg(test)]
async fn maybe_hang<F: std::future::Future>(inner: F) -> F::Output {
    tokio::time::sleep(test_hooks::hang()).await;
    inner.await
}

#[cfg(not(test))]
#[inline]
async fn maybe_hang<F: std::future::Future>(inner: F) -> F::Output {
    inner.await
}

/// The currently active endpoint (one per provider instance).
#[derive(Debug, Clone)]
struct Endpoint {
    api_key: String,
    base_url: String,
    /// Whether this endpoint accepts image content blocks.
    supports_images: bool,
}

/// Anthropic Messages wire request. The consumers build [`NeutralRequest`];
/// this is the adapter's serialization of it.
#[derive(Debug, Clone, Serialize)]
struct WireRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    system: Vec<WireSystemBlock>,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<serde_json::Value>,
    stream: bool,
    /// Thinking config: `{"type":"adaptive"}` (None = send no parameter).
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<serde_json::Value>,
    /// Output config: `{"effort": <level>}` (None = send no parameter).
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<serde_json::Value>,
}

impl WireRequest {
    fn from_neutral(request: &NeutralRequest) -> Self {
        Self {
            model: request.model.clone(),
            max_tokens: request.max_tokens,
            system: request
                .system
                .iter()
                .map(|b| WireSystemBlock {
                    text: b.text.clone(),
                    cache: b.cache,
                })
                .collect(),
            messages: request.messages.clone(),
            tools: request.tools.clone(),
            stream: request.stream,
            thinking: wire_thinking(request.thinking),
            output_config: wire_effort(request.thinking),
        }
    }
}

/// A single system-prompt block (may carry cache_control).
#[derive(Debug, Clone)]
struct WireSystemBlock {
    text: String,
    /// Carry `cache_control: ephemeral` on the API request.
    cache: bool,
}

impl Serialize for WireSystemBlock {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut block = serde_json::json!({
            "type": "text",
            "text": self.text,
        });
        if self.cache {
            block["cache_control"] = serde_json::json!({"type": "ephemeral"});
        }
        block.serialize(serializer)
    }
}

/// Thinking level → request `thinking` parameter.
///
/// The Claude 5 family (including the default `claude-sonnet-5`) rejects
/// `{"type":"enabled","budget_tokens":N}` with a 400 — `adaptive` is the only
/// on-mode, so every enabled level sends the same adaptive shape; depth goes
/// through [`wire_effort`] instead. None sends no parameter at all (keeps
/// DeepSeek/ollama endpoints happy).
fn wire_thinking(level: Option<ThinkingLevel>) -> Option<serde_json::Value> {
    level.map(|_| serde_json::json!({ "type": "adaptive" }))
}

/// Thinking level → request `output_config` parameter (`{"effort": <level>}`)。
///
/// Same gating as [`wire_thinking`]: None sends no parameter; the level is
/// the effort level (a GA parameter of the Claude 5 family — below `high`
/// saves tokens, xhigh/max think deeper).
fn wire_effort(level: Option<ThinkingLevel>) -> Option<serde_json::Value> {
    level.map(|l| serde_json::json!({ "effort": l.as_str() }))
}

#[derive(Debug, Deserialize)]
struct MessageStartPayload {
    message: MessageStartInner,
}

#[derive(Debug, Deserialize)]
struct MessageStartInner {
    id: String,
    model: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DeltaPayload {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "signature_delta")]
    Signature { signature: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
}

#[derive(Debug, Deserialize)]
struct ContentBlockDeltaPayload {
    index: usize,
    delta: DeltaPayload,
}

#[derive(Debug, Deserialize)]
struct MessageDeltaPayload {
    delta: MessageDeltaInner,
    #[serde(default)]
    usage: Option<UsagePayload>,
}

#[derive(Debug, Deserialize)]
struct UsagePayload {
    #[serde(rename = "output_tokens", default)]
    output_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct MessageDeltaInner {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorEventPayload {
    error: ErrorInner,
}

#[derive(Debug, Deserialize)]
struct ErrorInner {
    #[serde(rename = "type")]
    kind: String,
    message: String,
}

fn parse_content_block_start(data: &str) -> Result<Option<StreamEvent>, String> {
    let value: serde_json::Value =
        serde_json::from_str(data).map_err(|e| format!("bad content_block_start: {e}"))?;
    let index = value.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
    let block = value
        .get("content_block")
        .ok_or("content_block_start without content_block")?;
    let kind = block
        .get("type")
        .and_then(|t| t.as_str())
        .ok_or("content_block without type")?;
    Ok(Some(match kind {
        "text" => StreamEvent::TextStart { index },
        "thinking" => StreamEvent::ThinkingStart { index },
        "tool_use" => StreamEvent::ToolUseStart {
            index,
            id: block
                .get("id")
                .and_then(|v| v.as_str())
                .ok_or("tool_use without id")?
                .to_string(),
            name: block
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or("tool_use without name")?
                .to_string(),
        },
        _other => return Ok(None),
    }))
}

/// Parse one Anthropic SSE event/data pair into a [`StreamEvent`].
pub fn parse_sse_event(event: &str, data: &str) -> Result<Option<StreamEvent>, String> {
    match event {
        "message_start" => {
            let p: MessageStartPayload =
                serde_json::from_str(data).map_err(|e| format!("bad message_start: {e}"))?;
            Ok(Some(StreamEvent::MessageStart {
                id: p.message.id,
                model: p.message.model,
            }))
        }
        "content_block_start" => parse_content_block_start(data),
        "content_block_delta" => {
            let p: ContentBlockDeltaPayload =
                serde_json::from_str(data).map_err(|e| format!("bad content_block_delta: {e}"))?;
            let ev = match p.delta {
                DeltaPayload::Text { text } => StreamEvent::TextDelta {
                    index: p.index,
                    text,
                },
                DeltaPayload::Thinking { thinking } => StreamEvent::ThinkingDelta {
                    index: p.index,
                    thinking,
                },
                DeltaPayload::Signature { signature } => StreamEvent::SignatureDelta {
                    index: p.index,
                    signature,
                },
                DeltaPayload::InputJson { partial_json } => StreamEvent::InputJsonDelta {
                    index: p.index,
                    partial_json,
                },
            };
            Ok(Some(ev))
        }
        "content_block_stop" => {
            let index = serde_json::from_str::<serde_json::Value>(data)
                .ok()
                .and_then(|v| v.get("index").and_then(|i| i.as_u64()))
                .unwrap_or(0) as usize;
            Ok(Some(StreamEvent::BlockStop { index }))
        }
        "message_delta" => {
            let p: MessageDeltaPayload =
                serde_json::from_str(data).map_err(|e| format!("bad message_delta: {e}"))?;
            Ok(Some(StreamEvent::StopReason {
                stop_reason: p.delta.stop_reason,
                output_tokens: p.usage.map(|u| u.output_tokens),
            }))
        }
        "message_stop" => Ok(Some(StreamEvent::Done)),
        "ping" => Ok(None),
        "error" => {
            let p: ErrorEventPayload =
                serde_json::from_str(data).map_err(|e| format!("bad error event: {e}"))?;
            Ok(Some(StreamEvent::ApiError {
                message: format!("{}: {}", p.error.kind, p.error.message),
            }))
        }
        _other => Ok(None), // unknown event type: ignore, stay forward-compatible
    }
}

/// Anthropic Messages provider. One instance per endpoint; the `Client`
/// facade swaps instances on `/provider` switches.
#[derive(Debug, Clone)]
pub struct AnthropicProvider {
    http: reqwest::Client,
    endpoint: Arc<std::sync::RwLock<Endpoint>>,
}

impl AnthropicProvider {
    pub fn new(
        http: reqwest::Client,
        api_key: String,
        base_url: String,
        supports_images: bool,
    ) -> Self {
        Self {
            http,
            endpoint: Arc::new(std::sync::RwLock::new(Endpoint {
                api_key,
                base_url,
                supports_images,
            })),
        }
    }

    fn headers(&self) -> Result<HeaderMap, ClientError> {
        let endpoint = self.endpoint.read().unwrap_or_else(|p| p.into_inner());
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&endpoint.api_key)
                .map_err(|e| ClientError::InvalidApiKey(e.to_string()))?,
        );
        headers.insert("anthropic-version", HeaderValue::from_static(API_VERSION));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }

    /// Start a streaming request, returning a normalized event stream.
    async fn stream_inner(&self, request: &WireRequest) -> Result<BoxStream, ClientError> {
        // The 400 context-overflow recompute needs to mutate max_tokens →
        // clone a mutable request.
        let mut request = request.clone();
        let mut attempt = 0;
        let base_url = self
            .endpoint
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .base_url
            .clone();
        loop {
            let builder = self
                .http
                .post(format!("{base_url}/v1/messages"))
                .headers(self.headers()?)
                .json(&request);
            match tokio::time::timeout(REQUEST_TIMEOUT, builder.send()).await {
                Ok(Ok(response)) if response.status().is_success() => {
                    return Ok(Box::pin(Self::stream_body(response)));
                }
                Ok(Ok(response)) if retryable(&response.status()) => {
                    let status = response.status();
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(Duration::from_secs);
                    let body = response.text().await.unwrap_or_default();
                    if attempt >= MAX_RETRIES {
                        return Err(ClientError::Api {
                            status: status.as_u16(),
                            body,
                        });
                    }
                    let delay = retry_after.unwrap_or_else(|| backoff(attempt));
                    tokio::time::sleep(delay).await;
                }
                Ok(Ok(response)) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    // 400 output-budget overflow: per "input length and max_tokens exceed context limit: A + B > C"
                    // recompute max_tokens = max(3000, C − A − 1000) and retry once.
                    if status.as_u16() == 400
                        && attempt == 0
                        && body.contains("exceed context limit")
                        && let Some((input, window)) = parse_context_limit(&body)
                        && let Some(recomputed) = window
                            .checked_sub(input)
                            .and_then(|rem| rem.checked_sub(1000))
                            .map(|rem| rem.max(FLOOR_OUTPUT_TOKENS as u64))
                            .map(|v| v.min(DEFAULT_MAX_TOKENS as u64))
                        && recomputed != request.max_tokens as u64
                    {
                        request.max_tokens = recomputed as u32;
                        attempt += 1;
                        continue;
                    }
                    return Err(ClientError::Api {
                        status: status.as_u16(),
                        body,
                    });
                }
                Ok(Err(_transport)) if attempt < MAX_RETRIES => {
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Ok(Err(transport)) => return Err(ClientError::Transport(transport)),
                Err(_) => {
                    return Err(ClientError::Timeout);
                }
            }
            attempt += 1;
        }
    }

    /// Non-streaming completion's inner implementation (retries + response
    /// parsing). Backed by the outer feedback-layer timeout; a single network
    /// send is not separately timed (the outer 15s is the strongest guard).
    async fn complete_text_inner(&self, request: &WireRequest) -> Result<String, ClientError> {
        let mut request = request.clone();
        request.stream = false;
        let base_url = self
            .endpoint
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .base_url
            .clone();
        let mut attempt = 0;
        let response = loop {
            let builder = self
                .http
                .post(format!("{base_url}/v1/messages"))
                .headers(self.headers()?)
                .json(&request);
            match builder.send().await {
                Ok(response) if response.status().is_success() => break response,
                Ok(response) if retryable(&response.status()) && attempt < MAX_RETRIES => {
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(Duration::from_secs);
                    tokio::time::sleep(retry_after.unwrap_or_else(|| backoff(attempt))).await;
                }
                Ok(response) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    return Err(ClientError::Api {
                        status: status.as_u16(),
                        body,
                    });
                }
                Err(_transport) if attempt < MAX_RETRIES => {
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Err(transport) => return Err(ClientError::Transport(transport)),
            }
            attempt += 1;
        };
        let body: serde_json::Value = response.json().await?;
        let mut text = String::new();
        if let Some(blocks) = body.get("content").and_then(|c| c.as_array()) {
            for block in blocks {
                if let Some(t) = block
                    .get("text")
                    .and_then(|t| t.as_str())
                    .filter(|_| block.get("type").and_then(|t| t.as_str()) == Some("text"))
                {
                    text.push_str(t);
                }
            }
        }
        Ok(text)
    }

    fn stream_body(
        response: reqwest::Response,
    ) -> impl futures_util::Stream<Item = Result<StreamEvent, ClientError>> {
        let mut parser = SseParser::new();
        let mut body = response.bytes_stream();
        async_stream::stream! {
            loop {
                let chunk = match next_with_idle(&mut body, STREAM_IDLE_TIMEOUT).await {
                    Ok(Some(Ok(chunk))) => chunk,
                    Ok(Some(Err(e))) => {
                        yield Err(ClientError::Transport(e));
                        return;
                    }
                    Ok(None) => break,
                    Err(e) => {
                        yield Err(e);
                        return;
                    }
                };
                let frames = match parser.feed(&chunk) {
                    Ok(frames) => frames,
                    Err(message) => {
                        yield Err(ClientError::Stream(message));
                        return;
                    }
                };
                for frame in frames {
                    match parse_sse_event(&frame.event, &frame.data) {
                        Ok(Some(event)) => yield Ok(event),
                        Ok(None) => {}
                        Err(message) => {
                            yield Err(ClientError::Stream(message));
                            return;
                        }
                    }
                }
            }
        }
    }
}

#[async_trait]
impl ProviderClient for AnthropicProvider {
    fn capabilities(&self) -> Capabilities {
        let supports_images = self
            .endpoint
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .supports_images;
        Capabilities {
            supports_images,
            ..Default::default()
        }
    }

    fn auth_status(&self) -> AuthStatus {
        AuthStatus::ApiKey
    }

    async fn stream(&self, request: &NeutralRequest) -> Result<BoxStream, ClientError> {
        let wire = WireRequest::from_neutral(request);
        self.stream_inner(&wire).await
    }

    async fn complete_text(&self, request: &NeutralRequest) -> Result<String, ClientError> {
        let wire = WireRequest::from_neutral(request);
        tokio::time::timeout(
            SHORT_WRITE_TIMEOUT,
            maybe_hang(self.complete_text_inner(&wire)),
        )
        .await
        .map_err(|_| ClientError::Timeout)?
    }

    async fn list_models(&self) -> Result<Vec<String>, ClientError> {
        let base_url = self
            .endpoint
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .base_url
            .clone();
        let response = tokio::time::timeout(
            SHORT_READ_TIMEOUT,
            maybe_hang(
                self.http
                    .get(format!("{base_url}/v1/models"))
                    .headers(self.headers()?)
                    .send(),
            ),
        )
        .await
        .map_err(|_| ClientError::Timeout)??;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ClientError::Api {
                status: status.as_u16(),
                body,
            });
        }
        let body: serde_json::Value = response.json().await?;
        let mut models: Vec<String> = body
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|id| id.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        models.sort();
        Ok(models)
    }

    async fn count_tokens(
        &self,
        model: &str,
        system: &[SystemBlock],
        messages: &[Message],
    ) -> Result<u64, ClientError> {
        let payload = serde_json::json!({
            "model": model,
            "system": system.iter().map(|b| WireSystemBlock { text: b.text.clone(), cache: b.cache }).collect::<Vec<_>>(),
            "messages": messages,
        });
        let base_url = self
            .endpoint
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .base_url
            .clone();
        let response = tokio::time::timeout(
            SHORT_READ_TIMEOUT,
            maybe_hang(
                self.http
                    .post(format!("{base_url}/v1/messages/count_tokens"))
                    .headers(self.headers()?)
                    .json(&payload)
                    .send(),
            ),
        )
        .await
        .map_err(|_| ClientError::Timeout)??;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ClientError::Api {
                status: status.as_u16(),
                body,
            });
        }
        let body: serde_json::Value = response.json().await?;
        Ok(body
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0))
    }
}

/// Idle-timeout wrapper for `stream.next()`: if not a single event arrives
/// within the idle period, the stream is judged dead — so headless does not
/// block forever when the server hangs.
async fn next_with_idle<S, T>(body: &mut S, idle: Duration) -> Result<Option<T>, ClientError>
where
    S: futures_util::Stream<Item = T> + Unpin,
{
    tokio::time::timeout(idle, body.next())
        .await
        .map_err(|_| ClientError::Stream(format!("no stream data for {idle:?}: server stalled")))
}

/// Parse (input_tokens, context_window) from a 400 error body: locates the
/// three numbers adjacent to the "A + B > C" pattern, instead of taking the
/// full text's third-from-last number (request-id etc. would pollute it). A
/// segment that fails to parse (overflow / missing field) only skips that
/// candidate, never turns the whole result into None.
fn parse_context_limit(body: &str) -> Option<(u64, u64)> {
    // "input length and max_tokens exceed context limit: 12345 + 64000 > 200000"
    for (idx, _) in body.match_indices('>') {
        let Some(window) = leading_number(&body[idx + 1..]) else {
            continue;
        };
        let head = body[..idx].trim_end();
        let Some((_budget, digits)) = trailing_number(head) else {
            continue;
        };
        let Some(head) = head[..head.len() - digits].trim_end().strip_suffix('+') else {
            continue;
        };
        let Some((input, _)) = trailing_number(head.trim_end()) else {
            continue;
        };
        if input < window {
            return Some((input, window));
        }
    }
    None
}

/// The integer starting the text after skipping leading whitespace.
fn leading_number(text: &str) -> Option<u64> {
    let text = text.trim_start();
    let digits = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    text.get(..digits)?.parse().ok()
}

/// The trailing integer and its byte length.
fn trailing_number(text: &str) -> Option<(u64, usize)> {
    let digits = text.len() - text.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    let value: u64 = text.get(text.len() - digits..)?.parse().ok()?;
    Some((value, digits))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC-12/13/14: short-sync feedback-layer timeouts are tiered — read
    /// 10s / write 15s, never confused (read must fire before 11s and write
    /// must not fire before 14s, guaranteed by the constant values;
    /// list_models/count_tokens use the read tier, complete_text the write
    /// tier — see the implementations; stream long turns do not use the
    /// feedback layer — see AC-53).
    #[test]
    fn feedback_timeout_tiers_are_read_10s_write_15s() {
        assert_eq!(SHORT_READ_TIMEOUT, Duration::from_secs(10));
        assert_eq!(SHORT_WRITE_TIMEOUT, Duration::from_secs(15));
        // Long-turn transport guards stay 120s/60s (no 10s/15s).
        assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(120));
        assert_eq!(STREAM_IDLE_TIMEOUT, Duration::from_secs(60));
    }

    /// AC-12 deadline behaviour (plan A + fake-timers): a short-sync read
    /// lands on `TIMEOUT` at 10s.
    #[tokio::test(start_paused = true)]
    async fn read_times_out_at_10s() {
        let _guard = test_hooks::hang_guard(60_000); // hang 60s, > 10s read tier
        let provider = AnthropicProvider::new(
            reqwest::Client::new(),
            "k".into(),
            "https://example.com".into(),
            false,
        );
        let handle = tokio::spawn(async move { provider.list_models().await });
        tokio::time::advance(Duration::from_secs(11)).await;
        let res = handle.await.unwrap();
        assert!(
            matches!(res, Err(ClientError::Timeout)),
            "读超时应落 TIMEOUT"
        );
    }

    /// AC-13/14 deadline behaviour + tiering not confused: a write must not
    /// fire before 14s and lands on `TIMEOUT` at 15s.
    #[tokio::test(start_paused = true)]
    async fn write_times_out_at_15s_not_before_14s() {
        let _guard = test_hooks::hang_guard(60_000); // hang 60s, > 15s write tier
        let provider = AnthropicProvider::new(
            reqwest::Client::new(),
            "k".into(),
            "https://example.com".into(),
            false,
        );
        let handle = tokio::spawn(async move {
            let req = NeutralRequest {
                model: "test".into(),
                max_tokens: 100,
                system: vec![],
                messages: vec![],
                tools: vec![],
                stream: false,
                thinking: None,
            };
            provider.complete_text(&req).await
        });
        // AC-14: a write does not fire before 14s (the read tier's 10s
        // already passed and must not trip the write tier).
        tokio::time::advance(Duration::from_secs(14)).await;
        assert!(!handle.is_finished(), "写操作 14s 前不应超时");
        // At 16s the write tier fires.
        tokio::time::advance(Duration::from_secs(2)).await;
        let res = handle.await.unwrap();
        assert!(
            matches!(res, Err(ClientError::Timeout)),
            "写超时应落 TIMEOUT"
        );
    }

    #[test]
    fn parses_context_limit_error() {
        let body = "400: input length and max_tokens exceed context limit: 12345 + 64000 > 200000";
        assert_eq!(parse_context_limit(body), Some((12345, 200000)));
    }

    #[test]
    fn rejects_malformed_context_limit() {
        assert_eq!(parse_context_limit("boom 42"), None);
        assert_eq!(parse_context_limit("400: overloaded"), None);
        // A >= C is impossible: protective rejection
        assert_eq!(parse_context_limit("900000 + 64000 > 200000"), None);
    }

    /// Unrelated numbers (request-id etc.) and overflowing numbers must not
    /// pollute the parse.
    #[test]
    fn context_limit_ignores_unrelated_numbers() {
        let body = concat!(
            r#"{"request_id":"req_0129384756","error":{"type":"invalid_request_error","#,
            r#""message":"input length and max_tokens exceed context limit: 150000 + 64000 > 200000"}}"#
        );
        assert_eq!(parse_context_limit(body), Some((150000, 200000)));

        // An unrelated u64-overflowing number only skips that segment, never
        // turns the whole result into None.
        let overflowing = "trace 99999999999999999999999 \
             input length and max_tokens exceed context limit: 150000 + 64000 > 200000";
        assert_eq!(parse_context_limit(overflowing), Some((150000, 200000)));
    }

    /// Once connected, the server sends no more events: the idle timeout
    /// declares the stream dead instead of blocking forever.
    #[tokio::test]
    async fn idle_stream_times_out() {
        let mut stalled = futures_util::stream::pending::<u8>();
        let err = next_with_idle(&mut stalled, Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ClientError::Stream(m) if m.contains("server stalled")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn live_stream_passes_items_through() {
        let mut live = futures_util::stream::iter([1u8, 2]);
        let idle = Duration::from_secs(30);
        assert_eq!(next_with_idle(&mut live, idle).await.unwrap(), Some(1));
        assert_eq!(next_with_idle(&mut live, idle).await.unwrap(), Some(2));
        assert_eq!(
            next_with_idle(&mut live, idle).await.unwrap(),
            None,
            "流结束返回 None 而不是超时"
        );
    }

    /// The Claude 5 family only accepts `adaptive`; the budget_tokens shape
    /// is a 400 everywhere.
    #[test]
    fn wire_thinking_is_adaptive_for_every_enabled_level() {
        for level in [
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::Xhigh,
            ThinkingLevel::Max,
        ] {
            let param = wire_thinking(Some(level)).unwrap();
            assert_eq!(
                param,
                serde_json::json!({ "type": "adaptive" }),
                "{level:?}"
            );
            assert!(
                param.get("budget_tokens").is_none(),
                "{level:?} 不得带 budget"
            );
        }
    }

    #[test]
    fn wire_thinking_omitted_when_off_or_unset() {
        assert_eq!(wire_thinking(None), None, "未配置不发参数");
    }

    /// Levels map to effort levels; None suppresses both.
    #[test]
    fn wire_effort_follows_thinking_gate() {
        assert_eq!(
            wire_effort(Some(ThinkingLevel::Xhigh)),
            Some(serde_json::json!({ "effort": "xhigh" }))
        );
        assert_eq!(wire_effort(None), None);
    }

    #[test]
    fn wire_request_serializes_thinking_only_when_set() {
        let mut req = NeutralRequest {
            model: "m".into(),
            max_tokens: 100,
            system: Vec::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            stream: true,
            thinking: None,
        };
        let json = serde_json::to_value(WireRequest::from_neutral(&req)).unwrap();
        assert!(json.get("thinking").is_none(), "无 thinking 不序列化");
        assert!(
            json.get("output_config").is_none(),
            "无 output_config 不序列化"
        );
        req.thinking = Some(ThinkingLevel::Xhigh);
        let json = serde_json::to_value(WireRequest::from_neutral(&req)).unwrap();
        assert_eq!(json["thinking"], serde_json::json!({ "type": "adaptive" }));
        assert_eq!(
            json["output_config"],
            serde_json::json!({ "effort": "xhigh" })
        );
    }

    #[test]
    fn parses_text_delta() {
        let ev = parse_sse_event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hello"}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            StreamEvent::TextDelta {
                index: 1,
                text: "Hello".into()
            }
        );
    }

    #[test]
    fn parses_tool_use_start() {
        let ev = parse_sse_event(
            "content_block_start",
            r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"tu_1","name":"Bash","input":{}}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            StreamEvent::ToolUseStart {
                index: 2,
                id: "tu_1".into(),
                name: "Bash".into()
            }
        );
    }

    #[test]
    fn parses_message_delta_stop_reason() {
        let ev = parse_sse_event(
            "message_delta",
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":42}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            StreamEvent::StopReason {
                stop_reason: Some("end_turn".into()),
                output_tokens: Some(42)
            }
        );
    }

    #[test]
    fn parses_error_event() {
        let ev = parse_sse_event(
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            ev,
            StreamEvent::ApiError {
                message: "overloaded_error: Overloaded".into()
            }
        );
    }

    #[test]
    fn ignores_ping_and_unknown() {
        assert_eq!(parse_sse_event("ping", "{}").unwrap(), None);
        assert_eq!(parse_sse_event("weird_event", "{}").unwrap(), None);
    }

    /// System blocks with the cache flag serialize cache_control; without it
    /// the block is plain text.
    #[test]
    fn wire_system_block_cache_control() {
        let wire = WireSystemBlock {
            text: "sys".into(),
            cache: true,
        };
        let json = serde_json::to_value(&wire).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"type": "text", "text": "sys", "cache_control": {"type": "ephemeral"}})
        );
        let wire = WireSystemBlock {
            text: "sys".into(),
            cache: false,
        };
        let json = serde_json::to_value(&wire).unwrap();
        assert_eq!(json, serde_json::json!({"type": "text", "text": "sys"}));
    }
}

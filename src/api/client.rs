use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use std::sync::Arc;
use thiserror::Error;

use crate::error::ErrorCode;

use super::sse::SseParser;
use super::types::{parse_sse_event, Request, StreamEvent, API_BASE, API_VERSION};
use super::types::{ContentBlock, DEFAULT_MAX_TOKENS, Role, SystemBlock};

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

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("missing API key: set ANTHROPIC_API_KEY or DEEPSEEK_API_KEY")]
    MissingApiKey,
    #[error("invalid API key for HTTP header: {0}")]
    InvalidApiKey(String),
    #[error("API error: HTTP {status}: {body}")]
    Api { status: u16, body: String },
    #[error("API stream error: {0}")]
    Stream(String),
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    /// The server gave no response within REQUEST_TIMEOUT.
    #[error("request timed out after {REQUEST_TIMEOUT:?}")]
    Timeout,
}

impl ErrorCode for ClientError {
    /// Outbound mapping (see the `src/error.rs` code table): every variant
    /// explicitly returns a stable code, and the match is exhaustive with no
    /// `_` arm — a new variant that is not handled fails to compile.
    fn error_code(&self) -> &'static str {
        match self {
            ClientError::MissingApiKey | ClientError::InvalidApiKey(_) => "AUTH_REQUIRED",
            ClientError::Api { status: 401, .. } => "AUTH_REQUIRED",
            ClientError::Api { status: 403, .. } => "PERMISSION_DENIED",
            ClientError::Api { status: 429, .. } => "RATE_LIMITED",
            // Remaining non-success responses (4xx outside the above / 5xx):
            // server-interaction anomaly, action = "retry later".
            ClientError::Api { .. } => "SERVER_ERROR",
            ClientError::Stream(_) => "SERVER_ERROR",
            ClientError::Transport(_) => transport_offline_code(),
            ClientError::Timeout => "TIMEOUT",
        }
    }
}

/// Locks in the Transport-arm mapping (`#[doc(hidden)]`, only for the
/// drift-guard unit test to assert).
///
/// Why it exists: `reqwest::Error` has no public constructor (its 0.13.x
/// `new`/`builder` are all `pub(crate)`), so the `ClientError::Transport`
/// variant cannot be constructed at runtime in tests, and the drift-guard
/// unit test cannot enumerate that variant directly — this function locks
/// the "transport → OFFLINE" mapping instead.
#[doc(hidden)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn transport_offline_code() -> &'static str {
    "OFFLINE"
}

/// The currently active endpoint (updated on /provider switch).
#[derive(Debug, Clone)]
struct Endpoint {
    api_key: String,
    base_url: String,
    /// Whether this endpoint accepts image content blocks (default reads the
    /// top-level sendImages; named providers read supportsImages).
    supports_images: bool,
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: Arc<std::sync::RwLock<Endpoint>>,
    /// Named-provider table (settings.providers; default is not in the
    /// table).
    providers: std::collections::HashMap<String, Endpoint>,
}

impl Client {
    /// Settings first, falling back to environment variables
    /// (ANTHROPIC_API_KEY/DEEPSEEK_API_KEY, ANTHROPIC_BASE_URL). Reports
    /// MissingApiKey when neither settings nor env has a key.
    pub fn from_settings(settings: &crate::settings::Settings) -> Result<Self, ClientError> {
        Self::from_settings_with(settings, |name| std::env::var(name))
    }

    /// Injectable variant of from_settings (tests use a fake env, avoiding
    /// real environment variables).
    fn from_settings_with(
        settings: &crate::settings::Settings,
        env: impl Fn(&str) -> std::result::Result<String, std::env::VarError>,
    ) -> Result<Self, ClientError> {
        let api_key = settings
            .api_key
            .clone()
            .or_else(|| env("ANTHROPIC_API_KEY").ok())
            .or_else(|| env("DEEPSEEK_API_KEY").ok())
            .ok_or(ClientError::MissingApiKey)?;
        let base_url = settings.api_base_url.clone().unwrap_or_else(|| {
            env("ANTHROPIC_BASE_URL").unwrap_or_else(|_| API_BASE.to_string())
        });
        let providers = settings
            .providers
            .iter()
            .map(|(name, cfg)| {
                (
                    name.clone(),
                    Endpoint {
                        api_key: cfg.api_key.clone(),
                        base_url: cfg.api_base_url.clone(),
                        supports_images: cfg.supports_images.unwrap_or(false),
                    },
                )
            })
            .collect();
        Ok(Self {
            http: reqwest::Client::new(),
            endpoint: Arc::new(std::sync::RwLock::new(Endpoint {
                api_key,
                base_url,
                supports_images: settings.send_images.unwrap_or(false),
            })),
            providers,
        })
    }

    #[cfg(test)]
    pub fn new(api_key: String, base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            endpoint: Arc::new(std::sync::RwLock::new(Endpoint {
                api_key,
                base_url,
                supports_images: false,
            })),
            providers: std::collections::HashMap::new(),
        }
    }

    /// Named-provider list (default excluded; for the /provider listing).
    pub fn provider_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.providers.keys().cloned().collect();
        names.sort();
        names
    }

    /// The currently active provider endpoint (key/url references).
    pub fn current_endpoint(&self) -> (String, String) {
        let e = self.endpoint.read().unwrap_or_else(|p| p.into_inner());
        (e.api_key.clone(), e.base_url.clone())
    }

    /// Whether the current endpoint accepts image content blocks
    /// (`supportsImages`/`sendImages` config).
    pub fn supports_images(&self) -> bool {
        self.endpoint.read().unwrap_or_else(|p| p.into_inner()).supports_images
    }

    /// Switch to a named provider; unknown names error out (default can
    /// always be switched back to).
    pub fn set_provider(&self, name: &str) -> Result<(), String> {
        let Some(endpoint) = self.providers.get(name).cloned() else {
            return Err(format!("未找到 provider \"{name}\"（/provider 查看列表）"));
        };
        *self.endpoint.write().unwrap_or_else(|p| p.into_inner()) = endpoint;
        Ok(())
    }

    /// Derive an endpoint-independent Client (for sub-agents that pin a
    /// provider): the new Client locks that provider's endpoint, and the
    /// providers table is shared (same name table). Without a provider you
    /// should just clone (shared endpoint, follows the parent session's
    /// switches).
    pub fn with_provider(&self, name: &str) -> Result<Client, String> {
        let endpoint = self
            .providers
            .get(name)
            .cloned()
            .ok_or_else(|| format!("未找到 provider \"{name}\"（/provider 查看列表）"))?;
        Ok(Client {
            http: self.http.clone(),
            endpoint: Arc::new(std::sync::RwLock::new(endpoint)),
            providers: self.providers.clone(),
        })
    }

    fn headers(&self) -> Result<HeaderMap, ClientError> {
        let endpoint = self.endpoint.read().unwrap_or_else(|p| p.into_inner());
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-api-key",
            HeaderValue::from_str(&endpoint.api_key)
                .map_err(|e| ClientError::InvalidApiKey(e.to_string()))?,
        );
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(API_VERSION),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }

    /// Start a streaming request, returning a normalized event stream.
    pub async fn stream(
        &self,
        request: &Request,
    ) -> Result<impl futures_util::Stream<Item = Result<StreamEvent, ClientError>>, ClientError>
    {
        // The 400 context-overflow recompute needs to mutate max_tokens →
        // clone a mutable request.
        let mut request = request.clone();
        let mut attempt = 0;
        let base_url = self.current_endpoint().1;
        loop {
            let builder = self
                .http
                .post(format!("{base_url}/v1/messages"))
                .headers(self.headers()?)
                .json(&request);
            match tokio::time::timeout(REQUEST_TIMEOUT, builder.send()).await {
                Ok(Ok(response)) if response.status().is_success() => {
                    return Ok(self.stream_body(response));
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
                        return Err(ClientError::Api { status: status.as_u16(), body });
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
                    return Err(ClientError::Api { status: status.as_u16(), body });
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

    /// Non-streaming completion: returns the reply text (for compact
    /// summaries, memory extraction). Backoff retries consistent with
    /// `stream`: 429/5xx and transient transport errors must not directly
    /// count as a compression failure (failures accumulate into the circuit
    /// breaker). Short-sync write operation: the whole operation (including
    /// retries) is under the feedback-layer 15s (AC-13/14); at the deadline
    /// the future is dropped → `TIMEOUT`.
    pub async fn complete_text(
        &self,
        request: &Request,
    ) -> Result<String, ClientError> {
        tokio::time::timeout(SHORT_WRITE_TIMEOUT, maybe_hang(self.complete_text_inner(request)))
            .await
            .map_err(|_| ClientError::Timeout)?
    }

    /// complete_text's inner implementation (retries + response parsing).
    /// Backed by the outer feedback-layer timeout; a single network send is
    /// not separately timed (the outer 15s is the strongest guard).
    async fn complete_text_inner(
        &self,
        request: &Request,
    ) -> Result<String, ClientError> {
        let mut request = request.clone();
        request.stream = false;
        let base_url = self.current_endpoint().1;
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
                    return Err(ClientError::Api { status: status.as_u16(), body });
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

    /// List the models the current endpoint supports
    /// (`GET {base}/v1/models`, common to Anthropic/DeepSeek): returns
    /// `data[].id`. Used by the `/model` secondary selector's async fetch.
    /// Short-sync read operation: feedback-layer 10s (AC-12/14), drop at the
    /// deadline → `TIMEOUT`.
    pub async fn list_models(&self) -> Result<Vec<String>, ClientError> {
        let base_url = self.current_endpoint().1;
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

    /// Input token count (D12: the budget display goes through the official
    /// count_tokens API). Short-sync read operation: feedback-layer 10s
    /// (AC-12/14), drop at the deadline → `TIMEOUT`.
    pub async fn count_tokens(
        &self,
        model: &str,
        system: &[SystemBlock],
        messages: &[super::types::Message],
    ) -> Result<u64, ClientError> {
        let payload = serde_json::json!({
            "model": model,
            "system": system,
            "messages": messages,
        });
        let response = tokio::time::timeout(
            SHORT_READ_TIMEOUT,
            maybe_hang(
                self.http
                    .post(format!("{}/v1/messages/count_tokens", self.current_endpoint().1))
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

    fn stream_body(
        &self,
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

/// Idle-timeout wrapper for `stream.next()`: if not a single event arrives
/// within the idle period, the stream is judged dead — so headless does not
/// block forever when the server hangs.
async fn next_with_idle<S, T>(
    body: &mut S,
    idle: Duration,
) -> Result<Option<T>, ClientError>
where
    S: futures_util::Stream<Item = T> + Unpin,
{
    tokio::time::timeout(idle, body.next())
        .await
        .map_err(|_| ClientError::Stream(format!("no stream data for {idle:?}: server stalled")))
}

/// Exponential backoff + jitter: from 500ms, capped at 32s.
fn backoff(attempt: u32) -> Duration {
    let base_ms = (500u64 << attempt.min(6)).min(32_000);
    let jitter = rand_jitter(base_ms);
    Duration::from_millis(base_ms + jitter)
}

fn rand_jitter(scale: u64) -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    nanos % (scale / 2 + 1)
}

fn retryable(status: &reqwest::StatusCode) -> bool {
    status.is_server_error() || *status == reqwest::StatusCode::TOO_MANY_REQUESTS
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

/// Accumulates a complete assistant reply's stream events into a returnable
/// message.
#[derive(Debug, Default)]
pub struct AssistantAccumulator {
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    in_flight: Option<InFlight>,
}

#[derive(Debug)]
enum InFlight {
    Text { text: String },
    Thinking { thinking: String, signature: String },
    ToolUse { id: String, name: String, input: String },
}

impl AssistantAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, event: &StreamEvent) -> Result<(), String> {
        match event {
            StreamEvent::TextStart { index } => {
                self.ensure_slot(*index)?;
                self.in_flight = Some(InFlight::Text { text: String::new() });
            }
            StreamEvent::ThinkingStart { index } => {
                self.ensure_slot(*index)?;
                self.in_flight = Some(InFlight::Thinking {
                    thinking: String::new(),
                    signature: String::new(),
                });
            }
            StreamEvent::ToolUseStart { index, id, name } => {
                self.ensure_slot(*index)?;
                self.in_flight = Some(InFlight::ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: String::new(),
                });
            }
            StreamEvent::TextDelta { index, text } => {
                self.push_delta(*index, |f| match f {
                    InFlight::Text { text: t } => {
                        t.push_str(text);
                        Ok(())
                    }
                    _ => Err("text delta for non-text block".into()),
                })?;
            }
            StreamEvent::ThinkingDelta { index, thinking } => {
                self.push_delta(*index, |f| match f {
                    InFlight::Thinking { thinking: t, .. } => {
                        t.push_str(thinking);
                        Ok(())
                    }
                    _ => Err("thinking delta for non-thinking block".into()),
                })?;
            }
            StreamEvent::SignatureDelta { index, signature } => {
                self.push_delta(*index, |f| match f {
                    InFlight::Thinking { signature: s, .. } => {
                        s.push_str(signature);
                        Ok(())
                    }
                    _ => Err("signature delta for non-thinking block".into()),
                })?;
            }
            StreamEvent::InputJsonDelta { index, partial_json } => {
                self.push_delta(*index, |f| match f {
                    InFlight::ToolUse { input, .. } => {
                        input.push_str(partial_json);
                        Ok(())
                    }
                    _ => Err("input_json delta for non-tool_use block".into()),
                })?;
            }
            StreamEvent::BlockStop { index } => {
                let f = self.in_flight.take().ok_or("block stop without start")?;
                let block = match f {
                    InFlight::Text { text } => ContentBlock::Text { text },
                    InFlight::Thinking { thinking, signature } => {
                        ContentBlock::Thinking { thinking, signature }
                    }
                    InFlight::ToolUse { id, name, input } => {
                        let input = serde_json::from_str(&input).unwrap_or(serde_json::Value::Null);
                        ContentBlock::ToolUse { id, name, input }
                    }
                };
                if self.content.len() == *index {
                    self.content.push(block);
                } else {
                    return Err(format!("block index gap at {index}"));
                }
            }
            StreamEvent::StopReason { stop_reason, .. } => {
                self.stop_reason = stop_reason.clone();
            }
            _ => {}
        }
        Ok(())
    }

    fn ensure_slot(&self, index: usize) -> Result<(), String> {
        if index != self.content.len() {
            return Err(format!("block start out of order: {index} != {}", self.content.len()));
        }
        Ok(())
    }

    fn push_delta(
        &mut self,
        _index: usize,
        f: impl FnOnce(&mut InFlight) -> Result<(), String>,
    ) -> Result<(), String> {
        let flight = self.in_flight.as_mut().ok_or("delta without block start")?;
        f(flight)
    }

    pub fn message(&self) -> super::types::Message {
        super::types::Message {
            role: Role::Assistant,
            content: self.content.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::parse_sse_event;
    use crate::api::types::StreamEvent;

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
        let client = Client::new("k".into(), "https://example.com".into());
        let handle = tokio::spawn(async move { client.list_models().await });
        tokio::time::advance(Duration::from_secs(11)).await;
        let res = handle.await.unwrap();
        assert!(matches!(res, Err(ClientError::Timeout)), "读超时应落 TIMEOUT");
    }

    /// AC-13/14 deadline behaviour + tiering not confused: a write must not
    /// fire before 14s and lands on `TIMEOUT` at 15s.
    #[tokio::test(start_paused = true)]
    async fn write_times_out_at_15s_not_before_14s() {
        let _guard = test_hooks::hang_guard(60_000); // hang 60s, > 15s write tier
        let client = Client::new("k".into(), "https://example.com".into());
        let handle = tokio::spawn(async move {
            let req = crate::api::types::Request {
                model: "test".into(),
                max_tokens: 100,
                system: vec![],
                messages: vec![],
                tools: vec![],
                stream: false,
                thinking: None,
                output_config: None,
            };
            client.complete_text(&req).await
        });
        // AC-14: a write does not fire before 14s (the read tier's 10s
        // already passed and must not trip the write tier).
        tokio::time::advance(Duration::from_secs(14)).await;
        assert!(!handle.is_finished(), "写操作 14s 前不应超时");
        // At 16s the write tier fires.
        tokio::time::advance(Duration::from_secs(2)).await;
        let res = handle.await.unwrap();
        assert!(matches!(res, Err(ClientError::Timeout)), "写超时应落 TIMEOUT");
    }

    #[test]
    fn parses_context_limit_error() {
        let body = "400: input length and max_tokens exceed context limit: 12345 + 64000 > 200000";
        assert_eq!(parse_context_limit(body), Some((12345, 200000)));
    }

    #[test]
    fn from_settings_prefers_settings_over_env() {
        let settings = crate::settings::Settings {
            api_key: Some("sk-settings".into()),
            api_base_url: Some("https://settings.example".into()),
            ..Default::default()
        };
        let env = |name: &str| -> Result<String, std::env::VarError> {
            match name {
                "ANTHROPIC_API_KEY" => Ok("sk-env".into()),
                "ANTHROPIC_BASE_URL" => Ok("https://env.example".into()),
                _ => Err(std::env::VarError::NotPresent),
            }
        };
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().0, "sk-settings");
        assert_eq!(client.current_endpoint().1, "https://settings.example");
    }

    #[test]
    fn from_settings_falls_back_to_env() {
        let settings = crate::settings::Settings::default();
        let env = |name: &str| -> Result<String, std::env::VarError> {
            match name {
                "DEEPSEEK_API_KEY" => Ok("sk-deepseek".into()),
                "ANTHROPIC_BASE_URL" => Ok("https://deepseek.example".into()),
                _ => Err(std::env::VarError::NotPresent),
            }
        };
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().0, "sk-deepseek");
        assert_eq!(client.current_endpoint().1, "https://deepseek.example");
    }

    #[test]
    fn from_settings_missing_key_errors() {
        let settings = crate::settings::Settings::default();
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        assert!(matches!(
            Client::from_settings_with(&settings, env),
            Err(ClientError::MissingApiKey)
        ));
    }

    #[test]
    fn from_settings_defaults_base_url() {
        let settings = crate::settings::Settings {
            api_key: Some("sk".into()),
            ..Default::default()
        };
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().1, API_BASE);
    }

    #[test]
    fn provider_switch_changes_endpoint() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-ds".into(),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
            },
        );
        settings.providers.insert(
            "local".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-local".into(),
                api_base_url: "http://127.0.0.1:11434".into(),
                supports_images: None,
            },
        );
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().0, "sk-main");
        assert_eq!(client.provider_names(), vec!["deepseek", "local"]);

        client.set_provider("deepseek").unwrap();
        assert_eq!(client.current_endpoint().0, "sk-ds");
        assert_eq!(client.current_endpoint().1, "https://api.deepseek.com");

        // After the switch, headers use the new key.
        let headers = client.headers().unwrap();
        assert_eq!(
            headers.get("x-api-key").unwrap().to_str().unwrap(),
            "sk-ds"
        );

        assert!(client.set_provider("nope").is_err(), "未知 provider 报错");
        // An unknown provider does not affect the current endpoint.
        assert_eq!(client.current_endpoint().0, "sk-ds");
    }

    /// supports_images: default reads the top-level sendImages; named
    /// providers read their own supportsImages; follows endpoint switches.
    #[test]
    fn supports_images_follows_endpoint_switch() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            send_images: Some(true),
            ..Default::default()
        };
        settings.providers.insert(
            "vision".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-v".into(),
                api_base_url: "https://vision.example".into(),
                supports_images: Some(true),
            },
        );
        settings.providers.insert(
            "text-only".to_string(),
            crate::settings::ProviderConfig {
                api_key: "sk-t".into(),
                api_base_url: "https://text.example".into(),
                supports_images: Some(false),
            },
        );
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert!(client.supports_images(), "default 读顶层 sendImages");

        client.set_provider("text-only").unwrap();
        assert!(!client.supports_images(), "显式 false 覆盖");
        client.set_provider("vision").unwrap();
        assert!(client.supports_images(), "supportsImages=true 生效");
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
        assert!(matches!(&err, ClientError::Stream(m) if m.contains("server stalled")), "{err}");
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

    fn ev(event: &str, data: &str) -> StreamEvent {
        parse_sse_event(event, data).unwrap().unwrap()
    }

    #[test]
    fn accumulates_text_and_thinking() {
        let mut acc = AssistantAccumulator::new();
        acc.push(&ev("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}"#)).unwrap();
        acc.push(&ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"plan"}}"#)).unwrap();
        acc.push(&ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig123"}}"#)).unwrap();
        acc.push(&ev("content_block_stop", r#"{"type":"content_block_stop","index":0}"#)).unwrap();
        acc.push(&ev("content_block_start", r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#)).unwrap();
        acc.push(&ev("content_block_delta", r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"hi"}}"#)).unwrap();
        acc.push(&ev("content_block_stop", r#"{"type":"content_block_stop","index":1}"#)).unwrap();
        assert_eq!(
            acc.content,
            vec![
                ContentBlock::Thinking { thinking: "plan".into(), signature: "sig123".into() },
                ContentBlock::Text { text: "hi".into() },
            ]
        );
    }

    #[test]
    fn accumulates_tool_use_input() {
        let mut acc = AssistantAccumulator::new();
        acc.push(&ev("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_9","name":"Bash","input":{}}}"#)).unwrap();
        acc.push(&ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"command\":"}}"#)).unwrap();
        acc.push(&ev("content_block_delta", r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"\"ls\"}"}}"#)).unwrap();
        acc.push(&ev("content_block_stop", r#"{"type":"content_block_stop","index":0}"#)).unwrap();
        assert_eq!(
            acc.content,
            vec![ContentBlock::ToolUse {
                id: "tu_9".into(),
                name: "Bash".into(),
                input: serde_json::json!({"command": "ls"}),
            }]
        );
    }

    #[test]
    fn tool_use_input_falls_back_to_null() {
        let mut acc = AssistantAccumulator::new();
        acc.push(&ev("content_block_start", r#"{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tu_1","name":"Bash","input":{}}}"#)).unwrap();
        acc.push(&ev("content_block_stop", r#"{"type":"content_block_stop","index":0}"#)).unwrap();
        assert!(matches!(&acc.content[0], ContentBlock::ToolUse { input, .. } if input.is_null()));
    }
}

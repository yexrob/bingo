//! OpenAI Responses protocol adapter (D33). Mapping contract (api::contract)
//! → Responses API wire format per §4.2 of notes/design/provider-oauth.md,
//! verified against the official API reference. Default base
//! `https://api.openai.com`, auth `Authorization: Bearer <api_key>`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Deserialize;

use super::{AuthSource, backoff, retryable};
use crate::api::contract::{
    AuthStatus, BoxStream, Capabilities, ClientError, NeutralRequest, ProviderClient,
    StreamApiErrorKind, StreamEvent, SystemBlock, ThinkingLevel,
};
use crate::api::sse::SseParser;
use crate::api::types::{ContentBlock, Message, Role};

pub const API_BASE: &str = "https://api.openai.com";

/// Short-sync **read** operation feedback-layer timeout (AC-12/14,
/// same tier as the anthropic adapter).
const SHORT_READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Short-sync **write** operation feedback-layer timeout (AC-13/14).
const SHORT_WRITE_TIMEOUT: Duration = Duration::from_secs(15);

/// Overall request timeout (connection + first byte) for agent long turns.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Streaming-body idle timeout (server stalls after connecting).
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

pub const MAX_RETRIES: u32 = 5;

/// thinking level → Responses `reasoning.effort`; GPT-5.6 adds xhigh/max.
fn effort_for(model: &str, level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High => "high",
        ThinkingLevel::Xhigh if model.starts_with("gpt-5.6") => "xhigh",
        ThinkingLevel::Max if model.starts_with("gpt-5.6") => "max",
        ThinkingLevel::Xhigh | ThinkingLevel::Max => "high",
    }
}

/// The endpoint (one per provider instance; mirrors the anthropic adapter).
/// Endpoint flavor: the public Responses API (default) or the ChatGPT
/// subscription endpoint (codex variant, D33 §6.1b / Path 2): same wire
/// format, different path + ChatGPT-Account-Id header + its own model-list
/// route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiVariant {
    Default,
    Codex,
}

#[derive(Debug, Clone)]
struct Endpoint {
    base_url: String,
    supports_images: bool,
    variant: OpenAiVariant,
}

#[derive(Debug, Clone)]
pub struct OpenAIProvider {
    http: reqwest::Client,
    endpoint: Arc<std::sync::RwLock<Endpoint>>,
    auth: AuthSource,
}

impl OpenAIProvider {
    pub fn new(
        http: reqwest::Client,
        auth: AuthSource,
        base_url: String,
        supports_images: bool,
        variant: OpenAiVariant,
    ) -> Self {
        Self {
            http,
            endpoint: Arc::new(std::sync::RwLock::new(Endpoint {
                base_url,
                supports_images,
                variant,
            })),
            auth,
        }
    }

    fn variant(&self) -> OpenAiVariant {
        self.endpoint
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .variant
    }

    /// Codex model snapshot (fallback when the dynamic model list fails):
    /// all nine models main live-tested as 200-usable on the subscription
    /// endpoint. The dynamic list is authoritative; this is the safety net.
    pub const CODEX_MODELS: [&'static str; 9] = [
        "gpt-5.6-sol",
        "gpt-5.6-sol-wm",
        "gpt-5.6-terra",
        "gpt-5.6-luna",
        "gpt-5.5",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.3-codex-spark",
        "codex-auto-review",
    ];

    /// The chat endpoint path for the variant.
    fn api_path(&self) -> &'static str {
        match self.variant() {
            OpenAiVariant::Default => "/v1/responses",
            OpenAiVariant::Codex => "/codex/responses",
        }
    }

    /// The model-list path (the codex variant takes its own route before any
    /// request reaches here).
    fn models_path(&self) -> &'static str {
        "/v1/models"
    }

    async fn headers(&self) -> Result<HeaderMap, ClientError> {
        let bearer = match &self.auth {
            AuthSource::ApiKey(key) => format!("Bearer {key}"),
            // Fail fast with the login command instead of sending an empty
            // bearer and bouncing off a remote 401.
            AuthSource::StoredKey(stored) => match stored.key() {
                Some(key) => format!("Bearer {key}"),
                None => {
                    return Err(ClientError::Auth(format!(
                        "provider \"{}\" has no API key configured: /provider login {} --manual <key>",
                        stored.provider(),
                        stored.provider()
                    )));
                }
            },
            AuthSource::OAuth(provider) => {
                format!("Bearer {}", provider.access_token().await?)
            }
        };
        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&bearer)
                .map_err(|e| ClientError::InvalidApiKey(e.to_string()))?,
        );
        // Codex subscription routing: ChatGPT-Account-Id from JWT claims +
        // originator (opencode codex.ts chat.headers); only on the codex
        // variant — no cross-talk.
        if self.variant() == OpenAiVariant::Codex {
            headers.insert("originator", HeaderValue::from_static("bingo"));
            if let Some(account) = self.oauth_account()
                && let Ok(value) = HeaderValue::from_str(&account)
            {
                headers.insert("ChatGPT-Account-Id", value);
            }
        }
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }

    /// OAuth account for /provider listing (None = not logged in).
    fn oauth_account(&self) -> Option<String> {
        match &self.auth {
            AuthSource::OAuth(provider) => provider.account_sync(),
            AuthSource::ApiKey(_) | AuthSource::StoredKey(_) => None,
        }
    }

    fn base_url(&self) -> String {
        self.endpoint
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .base_url
            .clone()
    }

    /// Codex dynamic model list: `/codex/models?client_version=<semver>`
    /// (the endpoint rejects other version formats, main-tested). Any failure
    /// (network/status/parse) falls back to the static snapshot — the /model
    /// menu must never hard-fail because of the model list.
    async fn list_codex_models(&self) -> Result<Vec<String>, ClientError> {
        let base_url = self.base_url();
        // Any failure — timeout, transport, HTTP status or parse — falls
        // back to the static snapshot (the /model menu must not hard-fail).
        let response = match tokio::time::timeout(
            SHORT_READ_TIMEOUT,
            self.http
                .get(format!("{base_url}/codex/models"))
                .query(&[("client_version", "0.146.0")])
                .headers(self.headers().await?)
                .send(),
        )
        .await
        {
            Ok(Ok(r)) => r,
            _ => {
                return Ok(Self::CODEX_MODELS.iter().map(|m| m.to_string()).collect());
            }
        };
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or_default();
        if status.is_success()
            && let Some(models) = parse_models_list(&body)
            && !models.is_empty()
        {
            return Ok(models);
        }
        Ok(Self::CODEX_MODELS.iter().map(|m| m.to_string()).collect())
    }
}

/// NeutralRequest → Responses request body (variant-isolated params).
fn build_body(request: &NeutralRequest, variant: OpenAiVariant) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": request.model,
        "stream": request.stream,
        "input": build_input(&request.messages),
    });
    if variant == OpenAiVariant::Codex {
        // Codex endpoint contract (opencode codex.ts chat.params): the
        // subscription endpoint rejects max_output_tokens (400 Unsupported
        // parameter) and the default store:true — both are omitted/overridden.
        body["store"] = serde_json::json!(false);
    } else {
        body["max_output_tokens"] = serde_json::json!(request.max_tokens);
    }
    if !request.system.is_empty() {
        // The system prompt is a single `instructions` string; segments are
        // joined (caching breakpoints are a per-protocol concern — anthropic
        // maps `cache` flags, Responses prompt caching is not wired in v1).
        body["instructions"] = serde_json::Value::String(
            request
                .system
                .iter()
                .map(|b| b.text.clone())
                .collect::<Vec<_>>()
                .join("\n\n"),
        );
    }
    if !request.tools.is_empty() {
        body["tools"] = serde_json::Value::Array(
            request
                .tools
                .iter()
                .map(|schema| {
                    serde_json::json!({
                        "type": "function",
                        "name": schema.get("name").cloned().unwrap_or(serde_json::Value::Null),
                        "description": schema.get("description").cloned().unwrap_or(serde_json::Value::Null),
                        "parameters": schema.get("input_schema").cloned().unwrap_or(serde_json::Value::Null),
                        "strict": false,
                    })
                })
                .collect(),
        );
    }
    if let Some(level) = request.thinking {
        body["reasoning"] = serde_json::json!({ "effort": effort_for(&request.model, level) });
        // Reasoning include per endpoint: the public API takes the summary
        // (thinking UI affordance); the codex endpoint accepts only
        // reasoning.encrypted_content (main-tested 200; summary_text → 400).
        body["include"] = match variant {
            OpenAiVariant::Codex => serde_json::json!(["reasoning.encrypted_content"]),
            OpenAiVariant::Default => serde_json::json!(["reasoning.summary_text"]),
        };
    }
    body
}

/// Messages → `input` item list.
fn build_input(messages: &[Message]) -> Vec<serde_json::Value> {
    let mut items = Vec::new();
    for message in messages {
        match message.role {
            Role::User => {
                let mut content = Vec::new();
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => content.push(serde_json::json!({
                            "type": "input_text",
                            "text": text,
                        })),
                        ContentBlock::Image { source } => content.push(serde_json::json!({
                            "type": "input_image",
                            "image_url": format!(
                                "data:{};base64,{}",
                                source.media_type, source.data
                            ),
                        })),
                        ContentBlock::ToolResult {
                            tool_use_id,
                            content: output,
                            is_error,
                        } => {
                            items.push(serde_json::json!({
                                "type": "function_call_output",
                                "call_id": tool_use_id,
                                "output": tool_output_wire(output, *is_error),
                            }));
                        }
                        _ => {}
                    }
                }
                if !content.is_empty() {
                    items.push(serde_json::json!({
                        "type": "message",
                        "role": "user",
                        "content": content,
                    }));
                }
            }
            Role::Assistant => {
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => items.push(serde_json::json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text}],
                        })),
                        ContentBlock::ToolUse { id, name, input } => {
                            items.push(serde_json::json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": input.to_string(),
                            }));
                        }
                        // Thinking is not replayable on Responses (encrypted);
                        // v1 drops it on replay (D33 §10).
                        ContentBlock::Thinking { .. } => {}
                        _ => {}
                    }
                }
            }
        }
    }
    items
}

/// Tool result → `function_call_output.output` string. Anthropic carries
/// `is_error` as a separate flag; Responses carries a plain string — encode
/// the flag so the model still sees the error semantics (§4.2).
fn tool_output_wire(content: &serde_json::Value, is_error: bool) -> String {
    if is_error {
        serde_json::json!({ "is_error": true, "content": content }).to_string()
    } else {
        crate::api::types::tool_result_text(content)
    }
}

/// Parse a model list response: `data[].id` (OpenAI style), a plain string
/// array, or `models[]` — tolerant of the codex endpoint's exact shape.
fn parse_models_list(body: &serde_json::Value) -> Option<Vec<String>> {
    let collect = |arr: &Vec<serde_json::Value>| {
        arr.iter()
            .filter_map(|m| {
                m.get("id")
                    .and_then(|v| v.as_str())
                    .or_else(|| m.as_str())
                    .map(str::to_string)
            })
            .collect::<Vec<String>>()
    };
    body.get("data")
        .and_then(|d| d.as_array())
        .map(collect)
        .or_else(|| body.as_array().map(collect))
        .or_else(|| body.get("models").and_then(|m| m.as_array()).map(collect))
}

/// Non-streaming completion reply: join `output[].content[].text`.
fn parse_completion_text(body: &serde_json::Value) -> String {
    let mut text = String::new();
    if let Some(items) = body.get("output").and_then(|o| o.as_array()) {
        for item in items {
            if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                for part in parts {
                    if part.get("type").and_then(|t| t.as_str()) == Some("output_text")
                        && let Some(t) = part.get("text").and_then(|t| t.as_str())
                    {
                        text.push_str(t);
                    }
                }
            }
        }
    }
    text
}

#[derive(Debug, Deserialize)]
struct ErrorPayload {
    #[serde(default)]
    message: String,
    #[serde(default)]
    code: Option<String>,
}

/// `2`, `"1.5"`, `"3s"`, `"250ms"` → duration; `assume_ms` sets the unit for bare numbers.
fn retry_after_value(value: &serde_json::Value, assume_ms: bool) -> Option<Duration> {
    if let Some(number) = value.as_f64() {
        return retry_after_number(number, assume_ms);
    }
    let text = value.as_str()?.trim();
    if let Some(milliseconds) = text.strip_suffix("ms") {
        return retry_after_number(milliseconds.trim().parse().ok()?, true);
    }
    if let Some(seconds) = text.strip_suffix('s') {
        return retry_after_number(seconds.trim().parse().ok()?, false);
    }
    retry_after_number(text.parse().ok()?, assume_ms)
}

fn retry_after_number(value: f64, milliseconds: bool) -> Option<Duration> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(if milliseconds {
        value / 1_000.0
    } else {
        value
    }))
}

fn retry_after_from_value(value: &serde_json::Value) -> Option<Duration> {
    let error = value.get("error");
    let field = |key: &str| {
        value
            .get(key)
            .or_else(|| error.and_then(|error| error.get(key)))
    };
    ["retry_after_ms", "retry-after-ms"]
        .iter()
        .find_map(|key| field(key).and_then(|value| retry_after_value(value, true)))
        .or_else(|| {
            ["retry_after", "retry-after", "retry_delay"]
                .iter()
                .find_map(|key| field(key).and_then(|value| retry_after_value(value, false)))
        })
}

fn stream_api_error_kind(code: Option<&str>, message: &str) -> StreamApiErrorKind {
    match code {
        Some("server_is_overloaded" | "server_error" | "overloaded_error") => {
            StreamApiErrorKind::Retryable
        }
        Some(
            "insufficient_quota"
            | "usage_not_included"
            | "invalid_prompt"
            | "context_length_exceeded",
        ) => StreamApiErrorKind::NonRetryable,
        _ => StreamApiErrorKind::from_message(message),
    }
}

/// Incremental Responses SSE mapper: flattens the two-layer index
/// (output_item + content part) into the single block index the accumulator
/// expects, and defends empty-argument function calls with the authoritative
/// `arguments` on `output_item.done` (§4.2).
struct ResponsesSseMapper {
    /// output_index → block index (only for item types we emit).
    block_of: HashMap<usize, usize>,
    /// output_index → accumulated arguments text.
    args_buf: HashMap<usize, String>,
    next_block: usize,
}

impl ResponsesSseMapper {
    fn new() -> Self {
        Self {
            block_of: HashMap::new(),
            args_buf: HashMap::new(),
            next_block: 0,
        }
    }

    /// Parse one SSE event/data pair into `StreamEvent`s (empty when the
    /// event is noise for the accumulator; the empty-argument backfill may
    /// yield a delta + its BlockStop together).
    fn feed(&mut self, event: &str, data: &str) -> Result<Vec<StreamEvent>, String> {
        let value: serde_json::Value =
            serde_json::from_str(data).map_err(|e| format!("bad {event} payload: {e}"))?;
        match event {
            "response.created" => {
                let response = value
                    .get("response")
                    .ok_or("response.created without response")?;
                Ok(vec![StreamEvent::MessageStart {
                    id: response
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .into(),
                    model: response
                        .get("model")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .into(),
                }])
            }
            "response.output_item.added" => {
                let index = value
                    .get("output_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let item = value.get("item").ok_or("output_item.added without item")?;
                let kind = item
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let mut ev = match kind {
                    "message" => StreamEvent::TextStart { index: 0 },
                    "reasoning" => StreamEvent::ThinkingStart { index: 0 },
                    "function_call" => StreamEvent::ToolUseStart {
                        index: 0,
                        id: item
                            .get("call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .into(),
                        name: item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .into(),
                    },
                    _other => return Ok(Vec::new()),
                };
                // Flatten the two-layer index: only item types we emit occupy
                // a block slot, keeping the accumulator's index sequence dense.
                let block = self.next_block;
                self.next_block += 1;
                self.block_of.insert(index, block);
                match &mut ev {
                    StreamEvent::TextStart { index }
                    | StreamEvent::ThinkingStart { index }
                    | StreamEvent::ToolUseStart { index, .. } => *index = block,
                    _ => {}
                }
                Ok(vec![ev])
            }
            "response.output_text.delta" => {
                let index = output_index(&value)?;
                let block = self.block(&index)?;
                let text = value
                    .get("delta")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                Ok(vec![StreamEvent::TextDelta {
                    index: block,
                    text: text.into(),
                }])
            }
            // Two event names, one surface. The API streams a *summary* of reasoning for
            // models that keep theirs hidden, and the reasoning text itself for models
            // that do not — same `output_index`, same string `delta`. Reading only the
            // summary name dropped every token of the second kind, under a thinking block
            // the `reasoning` item had already opened: the affordance was there, always
            // empty (D125; observed on a proxy fronting DeepSeek, whose reasoning arrives
            // as `response.reasoning_text.delta` and is what the anthropic adapter gets
            // for free as `thinking_delta`).
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let index = output_index(&value)?;
                let block = self.block(&index)?;
                let thinking = value
                    .get("delta")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                Ok(vec![StreamEvent::ThinkingDelta {
                    index: block,
                    thinking: thinking.into(),
                }])
            }
            "response.function_call_arguments.delta" => {
                let index = output_index(&value)?;
                let block = self.block(&index)?;
                let partial = value
                    .get("delta")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                self.args_buf.entry(index).or_default().push_str(partial);
                Ok(vec![StreamEvent::InputJsonDelta {
                    index: block,
                    partial_json: partial.into(),
                }])
            }
            "response.output_item.done" => {
                let index = value
                    .get("output_index")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let Some(block) = self.block_of.remove(&index) else {
                    return Ok(Vec::new()); // an item type we never started
                };
                let item = value.get("item").ok_or("output_item.done without item")?;
                // Empty-argument defensive: some implementations skip delta
                // events for `{}`; the authoritative arguments arrive here —
                // emit them as the final delta, then close the block.
                if item.get("type").and_then(|v| v.as_str()) == Some("function_call")
                    && let Some(args) = item.get("arguments").and_then(|v| v.as_str())
                    && self.args_buf.remove(&index).unwrap_or_default() != args
                {
                    return Ok(vec![
                        StreamEvent::InputJsonDelta {
                            index: block,
                            partial_json: args.into(),
                        },
                        StreamEvent::BlockStop { index: block },
                    ]);
                }
                self.args_buf.remove(&index);
                Ok(vec![StreamEvent::BlockStop { index: block }])
            }
            "response.completed" | "response.incomplete" => {
                let response = value.get("response").ok_or("missing response")?;
                let usage = response.get("usage").cloned().unwrap_or_default();
                let output_tokens = usage
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .or_else(|| usage.get("total_tokens").and_then(|v| v.as_u64()));
                let stop_reason = if event == "response.completed" {
                    Some("end_turn".to_string())
                } else {
                    let reason = response
                        .get("incomplete_details")
                        .and_then(|d| d.get("reason"))
                        .and_then(|v| v.as_str());
                    Some(match reason {
                        // The query loop's max_tokens continuation keys on
                        // this exact string (same as anthropic).
                        Some("max_output_tokens") => "max_tokens".to_string(),
                        other => other.unwrap_or("incomplete").to_string(),
                    })
                };
                Ok(vec![StreamEvent::StopReason {
                    stop_reason,
                    output_tokens,
                }])
            }
            "response.failed" => {
                let response = value.get("response").unwrap_or(&value);
                let error = response.get("error");
                let message = error
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("response failed");
                let code = error.and_then(|e| e.get("code")).and_then(|v| v.as_str());
                Ok(vec![StreamEvent::ApiError {
                    message: match code {
                        Some(code) => format!("{code}: {message}"),
                        None => message.to_string(),
                    },
                    kind: stream_api_error_kind(code, message),
                    retry_after: retry_after_from_value(response),
                }])
            }
            "error" => {
                let retry_after = retry_after_from_value(&value);
                let payload: ErrorPayload =
                    serde_json::from_value(value).map_err(|e| format!("bad error payload: {e}"))?;
                let kind = stream_api_error_kind(payload.code.as_deref(), &payload.message);
                Ok(vec![StreamEvent::ApiError {
                    message: if let Some(code) = payload.code {
                        format!("{code}: {}", payload.message)
                    } else {
                        payload.message
                    },
                    kind,
                    retry_after,
                }])
            }
            _other => Ok(Vec::new()), // response.in_progress, ping, .done noise, ...
        }
    }

    fn block(&self, output_index: &usize) -> Result<usize, String> {
        self.block_of
            .get(output_index)
            .copied()
            .ok_or_else(|| format!("delta for unknown output item {output_index}"))
    }
}

fn output_index(value: &serde_json::Value) -> Result<usize, String> {
    value
        .get("output_index")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .ok_or_else(|| "delta without output_index".to_string())
}

#[async_trait]
impl ProviderClient for OpenAIProvider {
    fn capabilities(&self) -> Capabilities {
        let supports_images = self
            .endpoint
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .supports_images;
        Capabilities {
            supports_images,
            supports_count_tokens: false,
            supports_prompt_caching: false,
            ..Default::default()
        }
    }

    fn auth_status(&self) -> AuthStatus {
        match &self.auth {
            AuthSource::ApiKey(_) => AuthStatus::ApiKey,
            // Live read: a login in this session flips configured without a
            // restart (the /provider listing reads this).
            AuthSource::StoredKey(stored) => AuthStatus::StoredKey {
                configured: stored.key().is_some(),
            },
            AuthSource::OAuth(_) => AuthStatus::OAuth {
                account: self.oauth_account(),
            },
        }
    }

    async fn stream(&self, request: &NeutralRequest) -> Result<BoxStream, ClientError> {
        let body = build_body(request, self.variant());
        let mut attempt = 0;
        let mut auth_refreshed = false;
        let base_url = self.base_url();
        loop {
            let builder = self
                .http
                .post(format!("{base_url}{}", self.api_path()))
                .headers(self.headers().await?)
                .json(&body);
            match tokio::time::timeout(REQUEST_TIMEOUT, builder.send()).await {
                Ok(Ok(response)) if response.status().is_success() => {
                    return Ok(Box::pin(stream_body(response)));
                }
                // 401 with OAuth auth: refresh once (single-flight) and retry
                // with the new token (D33 §6.3).
                Ok(Ok(response))
                    if response.status() == reqwest::StatusCode::UNAUTHORIZED
                        && matches!(&self.auth, AuthSource::OAuth(_))
                        && !auth_refreshed =>
                {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    if let AuthSource::OAuth(provider) = &self.auth {
                        provider.force_refresh().await?;
                    }
                    auth_refreshed = true;
                    attempt += 1;
                    let _ = (status, body);
                    continue;
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
                        return Err(ClientError::from_response(status.as_u16(), body));
                    }
                    tokio::time::sleep(retry_after.unwrap_or_else(|| backoff(attempt))).await;
                }
                Ok(Ok(response)) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    return Err(ClientError::from_response(status.as_u16(), body));
                }
                Ok(Err(_transport)) if attempt < MAX_RETRIES => {
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Ok(Err(transport)) => return Err(ClientError::Transport(transport)),
                Err(_) => return Err(ClientError::Timeout),
            }
            attempt += 1;
        }
    }

    async fn complete_text(&self, request: &NeutralRequest) -> Result<String, ClientError> {
        // Codex endpoint is stream-only (400 on stream:false): stream
        // internally and aggregate the output text — the neutral interface
        // is unchanged, callers (compact/memory) are unaffected. The whole
        // aggregation stays under the short-write budget (AC-13/14) — the
        // codex path must not bypass the 15s feedback-layer deadline.
        if self.variant() == OpenAiVariant::Codex {
            let mut req = request.clone();
            req.stream = true;
            let result = tokio::time::timeout(SHORT_WRITE_TIMEOUT, async {
                let mut stream = self.stream(&req).await?;
                let mut text = String::new();
                while let Some(event) = stream.next().await {
                    match event? {
                        StreamEvent::TextDelta { text: t, .. } => text.push_str(&t),
                        StreamEvent::ApiError { message, .. } => {
                            return Err(ClientError::Stream(message));
                        }
                        _ => {}
                    }
                }
                Ok::<String, ClientError>(text)
            })
            .await
            .map_err(|_| ClientError::Timeout)??;
            return Ok(result);
        }
        let mut body = build_body(request, self.variant());
        body["stream"] = serde_json::json!(false);
        let base_url = self.base_url();
        // OAuth 401 recovery: refresh once and retry (short-sync write; the
        // retry stays inside the 15s feedback-layer deadline).
        let mut auth_refreshed = false;
        let response = loop {
            let response = tokio::time::timeout(
                SHORT_WRITE_TIMEOUT,
                self.http
                    .post(format!("{base_url}{}", self.api_path()))
                    .headers(self.headers().await?)
                    .json(&body)
                    .send(),
            )
            .await
            .map_err(|_| ClientError::Timeout)??;
            if response.status() == reqwest::StatusCode::UNAUTHORIZED
                && matches!(&self.auth, AuthSource::OAuth(_))
                && !auth_refreshed
            {
                if let AuthSource::OAuth(provider) = &self.auth {
                    provider.force_refresh().await?;
                }
                auth_refreshed = true;
                continue;
            }
            break response;
        };
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ClientError::from_response(status.as_u16(), body));
        }
        let body: serde_json::Value = response.json().await?;
        Ok(parse_completion_text(&body))
    }

    async fn list_models(&self) -> Result<Vec<String>, ClientError> {
        // Codex: the subscription's model list is dynamic
        // (`GET {base}/codex/models?client_version=0.146.0`, main live-tested
        // — 9 models; a failed request falls back to the static snapshot).
        if self.variant() == OpenAiVariant::Codex {
            return self.list_codex_models().await;
        }
        let base_url = self.base_url();
        let response = tokio::time::timeout(
            SHORT_READ_TIMEOUT,
            self.http
                .get(format!("{base_url}{}", self.models_path()))
                .headers(self.headers().await?)
                .send(),
        )
        .await
        .map_err(|_| ClientError::Timeout)??;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ClientError::from_response(status.as_u16(), body));
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

    /// No public count_tokens endpoint on the Responses API (D33 §4.2): the
    /// token gate falls back to local estimation.
    async fn count_tokens(
        &self,
        _model: &str,
        _system: &[SystemBlock],
        _messages: &[Message],
        _tools: &[serde_json::Value],
    ) -> Result<u64, ClientError> {
        Err(ClientError::Unsupported(
            "count_tokens is not available for the openai protocol (local estimation used)".into(),
        ))
    }
}

fn stream_body(
    response: reqwest::Response,
) -> impl futures_util::Stream<Item = Result<StreamEvent, ClientError>> {
    let mut parser = SseParser::new();
    let mut mapper = ResponsesSseMapper::new();
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
                match mapper.feed(&frame.event, &frame.data) {
                    Ok(events) => {
                        for event in events {
                            yield Ok(event);
                        }
                    }
                    Err(message) => {
                        yield Err(ClientError::Stream(message));
                        return;
                    }
                }
            }
        }
    }
}

/// Idle-timeout wrapper for `stream.next()` (same semantics as the anthropic
/// adapter: a silent stream is judged dead).
async fn next_with_idle<S, T>(body: &mut S, idle: Duration) -> Result<Option<T>, ClientError>
where
    S: futures_util::Stream<Item = T> + Unpin,
{
    tokio::time::timeout(idle, body.next())
        .await
        .map_err(|_| ClientError::Stream(format!("no stream data for {idle:?}: server stalled")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::contract::ThinkingLevel;

    fn req() -> NeutralRequest {
        NeutralRequest {
            model: "gpt-5".into(),
            max_tokens: 1024,
            system: vec![],
            messages: vec![],
            tools: vec![],
            stream: true,
            thinking: None,
        }
    }

    /// System → instructions; thinking → reasoning.effort + summary include.
    #[test]
    fn body_maps_system_and_thinking() {
        let mut r = req();
        r.system = vec![
            SystemBlock {
                text: "role".into(),
                cache: false,
            },
            SystemBlock {
                text: "tools".into(),
                cache: true,
            },
        ];
        r.thinking = Some(ThinkingLevel::High);
        let body = build_body(&r, OpenAiVariant::Default);
        assert_eq!(body["instructions"], "role\n\ntools");
        assert_eq!(body["reasoning"], serde_json::json!({"effort": "high"}));
        assert_eq!(
            body["include"],
            serde_json::json!(["reasoning.summary_text"])
        );
        assert_eq!(body["max_output_tokens"], 1024);
    }

    #[test]
    fn body_scopes_extended_thinking_effort_to_gpt_5_6() {
        let mut r = req();
        r.model = "gpt-5.6-sol".into();
        for (level, effort) in [(ThinkingLevel::Xhigh, "xhigh"), (ThinkingLevel::Max, "max")] {
            r.thinking = Some(level);
            let body = build_body(&r, OpenAiVariant::Default);
            assert_eq!(body["reasoning"]["effort"], effort);
        }

        r.model = "gpt-5".into();
        for level in [ThinkingLevel::Xhigh, ThinkingLevel::Max] {
            r.thinking = Some(level);
            let body = build_body(&r, OpenAiVariant::Default);
            assert_eq!(body["reasoning"]["effort"], "high");
        }
    }

    /// Variant-isolated request params (main-reported live matrix): the codex
    /// endpoint rejects max_output_tokens, include (reasoning) and stream:false
    /// (stream-only) and requires store:false; the default variant keeps
    /// max_output_tokens + reasoning include + no store. Guarded vs regressions.
    #[test]
    fn codex_request_params_isolation() {
        let mut r = req();
        r.thinking = Some(ThinkingLevel::High);
        let codex = build_body(&r, OpenAiVariant::Codex);
        assert!(
            codex.get("max_output_tokens").is_none(),
            "codex must not send max_output_tokens"
        );
        assert_eq!(
            codex["include"],
            serde_json::json!(["reasoning.encrypted_content"]),
            "codex uses the encrypted_content include (verified 200 on main)"
        );
        assert_eq!(
            codex["store"],
            serde_json::json!(false),
            "codex explicitly sends store:false"
        );
        assert_eq!(codex["model"], "gpt-5", "other fields are preserved");
        assert_eq!(codex["stream"], true, "codex forces streaming");
        assert_eq!(
            codex["reasoning"],
            serde_json::json!({"effort": "high"}),
            "reasoning is preserved"
        );

        let default = build_body(&r, OpenAiVariant::Default);
        assert_eq!(
            default["max_output_tokens"], 1024,
            "default keeps max_output_tokens"
        );
        assert_eq!(
            default["include"],
            serde_json::json!(["reasoning.summary_text"]),
            "default keeps the reasoning include"
        );
        assert!(
            default.get("store").is_none(),
            "default sends no store (zero behavior change)"
        );
    }

    /// Tools map input_schema → parameters with the function envelope.
    #[test]
    fn body_maps_tools() {
        let mut r = req();
        r.tools = vec![serde_json::json!({
            "name": "Bash",
            "description": "run a command",
            "input_schema": {"type": "object", "properties": {"command": {"type": "string"}}},
        })];
        let body = build_body(&r, OpenAiVariant::Default);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "Bash");
        assert_eq!(
            body["tools"][0]["parameters"]["properties"]["command"]["type"],
            "string"
        );
        assert_eq!(body["tools"][0]["strict"], false);
    }

    /// Messages → input items: text/images/function_call/function_call_output;
    /// thinking blocks are dropped on replay; error results keep the flag.
    #[test]
    fn input_maps_messages() {
        let messages = vec![
            Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text { text: "hi".into() },
                    ContentBlock::Image {
                        source: crate::api::types::ImageSource::base64("image/png", "aGk="),
                    },
                ],
            },
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "plan".into(),
                        signature: "s".into(),
                    },
                    ContentBlock::Text { text: "ok".into() },
                    ContentBlock::ToolUse {
                        id: "call_1".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({"command": "ls"}),
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "call_1".into(),
                    content: serde_json::Value::String("boom".into()),
                    is_error: true,
                }],
            },
        ];
        let items = build_input(&messages);
        assert_eq!(items.len(), 4);
        // user message with text+image content.
        assert_eq!(items[0]["type"], "message");
        assert_eq!(items[0]["content"][0]["type"], "input_text");
        assert_eq!(items[0]["content"][1]["type"], "input_image");
        assert_eq!(
            items[0]["content"][1]["image_url"],
            "data:image/png;base64,aGk="
        );
        // assistant: thinking dropped, text + function_call present.
        assert_eq!(items[1]["type"], "message");
        assert_eq!(items[1]["content"][0]["text"], "ok");
        assert_eq!(items[2]["type"], "function_call");
        assert_eq!(items[2]["call_id"], "call_1");
        assert_eq!(items[2]["arguments"], r#"{"command":"ls"}"#);
        // error tool result keeps the is_error flag in the output string.
        assert_eq!(items[3]["type"], "function_call_output");
        let output: serde_json::Value =
            serde_json::from_str(items[3]["output"].as_str().unwrap()).unwrap();
        assert_eq!(output["is_error"], true);
        assert_eq!(output["content"], "boom");
    }

    /// SSE: full text turn sequence produces the same normalized events as
    /// the anthropic adapter's text turn.
    #[test]
    fn sse_maps_text_turn() {
        let mut mapper = ResponsesSseMapper::new();
        let mut out = Vec::new();
        let push = |mapper: &mut ResponsesSseMapper,
                    out: &mut Vec<StreamEvent>,
                    event: &str,
                    data: &str| {
            for ev in mapper.feed(event, data).unwrap() {
                out.push(ev);
            }
        };
        push(
            &mut mapper,
            &mut out,
            "response.created",
            r#"{"type":"response.created","response":{"id":"resp_1","model":"gpt-5","status":"in_progress"}}"#,
        );
        push(
            &mut mapper,
            &mut out,
            "response.output_item.added",
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"msg_1","role":"assistant","status":"in_progress","content":[{"type":"output_text","text":"","annotations":[]}]}}"#,
        );
        push(
            &mut mapper,
            &mut out,
            "response.output_text.delta",
            r#"{"type":"response.output_text.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"Hel"}"#,
        );
        push(
            &mut mapper,
            &mut out,
            "response.output_text.delta",
            r#"{"type":"response.output_text.delta","item_id":"msg_1","output_index":0,"content_index":0,"delta":"lo"}"#,
        );
        push(
            &mut mapper,
            &mut out,
            "response.output_item.done",
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"message","id":"msg_1","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Hello","annotations":[]}]}}"#,
        );
        push(
            &mut mapper,
            &mut out,
            "response.completed",
            r#"{"type":"response.completed","response":{"id":"resp_1","status":"completed","usage":{"input_tokens":10,"output_tokens":5,"total_tokens":15}}}"#,
        );
        assert_eq!(
            out,
            vec![
                StreamEvent::MessageStart {
                    id: "resp_1".into(),
                    model: "gpt-5".into()
                },
                StreamEvent::TextStart { index: 0 },
                StreamEvent::TextDelta {
                    index: 0,
                    text: "Hel".into()
                },
                StreamEvent::TextDelta {
                    index: 0,
                    text: "lo".into()
                },
                StreamEvent::BlockStop { index: 0 },
                StreamEvent::StopReason {
                    stop_reason: Some("end_turn".into()),
                    output_tokens: Some(5)
                },
            ]
        );
    }

    /// SSE: tool call with deltas; the accumulator contract must see the same
    /// sequence as an anthropic tool_use turn.
    #[test]
    fn sse_maps_tool_turn() {
        let mut mapper = ResponsesSseMapper::new();
        let mut out = Vec::new();
        let mut push = |event: &str, data: &str| {
            for ev in mapper.feed(event, data).unwrap() {
                out.push(ev);
            }
        };
        push(
            "response.created",
            r#"{"type":"response.created","response":{"id":"resp_2","model":"gpt-5"}}"#,
        );
        push(
            "response.output_item.added",
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_9","name":"Bash","status":"in_progress","arguments":""}}"#,
        );
        push(
            "response.function_call_arguments.delta",
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":0,"delta":"{\"command\":"}"#,
        );
        push(
            "response.function_call_arguments.delta",
            r#"{"type":"response.function_call_arguments.delta","item_id":"fc_1","output_index":0,"delta":"\"ls\"}"}"#,
        );
        push(
            "response.output_item.done",
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_9","name":"Bash","status":"completed","arguments":"{\"command\":\"ls\"}"}}"#,
        );
        push(
            "response.completed",
            r#"{"type":"response.completed","response":{"id":"resp_2","status":"completed","usage":{"output_tokens":5}}}"#,
        );
        assert_eq!(
            out,
            vec![
                StreamEvent::MessageStart {
                    id: "resp_2".into(),
                    model: "gpt-5".into()
                },
                StreamEvent::ToolUseStart {
                    index: 0,
                    id: "call_9".into(),
                    name: "Bash".into()
                },
                StreamEvent::InputJsonDelta {
                    index: 0,
                    partial_json: "{\"command\":".into()
                },
                StreamEvent::InputJsonDelta {
                    index: 0,
                    partial_json: "\"ls\"}".into()
                },
                StreamEvent::BlockStop { index: 0 },
                StreamEvent::StopReason {
                    stop_reason: Some("end_turn".into()),
                    output_tokens: Some(5)
                },
            ]
        );
    }

    /// Defensive: a function call whose deltas were skipped (empty `{}`
    /// arguments) gets the authoritative arguments from output_item.done.
    #[test]
    fn sse_backfills_missing_arguments() {
        let mut mapper = ResponsesSseMapper::new();
        let mut out = Vec::new();
        let mut push = |event: &str, data: &str| {
            for ev in mapper.feed(event, data).unwrap() {
                out.push(ev);
            }
        };
        push(
            "response.output_item.added",
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"Read","status":"in_progress","arguments":""}}"#,
        );
        push(
            "response.output_item.done",
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"function_call","call_id":"c1","name":"Read","status":"completed","arguments":"{\"path\":\"a.txt\"}"}}"#,
        );
        assert_eq!(
            out[0],
            StreamEvent::ToolUseStart {
                index: 0,
                id: "c1".into(),
                name: "Read".into()
            }
        );
        assert_eq!(
            out[1],
            StreamEvent::InputJsonDelta {
                index: 0,
                partial_json: "{\"path\":\"a.txt\"}".into()
            }
        );
        assert_eq!(out[2], StreamEvent::BlockStop { index: 0 });
    }

    /// Reasoning summaries map to the thinking affordance; unknown items are
    /// skipped but keep the block index sequence dense (two-layer flatten).
    #[test]
    fn sse_flattens_indexes_and_maps_reasoning() {
        let mut mapper = ResponsesSseMapper::new();
        let mut out = Vec::new();
        let mut push = |event: &str, data: &str| {
            for ev in mapper.feed(event, data).unwrap() {
                out.push(ev);
            }
        };
        // reasoning item (0), an ignored web_search item (1), text (2).
        push(
            "response.output_item.added",
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"r1","summary":[],"status":"in_progress"}}"#,
        );
        push(
            "response.reasoning_summary_text.delta",
            r#"{"type":"response.reasoning_summary_text.delta","item_id":"r1","output_index":0,"delta":"think"}"#,
        );
        push(
            "response.output_item.done",
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"r1","status":"completed","summary":[{"type":"summary_text","text":"think"}]}}"#,
        );
        push(
            "response.output_item.added",
            r#"{"type":"response.output_item.added","output_index":1,"item":{"type":"web_search_call","id":"w1","status":"in_progress"}}"#,
        );
        push(
            "response.output_item.done",
            r#"{"type":"response.output_item.done","output_index":1,"item":{"type":"web_search_call","id":"w1","status":"completed"}}"#,
        );
        push(
            "response.output_item.added",
            r#"{"type":"response.output_item.added","output_index":2,"item":{"type":"message","id":"m2","role":"assistant","status":"in_progress","content":[{"type":"output_text","text":"","annotations":[]}]}}"#,
        );
        push(
            "response.output_text.delta",
            r#"{"type":"response.output_text.delta","item_id":"m2","output_index":2,"content_index":0,"delta":"ans"}"#,
        );
        push(
            "response.output_item.done",
            r#"{"type":"response.output_item.done","output_index":2,"item":{"type":"message","id":"m2","role":"assistant","status":"completed","content":[{"type":"output_text","text":"ans","annotations":[]}]}}"#,
        );
        // block indexes are 0 (reasoning) and 1 (text) — dense, no gap.
        assert_eq!(out[0], StreamEvent::ThinkingStart { index: 0 });
        assert_eq!(
            out[1],
            StreamEvent::ThinkingDelta {
                index: 0,
                thinking: "think".into()
            }
        );
        assert_eq!(out[2], StreamEvent::BlockStop { index: 0 });
        assert_eq!(out[3], StreamEvent::TextStart { index: 1 });
        assert_eq!(
            out[4],
            StreamEvent::TextDelta {
                index: 1,
                text: "ans".into()
            }
        );
        assert_eq!(out[5], StreamEvent::BlockStop { index: 1 });
    }

    /// D125: raw reasoning text is the same affordance under a second event name, and
    /// arrives with the same `content_part.added` noise around it. Verified against the
    /// wire (a proxy fronting DeepSeek): the `reasoning` item opens the block and every
    /// token comes as `response.reasoning_text.delta`, so reading only the summary name
    /// rendered an empty thinking block.
    #[test]
    fn sse_maps_raw_reasoning_text_deltas_like_summaries() {
        let mut mapper = ResponsesSseMapper::new();
        let mut out = Vec::new();
        let mut push = |event: &str, data: &str| {
            for ev in mapper.feed(event, data).unwrap() {
                out.push(ev);
            }
        };
        push(
            "response.output_item.added",
            r#"{"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"r1","content":[],"summary":[],"status":"in_progress"}}"#,
        );
        push(
            "response.content_part.added",
            r#"{"type":"response.content_part.added","output_index":0,"content_index":0,"item_id":"r1","part":{"type":"reasoning_text","text":""}}"#,
        );
        push(
            "response.reasoning_text.delta",
            r#"{"type":"response.reasoning_text.delta","item_id":"r1","output_index":0,"content_index":0,"delta":"We need"}"#,
        );
        push(
            "response.output_item.done",
            r#"{"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"r1","status":"completed","content":[{"type":"reasoning_text","text":"We need"}],"summary":[]}}"#,
        );

        assert_eq!(
            out,
            vec![
                StreamEvent::ThinkingStart { index: 0 },
                StreamEvent::ThinkingDelta {
                    index: 0,
                    thinking: "We need".into()
                },
                StreamEvent::BlockStop { index: 0 },
            ],
            "the raw-reasoning stream produces a filled thinking block, not an empty one"
        );
    }

    /// incomplete(max_output_tokens) → the query loop's continuation string.
    #[test]
    fn sse_maps_incomplete_to_max_tokens() {
        let mut mapper = ResponsesSseMapper::new();
        let ev = mapper
            .feed(
                "response.incomplete",
                r#"{"type":"response.incomplete","response":{"id":"r","status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{"output_tokens":1024}}}"#,
            )
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            ev,
            StreamEvent::StopReason {
                stop_reason: Some("max_tokens".into()),
                output_tokens: Some(1024)
            }
        );
    }

    /// completed → end_turn (the query loop's normal stop path; §9).
    #[test]
    fn sse_maps_completed_to_end_turn() {
        let mut mapper = ResponsesSseMapper::new();
        let ev = mapper
            .feed(
                "response.completed",
                r#"{"type":"response.completed","response":{"id":"r","status":"completed","usage":{"output_tokens":42}}}"#,
            )
            .unwrap()
            .pop()
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
    fn retry_after_metadata_accepts_milliseconds_and_seconds() {
        assert_eq!(
            retry_after_from_value(&serde_json::json!({"retry_after_ms": 125})),
            Some(Duration::from_millis(125))
        );
        assert_eq!(
            retry_after_from_value(&serde_json::json!({
                "error": {"retry_after_ms": 250}
            })),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            retry_after_from_value(&serde_json::json!({"retry_after": 1.5})),
            Some(Duration::from_millis(1_500))
        );
        assert_eq!(
            retry_after_from_value(&serde_json::json!({"error": {"retry_delay": "3s"}})),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            retry_after_from_value(&serde_json::json!({"retry_after": "250ms"})),
            Some(Duration::from_millis(250))
        );
        assert_eq!(
            retry_after_from_value(&serde_json::json!({"retry_after": "2"})),
            Some(Duration::from_secs(2))
        );
        assert_eq!(
            retry_after_from_value(&serde_json::json!({"retry_after": "soon"})),
            None
        );
    }

    /// failed → ApiError with the error detail (never a silent Done; §9).
    #[test]
    fn sse_maps_failed_to_api_error() {
        let mut mapper = ResponsesSseMapper::new();
        let ev = mapper
            .feed(
                "response.failed",
                r#"{"type":"response.failed","response":{"id":"r","status":"failed","error":{"code":"server_error","message":"upstream unavailable"}}}"#,
            )
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(
            ev,
            StreamEvent::ApiError {
                message: "server_error: upstream unavailable".into(),
                kind: StreamApiErrorKind::Retryable,
                retry_after: None,
            }
        );
    }

    #[test]
    fn completion_text_joins_output_parts() {
        let body = serde_json::json!({
            "output": [
                {"type": "message", "content": [{"type": "output_text", "text": "a"}, {"type": "output_text", "text": "b"}]},
                {"type": "message", "content": [{"type": "output_text", "text": "c"}]},
            ]
        });
        assert_eq!(parse_completion_text(&body), "abc");
    }
}

#[cfg(test)]
mod codex_variant_tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::api::auth::{OauthFlowConfig, TokenProvider, TokenSet};

    /// Fake JWT with the given payload (header.payload.sig).
    fn jwt(payload: &serde_json::Value) -> String {
        use base64::Engine;
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let body =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());
        format!("{header}.{body}.sig")
    }

    /// One captured request (request_line, authorization, account_id, originator).
    type CapturedRequest = (String, String, String, String);

    /// Mock server capturing request lines + relevant headers.
    struct Capture {
        addr: String,
        requests: Arc<Mutex<Vec<CapturedRequest>>>,
        hits: Arc<AtomicUsize>,
    }

    async fn spawn_capture() -> Capture {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let requests: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let hits = Arc::new(AtomicUsize::new(0));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let reqs = requests.clone();
        let hits_c = hits.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 64 * 1024];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let mut lines = head.lines();
                let request_line = lines.next().unwrap_or_default().to_string();
                let mut authorization = String::new();
                let mut account_id = String::new();
                let mut originator = String::new();
                for line in lines {
                    let lower = line.to_ascii_lowercase();
                    if lower.strip_prefix("authorization:").is_some() {
                        authorization = line
                            .split_once(':')
                            .map(|(_, v)| v.trim().to_string())
                            .unwrap_or_default();
                    }
                    if lower.strip_prefix("chatgpt-account-id:").is_some() {
                        account_id = line
                            .split_once(':')
                            .map(|(_, v)| v.trim().to_string())
                            .unwrap_or_default();
                    }
                    if lower.strip_prefix("originator:").is_some() {
                        originator = line
                            .split_once(':')
                            .map(|(_, v)| v.trim().to_string())
                            .unwrap_or_default();
                    }
                }
                reqs.lock()
                    .unwrap()
                    .push((request_line, authorization, account_id, originator));
                hits_c.fetch_add(1, Ordering::SeqCst);
                // 404 responses endpoint: enough to complete the request.
                let body = r#"{"error":{"message":"mock"}}"#;
                let response = format!(
                    "HTTP/1.1 404 Not Found\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        Capture {
            addr: format!("http://{addr}"),
            requests,
            hits,
        }
    }

    fn tmp_home(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bingo-codex-v-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("home")
    }

    /// OAuth TokenProvider seeded with a **fresh** fake JWT access token
    /// (expires in the future → no refresh on use; the account id is
    /// backfilled from the JWT claims on save).
    async fn oauth_provider(home: &std::path::Path, access: &str) -> TokenProvider {
        let tp = TokenProvider::new(home, "codex", OauthFlowConfig::codex());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let tokens = TokenSet {
            access_token: access.into(),
            refresh_token: "rt".into(),
            id_token: None,
            expires_at: Some(now + 3600),
            account_id: None,
        };
        tp.save(&tokens).await.unwrap();
        tp
    }

    /// D65: bingo filters nothing. The default variant asks the endpoint for
    /// its own list — which is what the opencode-go preset does now that its
    /// one-model allowlist is gone.
    #[tokio::test]
    async fn default_variant_pulls_the_endpoint_model_list() {
        let cap = spawn_capture().await;
        let provider = OpenAIProvider::new(
            reqwest::Client::new(),
            AuthSource::ApiKey("sk-test".into()),
            cap.addr.clone(),
            false,
            OpenAiVariant::Default,
        );
        assert!(
            provider.list_models().await.is_err(),
            "the mock endpoint answers 404"
        );
        let requests = cap.requests.lock().unwrap();
        assert_eq!(requests.len(), 1, "exactly one request went out");
        assert!(
            requests[0].0.starts_with("GET /v1/models"),
            "got {}",
            requests[0].0
        );
    }

    /// Codex variant: POST goes to /codex/responses with the bearer + the
    /// ChatGPT-Account-Id header from JWT claims.
    #[tokio::test]
    async fn codex_variant_posts_to_codex_responses_with_account_header() {
        let cap = spawn_capture().await;
        let home = tmp_home("path");
        let access = jwt(&serde_json::json!({"chatgpt_account_id": "acc_1"}));
        let tp = oauth_provider(&home, &access).await;
        let provider = OpenAIProvider::new(
            reqwest::Client::new(),
            AuthSource::OAuth(Arc::new(tp)),
            cap.addr.clone(),
            false,
            OpenAiVariant::Codex,
        );
        let request = NeutralRequest {
            model: "gpt-5.5".into(),
            max_tokens: 100,
            system: vec![],
            messages: vec![],
            tools: vec![],
            stream: true,
            thinking: None,
        };
        let _ = provider.stream(&request).await;
        assert_eq!(cap.hits.load(Ordering::SeqCst), 1, "one request issued");
        let (request_line, authorization, account_id, originator) =
            cap.requests.lock().unwrap()[0].clone();
        assert!(
            request_line.starts_with("POST /codex/responses"),
            "codex variant path: {request_line}"
        );
        assert!(
            authorization.starts_with("Bearer "),
            "bearer header: {authorization}"
        );
        assert_eq!(
            account_id, "acc_1",
            "ChatGPT-Account-Id comes from the JWT claims"
        );
        assert_eq!(
            originator, "bingo",
            "codex variant sends the originator header"
        );
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    /// Codex dynamic model list: a live /codex/models response (9 models,
    /// data[].id shape) is returned as-is — /model shows the full set.
    #[tokio::test]
    async fn codex_variant_lists_models_dynamically() {
        let models = serde_json::json!({"data": [
            {"id": "gpt-5.6-sol"}, {"id": "gpt-5.6-sol-wm"}, {"id": "gpt-5.6-terra"},
            {"id": "gpt-5.6-luna"}, {"id": "gpt-5.5"}, {"id": "gpt-5.4"},
            {"id": "gpt-5.4-mini"}, {"id": "gpt-5.3-codex-spark"}, {"id": "codex-auto-review"},
        ]});
        let addr = spawn_json_server(models.to_string()).await;
        let home = tmp_home("dyn");
        let access = jwt(&serde_json::json!({"chatgpt_account_id": "acc_1"}));
        let tp = oauth_provider(&home, &access).await;
        let provider = OpenAIProvider::new(
            reqwest::Client::new(),
            AuthSource::OAuth(Arc::new(tp)),
            addr,
            false,
            OpenAiVariant::Codex,
        );
        let models = provider.list_models().await.unwrap();
        assert_eq!(models.len(), 9, "dynamic list of 9 models");
        assert!(models.contains(&"gpt-5.6-sol".to_string()));
        assert!(models.contains(&"codex-auto-review".to_string()));
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    /// JSON server returning a canned body (200 application/json).
    async fn spawn_json_server(body: String) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 64 * 1024];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    /// parse_models_list tolerates data[].id / plain array / models[] shapes.
    #[test]
    fn parses_model_list_shapes() {
        let data = serde_json::json!({"data": [{"id": "a"}, {"id": "b"}]});
        assert_eq!(parse_models_list(&data), Some(vec!["a".into(), "b".into()]));
        let arr = serde_json::json!(["a", "b"]);
        assert_eq!(parse_models_list(&arr), Some(vec!["a".into(), "b".into()]));
        let nested = serde_json::json!({"models": [{"id": "a"}]});
        assert_eq!(parse_models_list(&nested), Some(vec!["a".into()]));
        assert_eq!(parse_models_list(&serde_json::json!({"error": "x"})), None);
        assert_eq!(
            parse_models_list(&serde_json::json!({"data": []})),
            Some(vec![])
        );
    }

    /// Codex dynamic list fallback: an unreachable /codex/models endpoint
    /// must not hard-fail the /model menu — the static 9-model snapshot is
    /// returned instead.
    #[tokio::test]
    async fn codex_variant_falls_back_to_static_models() {
        let home = tmp_home("models");
        let tp = oauth_provider(&home, "at").await;
        let provider = OpenAIProvider::new(
            reqwest::Client::new(),
            AuthSource::OAuth(Arc::new(tp)),
            "http://127.0.0.1:9".into(),
            false,
            OpenAiVariant::Codex,
        );
        let models = provider.list_models().await.unwrap();
        assert_eq!(
            models,
            OpenAIProvider::CODEX_MODELS.to_vec(),
            "fallback static 9-model list"
        );
        assert_eq!(models.len(), 9);
        assert!(models.contains(&"gpt-5.6-luna".to_string()));
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    /// codex complete_text: the endpoint is stream-only — the adapter streams
    /// internally and aggregates the output text (neutral interface unchanged;
    /// compact/memory callers are unaffected).
    #[tokio::test]
    async fn codex_complete_text_aggregates_stream() {
        let sse = concat!(
            "event: response.created\ndata: {\"type\":\"response.created\",\"response\":{\"id\":\"r1\",\"model\":\"gpt-5.5\"}}\n\n",
            "event: response.output_item.added\ndata: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"m1\",\"role\":\"assistant\",\"status\":\"in_progress\",\"content\":[{\"type\":\"output_text\",\"text\":\"\",\"annotations\":[]}]}}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"m1\",\"output_index\":0,\"content_index\":0,\"delta\":\"Hel\"}\n\n",
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"m1\",\"output_index\":0,\"content_index\":0,\"delta\":\"lo\"}\n\n",
            "event: response.output_item.done\ndata: {\"type\":\"response.output_item.done\",\"output_index\":0,\"item\":{\"type\":\"message\",\"id\":\"m1\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"Hello\",\"annotations\":[]}]}}\n\n",
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"r1\",\"status\":\"completed\",\"usage\":{\"output_tokens\":5}}}\n\n",
        )
        .to_string();
        let addr = spawn_sse_server(sse).await;
        let home = tmp_home("ct");
        let access = jwt(&serde_json::json!({"chatgpt_account_id": "acc_1"}));
        let tp = oauth_provider(&home, &access).await;
        let provider = OpenAIProvider::new(
            reqwest::Client::new(),
            AuthSource::OAuth(Arc::new(tp)),
            addr,
            false,
            OpenAiVariant::Codex,
        );
        let request = NeutralRequest {
            model: "gpt-5.5".into(),
            max_tokens: 100,
            system: vec![],
            messages: vec![],
            tools: vec![],
            stream: false,
            thinking: None,
        };
        let text = provider.complete_text(&request).await.unwrap();
        assert_eq!(text, "Hello", "streaming aggregates the output text");
        let _ = std::fs::remove_dir_all(home.parent().unwrap());
    }

    /// SSE server returning a canned Responses stream (200 text/event-stream).
    async fn spawn_sse_server(sse: String) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 64 * 1024];
                let _ = socket.read(&mut buf).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                    sse.len(),
                    sse
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        format!("http://{addr}")
    }

    /// No cross-talk: the default variant never sends ChatGPT-Account-Id.
    #[tokio::test]
    async fn default_variant_does_not_send_account_header() {
        let cap = spawn_capture().await;
        let provider = OpenAIProvider::new(
            reqwest::Client::new(),
            AuthSource::ApiKey("sk-oa".into()),
            cap.addr.clone(),
            false,
            OpenAiVariant::Default,
        );
        let request = NeutralRequest {
            model: "gpt-5".into(),
            max_tokens: 100,
            system: vec![],
            messages: vec![],
            tools: vec![],
            stream: true,
            thinking: None,
        };
        let _ = provider.stream(&request).await;
        let (request_line, authorization, account_id, originator) =
            cap.requests.lock().unwrap()[0].clone();
        assert!(
            request_line.starts_with("POST /v1/responses"),
            "default variant path: {request_line}"
        );
        assert_eq!(
            account_id, "",
            "default variant sends no ChatGPT-Account-Id"
        );
        assert_eq!(
            originator, "",
            "default variant sends no originator (no cross-talk)"
        );
        assert!(authorization.starts_with("Bearer sk-oa"), "{authorization}");
    }
}

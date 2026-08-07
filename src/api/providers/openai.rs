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

use super::{backoff, retryable};
use crate::api::contract::{
    AuthStatus, BoxStream, Capabilities, ClientError, NeutralRequest, ProviderClient, StreamEvent,
    SystemBlock, ThinkingLevel,
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

/// thinking level → Responses `reasoning.effort`. OpenAI accepts
/// minimal/low/medium/high; bingo's xhigh/max converge to high (depth beyond
/// the public effort ladder is a Claude-5 concept).
fn effort_for(level: ThinkingLevel) -> &'static str {
    match level {
        ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        ThinkingLevel::High | ThinkingLevel::Xhigh | ThinkingLevel::Max => "high",
    }
}

/// The endpoint (one per provider instance; mirrors the anthropic adapter).
#[derive(Debug, Clone)]
struct Endpoint {
    api_key: String,
    base_url: String,
    supports_images: bool,
}

#[derive(Debug, Clone)]
pub struct OpenAIProvider {
    http: reqwest::Client,
    endpoint: Arc<std::sync::RwLock<Endpoint>>,
}

impl OpenAIProvider {
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
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", endpoint.api_key))
                .map_err(|e| ClientError::InvalidApiKey(e.to_string()))?,
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Ok(headers)
    }

    fn base_url(&self) -> String {
        self.endpoint
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .base_url
            .clone()
    }
}

/// NeutralRequest → Responses request body.
fn build_body(request: &NeutralRequest) -> serde_json::Value {
    let mut body = serde_json::json!({
        "model": request.model,
        "max_output_tokens": request.max_tokens,
        "stream": request.stream,
        "input": build_input(&request.messages),
    });
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
        body["reasoning"] = serde_json::json!({ "effort": effort_for(level) });
        // Reasoning summaries drive the thinking UI affordance (encrypted
        // reasoning is not replayable — v1 discards it, per D33 §10).
        body["include"] = serde_json::json!(["reasoning.summary_text"]);
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
        match content {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        }
    }
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
            "response.reasoning_summary_text.delta" => {
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
                let message = response
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("response failed")
                    .to_string();
                Ok(vec![StreamEvent::ApiError { message }])
            }
            "error" => {
                let payload: ErrorPayload =
                    serde_json::from_value(value).map_err(|e| format!("bad error payload: {e}"))?;
                Ok(vec![StreamEvent::ApiError {
                    message: if let Some(code) = payload.code {
                        format!("{code}: {}", payload.message)
                    } else {
                        payload.message
                    },
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
        AuthStatus::ApiKey
    }

    async fn stream(&self, request: &NeutralRequest) -> Result<BoxStream, ClientError> {
        let body = build_body(request);
        let mut attempt = 0;
        let base_url = self.base_url();
        loop {
            let builder = self
                .http
                .post(format!("{base_url}/v1/responses"))
                .headers(self.headers()?)
                .json(&body);
            match tokio::time::timeout(REQUEST_TIMEOUT, builder.send()).await {
                Ok(Ok(response)) if response.status().is_success() => {
                    return Ok(Box::pin(stream_body(response)));
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
                    tokio::time::sleep(retry_after.unwrap_or_else(|| backoff(attempt))).await;
                }
                Ok(Ok(response)) => {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    return Err(ClientError::Api {
                        status: status.as_u16(),
                        body,
                    });
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
        let mut body = build_body(request);
        body["stream"] = serde_json::json!(false);
        let base_url = self.base_url();
        let response = tokio::time::timeout(
            SHORT_WRITE_TIMEOUT,
            self.http
                .post(format!("{base_url}/v1/responses"))
                .headers(self.headers()?)
                .json(&body)
                .send(),
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
        Ok(parse_completion_text(&body))
    }

    async fn list_models(&self) -> Result<Vec<String>, ClientError> {
        let base_url = self.base_url();
        let response = tokio::time::timeout(
            SHORT_READ_TIMEOUT,
            self.http
                .get(format!("{base_url}/v1/models"))
                .headers(self.headers()?)
                .send(),
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

    /// No public count_tokens endpoint on the Responses API (D33 §4.2): the
    /// token gate falls back to local estimation.
    async fn count_tokens(
        &self,
        _model: &str,
        _system: &[SystemBlock],
        _messages: &[Message],
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
        let body = build_body(&r);
        assert_eq!(body["instructions"], "role\n\ntools");
        assert_eq!(body["reasoning"], serde_json::json!({"effort": "high"}));
        assert_eq!(
            body["include"],
            serde_json::json!(["reasoning.summary_text"])
        );
        assert_eq!(body["max_output_tokens"], 1024);
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
        let body = build_body(&r);
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

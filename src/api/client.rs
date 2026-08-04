use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use thiserror::Error;

use super::sse::SseParser;
use super::types::{parse_sse_event, Request, StreamEvent, API_BASE, API_VERSION};
use super::types::{ContentBlock, Role, SystemBlock};

pub const MAX_RETRIES: u32 = 2;

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("missing API key: set ANTHROPIC_API_KEY or DEEPSEEK_API_KEY")]
    MissingApiKey,
    #[error("API error: HTTP {status}: {body}")]
    Api { status: u16, body: String },
    #[error("API stream error: {0}")]
    Stream(String),
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
}

#[derive(Debug, Clone)]
pub struct Client {
    http: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl Client {
    pub fn from_env() -> Result<Self, ClientError> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .or_else(|_| std::env::var("DEEPSEEK_API_KEY"))
            .map_err(|_| ClientError::MissingApiKey)?;
        Ok(Self::new(
            api_key,
            std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| API_BASE.to_string()),
        ))
    }

    pub fn new(api_key: String, base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_key,
            base_url,
        }
    }

    /// DeepSeek 兼容端点对 cache_control 处理不稳定（偶发挂起），需禁用。
    pub fn is_deepseek(&self) -> bool {
        self.base_url.contains("deepseek")
    }

    fn headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-api-key", HeaderValue::from_str(&self.api_key).unwrap());
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(API_VERSION),
        );
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers
    }

    /// 发起流式请求，返回归一化事件流。
    pub async fn stream(
        &self,
        request: &Request,
    ) -> Result<impl futures_util::Stream<Item = Result<StreamEvent, ClientError>>, ClientError>
    {
        let mut attempt = 0;
        loop {
            let builder = self
                .http
                .post(format!("{}/v1/messages", self.base_url))
                .headers(self.headers())
                .json(request);
            match builder.send().await {
                Ok(response) if response.status().is_success() => {
                    return Ok(self.stream_body(response));
                }
                Ok(response) if retryable(&response.status()) => {
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
        }
    }

    /// 非流式补全：返回回复文本（compact 摘要、记忆提取用）。
    pub async fn complete_text(
        &self,
        request: &Request,
    ) -> Result<String, ClientError> {
        let mut request = request.clone();
        request.stream = false;
        let response = self
            .http
            .post(format!("{}/v1/messages", self.base_url))
            .headers(self.headers())
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ClientError::Api {
                status: status.as_u16(),
                body,
            });
        }
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

    /// 输入 token 计数（D12：预算显示走官方 count_tokens API）。
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
        let response = self
            .http
            .post(format!("{}/v1/messages/count_tokens", self.base_url))
            .headers(self.headers())
            .json(&payload)
            .send()
            .await?;
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
                match body.next().await {
                    Some(Ok(chunk)) => {
                        for frame in parser.feed(&chunk) {
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
                    Some(Err(e)) => {
                        yield Err(ClientError::Transport(e));
                        return;
                    }
                    None => break,
                }
            }
        }
    }
}

/// 指数退避 + jitter：500ms → 1s → 2s。
fn backoff(attempt: u32) -> Duration {
    let base_ms = 500u64 << attempt.min(4);
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

/// 把一条完整 assistant 回复的流事件累积成回传消息。
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
    use crate::api::types::StreamEvent;

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

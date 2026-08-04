use serde::{Deserialize, Serialize};

pub const API_BASE: &str = "https://api.anthropic.com";
pub const API_VERSION: &str = "2023-06-01";
pub const DEFAULT_MODEL: &str = "claude-sonnet-5";
pub const DEFAULT_MAX_TOKENS: u32 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    #[serde(rename_all = "snake_case")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        tool_use_id: String,
        content: serde_json::Value,
        #[serde(skip_serializing_if = "is_false")]
        is_error: bool,
    },
    #[serde(rename_all = "snake_case")]
    Thinking {
        thinking: String,
        signature: String,
    },
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub model: String,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub system: String,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<serde_json::Value>,
    pub stream: bool,
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
    Text {
        text: String,
    },
    #[serde(rename = "thinking_delta")]
    Thinking {
        thinking: String,
    },
    #[serde(rename = "signature_delta")]
    Signature {
        signature: String,
    },
    #[serde(rename = "input_json_delta")]
    InputJson {
        partial_json: String,
    },
}

#[derive(Debug, Deserialize)]
struct ContentBlockDeltaPayload {
    index: usize,
    delta: DeltaPayload,
}

#[derive(Debug, Deserialize)]
struct MessageDeltaPayload {
    delta: MessageDeltaInner,
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

/// 归一化后的流式事件，供 queryLoop 消费。
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    MessageStart { id: String, model: String },
    TextStart { index: usize },
    ThinkingStart { index: usize },
    ToolUseStart { index: usize, id: String, name: String },
    TextDelta { index: usize, text: String },
    ThinkingDelta { index: usize, thinking: String },
    SignatureDelta { index: usize, signature: String },
    InputJsonDelta { index: usize, partial_json: String },
    BlockStop { index: usize },
    StopReason { stop_reason: Option<String> },
    Done,
    ApiError { message: String },
}

fn parse_content_block_start(data: &str) -> Result<Option<StreamEvent>, String> {
    let value: serde_json::Value =
        serde_json::from_str(data).map_err(|e| format!("bad content_block_start: {e}"))?;
    let index = value
        .get("index")
        .and_then(|i| i.as_u64())
        .unwrap_or(0) as usize;
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

/// 把一条 SSE event/data 对解析成 StreamEvent。
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
        _other => Ok(None), // 未知事件类型：忽略，向前兼容
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_text_delta() {
        let ev = parse_sse_event(
            "content_block_delta",
            r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Hello"}}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(ev, StreamEvent::TextDelta { index: 1, text: "Hello".into() });
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
            StreamEvent::ToolUseStart { index: 2, id: "tu_1".into(), name: "Bash".into() }
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
        assert_eq!(ev, StreamEvent::StopReason { stop_reason: Some("end_turn".into()) });
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
            StreamEvent::ApiError { message: "overloaded_error: Overloaded".into() }
        );
    }

    #[test]
    fn ignores_ping_and_unknown() {
        assert_eq!(parse_sse_event("ping", "{}").unwrap(), None);
        assert_eq!(parse_sse_event("weird_event", "{}").unwrap(), None);
    }
}

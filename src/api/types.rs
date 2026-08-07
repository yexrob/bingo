use serde::{Deserialize, Serialize};

pub const API_BASE: &str = "https://api.anthropic.com";
pub const API_VERSION: &str = "2023-06-01";
pub const DEFAULT_MODEL: &str = "claude-sonnet-5";
pub const DEFAULT_MAX_TOKENS: u32 = 64_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
        #[serde(default, skip_serializing_if = "is_false")]
        is_error: bool,
    },
    #[serde(rename_all = "snake_case")]
    Thinking {
        thinking: String,
        signature: String,
    },
    /// Image content block (Anthropic Messages protocol base64 form:
    /// `{"type":"image","source":{"type":"base64","media_type":...,"data":...}}`).
    Image {
        source: ImageSource,
    },
}

/// Image-block data source (the protocol fixes `type: "base64"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageSource {
    #[serde(rename = "type", default = "base64_source_type")]
    pub source_type: String,
    pub media_type: String,
    pub data: String,
}

fn base64_source_type() -> String {
    "base64".into()
}

impl ImageSource {
    pub fn base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            source_type: "base64".into(),
            media_type: media_type.into(),
            data: data.into(),
        }
    }
}

/// Image attachment mounted on the input box (base64 data; only built into a
/// `ContentBlock::Image` when sending).
#[derive(Debug, Clone)]
pub struct ImageAttachment {
    pub media_type: String,
    pub data: String,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// A single system-prompt block (may carry cache_control).
#[derive(Debug, Clone)]
pub struct SystemBlock {
    pub text: String,
    /// Carry `cache_control: ephemeral` on the API request.
    pub cache: bool,
}

impl Serialize for SystemBlock {
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

#[derive(Debug, Clone, Serialize)]
pub struct Request {
    pub model: String,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub system: Vec<SystemBlock>,
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<serde_json::Value>,
    pub stream: bool,
    /// Thinking config: `{"type":"adaptive"}` (None = send no parameter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<serde_json::Value>,
    /// Output config: `{"effort": <level>}` (None = send no parameter).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<serde_json::Value>,
}

/// Thinking levels accepted by `/think` and `settings.thinkingLevel`
/// (levels besides `off`; aligned with Claude Code's /effort levels).
pub const THINKING_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Thinking level → request `thinking` parameter.
///
/// The Claude 5 family (including the default `claude-sonnet-5`) rejects
/// `{"type":"enabled","budget_tokens":N}` with a 400 — `adaptive` is the only
/// on-mode, so every enabled level sends the same adaptive shape; depth goes
/// through [`effort_param`] instead. off/unset sends no parameter at all
/// (keeps DeepSeek/ollama endpoints happy).
pub fn thinking_param(level: Option<&str>) -> Option<serde_json::Value> {
    THINKING_LEVELS
        .contains(&level?)
        .then(|| serde_json::json!({ "type": "adaptive" }))
}

/// Thinking level → request `output_config` parameter (`{"effort": <level>}`)。
///
/// Same gating as [`thinking_param`]: off/unset sends no parameter; the level
/// is the effort level (a GA parameter of the Claude 5 family — below `high`
/// saves tokens, xhigh/max think deeper).
pub fn effort_param(level: Option<&str>) -> Option<serde_json::Value> {
    let level = level?;
    THINKING_LEVELS
        .contains(&level)
        .then(|| serde_json::json!({ "effort": level }))
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

/// Normalized streaming event, consumed by queryLoop.
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
    StopReason {
        stop_reason: Option<String>,
        output_tokens: Option<u64>,
    },
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

/// Parse one SSE event/data pair into a `StreamEvent`.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The Claude 5 family only accepts `adaptive`; the budget_tokens shape
    /// is a 400 everywhere.
    #[test]
    fn thinking_param_is_adaptive_for_every_enabled_level() {
        for level in THINKING_LEVELS {
            let param = thinking_param(Some(level)).unwrap();
            assert_eq!(param, serde_json::json!({ "type": "adaptive" }), "{level}");
            assert!(param.get("budget_tokens").is_none(), "{level} 不得带 budget");
        }
    }

    #[test]
    fn thinking_param_omitted_when_off_or_unset() {
        assert_eq!(thinking_param(None), None, "未配置不发参数");
        assert_eq!(thinking_param(Some("off")), None);
        assert_eq!(thinking_param(Some("bogus")), None);
    }

    /// Levels map to effort levels; off/unknown levels are suppressed
    /// together with thinking.
    #[test]
    fn effort_param_follows_thinking_gate() {
        for level in THINKING_LEVELS {
            assert_eq!(
                effort_param(Some(level)),
                Some(serde_json::json!({ "effort": level })),
                "{level}"
            );
        }
        assert_eq!(effort_param(None), None);
        assert_eq!(effort_param(Some("off")), None);
        assert_eq!(effort_param(Some("bogus")), None);
    }

    #[test]
    fn request_serializes_thinking_only_when_set() {
        let mut req = Request {
            model: "m".into(),
            max_tokens: 100,
            system: Vec::new(),
            messages: Vec::new(),
            tools: Vec::new(),
            stream: true,
            thinking: None,
            output_config: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("thinking").is_none(), "无 thinking 不序列化");
        assert!(json.get("output_config").is_none(), "无 output_config 不序列化");
        req.thinking = thinking_param(Some("xhigh"));
        req.output_config = effort_param(Some("xhigh"));
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["thinking"], serde_json::json!({ "type": "adaptive" }));
        assert_eq!(json["output_config"], serde_json::json!({ "effort": "xhigh" }));
    }

    /// Image blocks serialize in Anthropic base64 form; a missing
    /// source_type falls back to base64.
    #[test]
    fn image_block_serializes_anthropic_format() {
        let block = ContentBlock::Image {
            source: ImageSource::base64("image/png", "aGVsbG8="),
        };
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/png",
                    "data": "aGVsbG8=",
                }
            })
        );
        // Deserialization round-trip; falls back to base64 when source.type
        // is missing.
        let round: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(round, block);
        let no_type: ContentBlock = serde_json::from_value(serde_json::json!({
            "type": "image",
            "source": { "media_type": "image/jpeg", "data": "eA==" }
        }))
        .unwrap();
        assert_eq!(
            no_type,
            ContentBlock::Image { source: ImageSource::base64("image/jpeg", "eA==") }
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
            StreamEvent::ApiError { message: "overloaded_error: Overloaded".into() }
        );
    }

    #[test]
    fn ignores_ping_and_unknown() {
        assert_eq!(parse_sse_event("ping", "{}").unwrap(), None);
        assert_eq!(parse_sse_event("weird_event", "{}").unwrap(), None);
    }
}

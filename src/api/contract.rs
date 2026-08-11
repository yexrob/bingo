//! Provider-neutral protocol contract (D33): every adapter (anthropic, openai,
//! future) implements `ProviderClient` against these types; consumers (query
//! loop, compact, memory, TUI) never see protocol wire JSON.

use std::pin::Pin;

use async_trait::async_trait;
use futures_util::Stream;
use thiserror::Error;

use super::types::{ContentBlock, Message, Role};

/// Client error shared by every provider adapter. Stable codes via
/// [`ErrorCode`](crate::error::ErrorCode) (see the code table in error.rs).
#[derive(Debug, Error)]
pub enum ClientError {
    #[error(
        "missing credentials: set apiKey in ~/.config/bingo/settings.json, export ANTHROPIC_API_KEY, or run /provider login codex (ChatGPT subscription)"
    )]
    MissingApiKey,
    #[error("invalid API key for HTTP header: {0}")]
    InvalidApiKey(String),
    #[error("API error: HTTP {status}: {body}")]
    Api { status: u16, body: String },
    #[error("API context overflow: HTTP {status}: {body}")]
    ContextOverflow { status: u16, body: String },
    #[error("API stream error: {0}")]
    Stream(String),
    #[error("transport error: {0}")]
    Transport(#[from] reqwest::Error),
    /// The server gave no response within the request timeout.
    #[error("request timed out")]
    Timeout,
    /// OAuth problem (not logged in / refresh failed / ...), D33 §6.
    #[error("{0}")]
    Auth(String),
    /// The current protocol/endpoint does not support this operation (e.g.
    /// count_tokens on the Responses API) — callers degrade gracefully.
    #[error("{0}")]
    Unsupported(String),
    /// Provider configuration error (unknown protocol etc.), surfaced at
    /// startup before any request goes out.
    #[error("{0}")]
    Config(String),
}

impl ClientError {
    pub fn from_response(status: u16, body: String) -> Self {
        if is_context_overflow(status, &body) {
            Self::ContextOverflow { status, body }
        } else {
            Self::Api { status, body }
        }
    }
}

fn is_context_overflow(status: u16, body: &str) -> bool {
    if status != 400 && status != 413 {
        return false;
    }
    let body = body.to_ascii_lowercase();
    [
        "context length",
        "context window",
        "context limit",
        "max context",
        "maximum context",
        "input exceeds",
        "input is too long",
        "input too long",
        "prompt is too long",
        "prompt too long",
        "too many tokens",
        "token limit",
    ]
    .iter()
    .any(|feature| body.contains(feature))
        || (body.contains("maximum")
            && body.contains("token")
            && (body.contains("prompt") || body.contains("input")))
}

impl From<crate::api::auth::AuthError> for ClientError {
    fn from(e: crate::api::auth::AuthError) -> Self {
        ClientError::Auth(e.to_string())
    }
}

impl crate::error::ErrorCode for ClientError {
    /// Outbound mapping (see the `src/error.rs` code table): every variant
    /// explicitly returns a stable code, and the match is exhaustive with no
    /// `_` arm — a new variant that is not handled fails to compile.
    fn error_code(&self) -> &'static str {
        match self {
            ClientError::MissingApiKey | ClientError::InvalidApiKey(_) => "AUTH_REQUIRED",
            ClientError::Auth(_) => "AUTH_REQUIRED",
            ClientError::Config(_) => "CONFIG_INVALID",
            ClientError::Api { status: 401, .. } => "AUTH_REQUIRED",
            ClientError::Api { status: 403, .. } => "PERMISSION_DENIED",
            ClientError::Api { status: 429, .. } => "RATE_LIMITED",
            ClientError::ContextOverflow { .. } => "CONTEXT_OVERFLOW",
            // Remaining non-success responses (4xx outside the above / 5xx):
            // server-interaction anomaly, action = "retry later".
            ClientError::Api { .. } => "SERVER_ERROR",
            ClientError::Stream(_) => "SERVER_ERROR",
            ClientError::Unsupported(_) => "SERVER_ERROR",
            ClientError::Transport(_) => transport_offline_code(),
            ClientError::Timeout => "TIMEOUT",
        }
    }
}

/// Locks in the Transport-arm mapping (`#[doc(hidden)]`, only for the
/// drift-guard unit test to assert).
#[doc(hidden)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn transport_offline_code() -> &'static str {
    "OFFLINE"
}

/// Thinking depth levels (besides off; aligned with Claude Code's /effort
/// levels). `None` in a request = no thinking parameter sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingLevel {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// Level names, in the same order as the enum (the `/think` menu table is
/// `off` + this list; consistency is guarded by a test in tui/chat.rs).
pub const THINKING_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

impl ThinkingLevel {
    /// Parse a level name; `off`, `None` and unknown values yield `None`
    /// (no thinking parameter — keeps DeepSeek/ollama endpoints happy).
    pub fn parse(level: Option<&str>) -> Option<ThinkingLevel> {
        match level {
            Some("low") => Some(Self::Low),
            Some("medium") => Some(Self::Medium),
            Some("high") => Some(Self::High),
            Some("xhigh") => Some(Self::Xhigh),
            Some("max") => Some(Self::Max),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Provider capabilities, declared statically per protocol (v1; no runtime
/// negotiation). Defaults are the Anthropic protocol baseline; adapters
/// override from config. `supports_images` is live in P0; the remaining
/// fields are consumed by the openai adapter (P1, same batch).
#[derive(Debug, Clone)]
#[allow(dead_code)] // P1: openai adapter consumes the non-image capabilities
pub struct Capabilities {
    /// Whether this endpoint's model accepts image content blocks.
    pub supports_images: bool,
    pub supports_thinking: bool,
    /// Whether a token-count endpoint exists (OpenAI has none — the token
    /// gate falls back to local estimation, D6).
    pub supports_count_tokens: bool,
    pub supports_prompt_caching: bool,
    pub context_window: u64,
    pub max_output_tokens: u32,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            // The Anthropic Messages API and the OpenAI Responses API both carry image blocks;
            // an endpoint that cannot is the exception and declares itself so in config.
            supports_images: true,
            supports_thinking: true,
            supports_count_tokens: true,
            supports_prompt_caching: true,
            context_window: 200_000,
            max_output_tokens: 64_000,
        }
    }
}

/// Auth state surfaced by `/provider` and `/status` (OAuth lands in P2).
#[derive(Debug, Clone)]
#[allow(dead_code)] // P2: /provider login|logout surface
pub enum AuthStatus {
    ApiKey,
    /// apiKey-type provider whose key lives in auth.json (`/provider login
    /// --manual`): `configured` is read live, so logging in takes effect in
    /// the running session.
    StoredKey {
        configured: bool,
    },
    OAuth {
        account: Option<String>,
    },
    /// No credentials at all (the default provider before onboarding).
    Unconfigured,
}

/// A single system-prompt segment; `cache` marks the segment as cacheable
/// (each protocol maps it to its own caching mechanism).
#[derive(Debug, Clone)]
pub struct SystemBlock {
    pub text: String,
    /// Mark this segment for prompt caching on the wire.
    pub cache: bool,
}

/// Provider-neutral request built by the consumers; each adapter maps it to
/// its own wire format.
#[derive(Debug, Clone)]
pub struct NeutralRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: Vec<SystemBlock>,
    pub messages: Vec<Message>,
    pub tools: Vec<serde_json::Value>,
    pub stream: bool,
    /// Level → wire param per protocol (`adaptive`+effort for anthropic,
    /// `reasoning.effort` for openai); None = send no thinking parameter.
    pub thinking: Option<ThinkingLevel>,
}

/// Normalized streaming event, consumed by the query loop and the TUI.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    MessageStart {
        id: String,
        model: String,
    },
    TextStart {
        index: usize,
    },
    ThinkingStart {
        index: usize,
    },
    ToolUseStart {
        index: usize,
        id: String,
        name: String,
    },
    TextDelta {
        index: usize,
        text: String,
    },
    ThinkingDelta {
        index: usize,
        thinking: String,
    },
    SignatureDelta {
        index: usize,
        signature: String,
    },
    InputJsonDelta {
        index: usize,
        partial_json: String,
    },
    BlockStop {
        index: usize,
    },
    StopReason {
        stop_reason: Option<String>,
        output_tokens: Option<u64>,
    },
    Done,
    ApiError {
        message: String,
    },
}

pub type BoxStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ClientError>> + Send>>;

/// A provider protocol implementation. `Client` (api::client) is the facade:
/// provider switching, forks and endpoint info; adapters own the wire.
#[async_trait]
pub trait ProviderClient: Send + Sync {
    fn capabilities(&self) -> Capabilities;
    /// P2: OAuth providers report account state here.
    #[allow(dead_code)]
    fn auth_status(&self) -> AuthStatus;

    /// Start a streaming request, returning a normalized event stream.
    async fn stream(&self, request: &NeutralRequest) -> Result<BoxStream, ClientError>;

    /// Non-streaming completion (compact summaries, memory extraction).
    async fn complete_text(&self, request: &NeutralRequest) -> Result<String, ClientError>;

    /// List the models the endpoint supports (`/model` menu).
    async fn list_models(&self) -> Result<Vec<String>, ClientError>;

    /// Exact input token count (D6). Protocols without a count endpoint
    /// fall back to local estimation (see compact.rs).
    async fn count_tokens(
        &self,
        model: &str,
        system: &[SystemBlock],
        messages: &[Message],
    ) -> Result<u64, ClientError>;
}

/// Accumulates a complete assistant reply's stream events into a returnable
/// message (protocol-neutral; shared by every adapter's stream).
#[derive(Debug, Default)]
pub struct AssistantAccumulator {
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>,
    in_flight: Option<InFlight>,
}

#[derive(Debug)]
enum InFlight {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: String,
    },
}

impl AssistantAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, event: &StreamEvent) -> Result<(), String> {
        match event {
            StreamEvent::TextStart { index } => {
                self.ensure_slot(*index)?;
                self.in_flight = Some(InFlight::Text {
                    text: String::new(),
                });
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
            StreamEvent::InputJsonDelta {
                index,
                partial_json,
            } => {
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
                    InFlight::Thinking {
                        thinking,
                        signature,
                    } => ContentBlock::Thinking {
                        thinking,
                        signature,
                    },
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
            return Err(format!(
                "block start out of order: {index} != {}",
                self.content.len()
            ));
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

    pub fn finish(&mut self) -> bool {
        let Some(in_flight) = self.in_flight.take() else {
            return false;
        };
        let block = match in_flight {
            InFlight::Text { text } => Some(ContentBlock::Text { text }),
            InFlight::Thinking {
                thinking,
                signature,
            } => Some(ContentBlock::Thinking {
                thinking,
                signature,
            }),
            InFlight::ToolUse { .. } => None,
        };
        if let Some(block) = block {
            self.content.push(block);
        }
        if self.stop_reason.as_deref() != Some("max_tokens") {
            self.stop_reason = Some("truncated".to_string());
        }
        true
    }

    pub fn message(&self) -> Message {
        Message {
            role: Role::Assistant,
            content: self.content.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_overflow_response_classification() {
        for (status, body) in [
            (
                400,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 211000 tokens > 200000 maximum"}}"#,
            ),
            (
                400,
                r#"{"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 132450 tokens."}}"#,
            ),
            (
                413,
                r#"{"error":{"message":"The input exceeds the context window for this model."}}"#,
            ),
        ] {
            assert!(
                matches!(
                    ClientError::from_response(status, body.to_string()),
                    ClientError::ContextOverflow { status: actual, .. } if actual == status
                ),
                "status={status}, body={body}"
            );
        }
    }

    #[test]
    fn context_overflow_requires_400_or_413_and_a_message_feature() {
        assert!(matches!(
            ClientError::from_response(500, "maximum context length".to_string()),
            ClientError::Api { status: 500, .. }
        ));
        assert!(matches!(
            ClientError::from_response(400, "invalid request".to_string()),
            ClientError::Api { status: 400, .. }
        ));
        assert!(matches!(
            ClientError::from_response(413, "request too large".to_string()),
            ClientError::Api { status: 413, .. }
        ));
    }

    #[test]
    fn thinking_level_parse_and_display() {
        assert_eq!(ThinkingLevel::parse(Some("low")), Some(ThinkingLevel::Low));
        assert_eq!(ThinkingLevel::parse(Some("max")), Some(ThinkingLevel::Max));
        assert_eq!(ThinkingLevel::parse(Some("off")), None);
        assert_eq!(ThinkingLevel::parse(None), None);
        assert_eq!(ThinkingLevel::parse(Some("bogus")), None);
        assert_eq!(ThinkingLevel::High.as_str(), "high");
    }

    /// The accumulator turns a normalized event sequence into a message,
    /// regardless of which protocol produced the events.
    #[test]
    fn accumulates_text_and_thinking() {
        let mut acc = AssistantAccumulator::new();
        acc.push(&StreamEvent::ThinkingStart { index: 0 }).unwrap();
        acc.push(&StreamEvent::ThinkingDelta {
            index: 0,
            thinking: "plan".into(),
        })
        .unwrap();
        acc.push(&StreamEvent::SignatureDelta {
            index: 0,
            signature: "sig123".into(),
        })
        .unwrap();
        acc.push(&StreamEvent::BlockStop { index: 0 }).unwrap();
        acc.push(&StreamEvent::TextStart { index: 1 }).unwrap();
        acc.push(&StreamEvent::TextDelta {
            index: 1,
            text: "hi".into(),
        })
        .unwrap();
        acc.push(&StreamEvent::BlockStop { index: 1 }).unwrap();
        assert_eq!(
            acc.content,
            vec![
                ContentBlock::Thinking {
                    thinking: "plan".into(),
                    signature: "sig123".into()
                },
                ContentBlock::Text { text: "hi".into() },
            ]
        );
        assert_eq!(acc.message().role, Role::Assistant);
    }

    #[test]
    fn accumulates_tool_use_input() {
        let mut acc = AssistantAccumulator::new();
        acc.push(&StreamEvent::ToolUseStart {
            index: 0,
            id: "tu_9".into(),
            name: "Bash".into(),
        })
        .unwrap();
        acc.push(&StreamEvent::InputJsonDelta {
            index: 0,
            partial_json: "{\"command\":".into(),
        })
        .unwrap();
        acc.push(&StreamEvent::InputJsonDelta {
            index: 0,
            partial_json: "\"ls\"}".into(),
        })
        .unwrap();
        acc.push(&StreamEvent::BlockStop { index: 0 }).unwrap();
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
        acc.push(&StreamEvent::ToolUseStart {
            index: 0,
            id: "tu_1".into(),
            name: "Bash".into(),
        })
        .unwrap();
        acc.push(&StreamEvent::BlockStop { index: 0 }).unwrap();
        assert!(matches!(&acc.content[0], ContentBlock::ToolUse { input, .. } if input.is_null()));
    }

    #[test]
    fn finish_preserves_max_tokens_for_unclosed_text() {
        let mut acc = AssistantAccumulator::new();
        acc.push(&StreamEvent::TextStart { index: 0 }).unwrap();
        acc.push(&StreamEvent::TextDelta {
            index: 0,
            text: "partial answer".into(),
        })
        .unwrap();
        acc.push(&StreamEvent::StopReason {
            stop_reason: Some("max_tokens".into()),
            output_tokens: Some(4),
        })
        .unwrap();

        assert!(acc.finish());
        assert_eq!(acc.stop_reason.as_deref(), Some("max_tokens"));
        assert_eq!(
            acc.content,
            vec![ContentBlock::Text {
                text: "partial answer".into(),
            }]
        );
    }

    #[test]
    fn finish_marks_unclosed_block_truncated_for_non_recoverable_stop_reason() {
        let mut acc = AssistantAccumulator::new();
        acc.push(&StreamEvent::TextStart { index: 0 }).unwrap();
        acc.push(&StreamEvent::TextDelta {
            index: 0,
            text: "partial answer".into(),
        })
        .unwrap();
        acc.push(&StreamEvent::StopReason {
            stop_reason: Some("stop_sequence".into()),
            output_tokens: Some(4),
        })
        .unwrap();

        assert!(acc.finish());
        assert_eq!(acc.stop_reason.as_deref(), Some("truncated"));
    }

    #[test]
    fn finish_preserves_unclosed_thinking_as_truncated() {
        let mut acc = AssistantAccumulator::new();
        acc.push(&StreamEvent::ThinkingStart { index: 0 }).unwrap();
        acc.push(&StreamEvent::ThinkingDelta {
            index: 0,
            thinking: "unfinished plan".into(),
        })
        .unwrap();
        acc.push(&StreamEvent::StopReason {
            stop_reason: Some("end_turn".into()),
            output_tokens: Some(4),
        })
        .unwrap();

        assert!(acc.finish());
        assert_eq!(acc.stop_reason.as_deref(), Some("truncated"));
        assert_eq!(
            acc.content,
            vec![ContentBlock::Thinking {
                thinking: "unfinished plan".into(),
                signature: String::new(),
            }]
        );
    }

    #[test]
    fn finish_drops_unclosed_tool_use_and_marks_truncated() {
        let mut acc = AssistantAccumulator::new();
        acc.push(&StreamEvent::ToolUseStart {
            index: 0,
            id: "tu_1".into(),
            name: "Bash".into(),
        })
        .unwrap();

        assert!(acc.finish());
        assert!(acc.content.is_empty());
        assert_eq!(acc.stop_reason.as_deref(), Some("truncated"));
    }

    #[test]
    fn out_of_order_blocks_error() {
        let mut acc = AssistantAccumulator::new();
        acc.push(&StreamEvent::TextStart { index: 1 }).unwrap_err();
        acc.push(&StreamEvent::TextStart { index: 0 }).unwrap();
        acc.push(&StreamEvent::BlockStop { index: 0 }).unwrap();
        assert!(
            acc.push(&StreamEvent::TextStart { index: 0 }).is_err(),
            "index reuse must error"
        );
    }
}

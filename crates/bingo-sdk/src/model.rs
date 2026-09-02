//! The provider-neutral model vocabulary: messages and content parts as the
//! kernel stores them, the request a provider receives, and the stream events
//! it yields. The stream mirrors the Vercel `@ai-sdk/provider` V4 part algebra:
//! per-block ids, start/delta/end triples, a `{unified, raw}` finish reason,
//! and provider metadata keyed by provider id so signatures and encrypted
//! reasoning round-trip without the kernel knowing what they are.

use std::collections::BTreeMap;
use std::pin::Pin;

use futures::Stream;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::ErrorCode;

/// Provider-private data attached to a message or part, keyed by provider id.
/// The kernel never reads it; `ContextView::fold` passes it back only to the
/// provider that produced it.
pub type ProviderMetadata = BTreeMap<String, serde_json::Map<String, Value>>;

fn is_empty_meta(m: &ProviderMetadata) -> bool {
    m.is_empty()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub role: Role,
    pub parts: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "is_empty_meta")]
    pub provider_options: ProviderMetadata,
}

impl Message {
    pub fn user(parts: Vec<ContentPart>) -> Self {
        Self {
            role: Role::User,
            parts,
            provider_options: ProviderMetadata::new(),
        }
    }

    pub fn assistant(parts: Vec<ContentPart>) -> Self {
        Self {
            role: Role::Assistant,
            parts,
            provider_options: ProviderMetadata::new(),
        }
    }

    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            parts: vec![ContentPart::text(text)],
            provider_options: ProviderMetadata::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ContentPart {
    Text {
        text: String,
    },
    Image {
        media_type: String,
        /// Base64 payload.
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        parts: Vec<ContentPart>,
        #[serde(default)]
        is_error: bool,
    },
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "is_empty_meta")]
        provider_metadata: ProviderMetadata,
    },
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        ContentPart::Text { text: text.into() }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentPart::Text { text } => Some(text),
            _ => None,
        }
    }
}

/// One cacheable segment of the system prompt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SystemBlock {
    pub text: String,
    #[serde(default)]
    pub cache: bool,
}

/// A tool as the model sees it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// What a catalogue may show beside the tool (an MCP tool's `server`);
    /// never sent to a model.
    #[serde(default, skip_serializing_if = "is_empty_map")]
    pub meta: serde_json::Map<String, Value>,
}

fn is_empty_map(m: &serde_json::Map<String, Value>) -> bool {
    m.is_empty()
}

/// Reasoning effort, in the vocabulary providers converge on.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum Effort {
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: Vec<SystemBlock>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<Effort>,
    /// Whose turn this is. A stateless provider ignores it; one that keeps a
    /// conversation of its own per session — an ACP adapter holding one agent
    /// session per bingo session (ADR-0035 §3), a stateful wire like the
    /// Responses API — has no other way to know which conversation it is
    /// answering. `None` is a request built by hand or a side question, which
    /// belongs to no session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<crate::ids::SessionId>,
    #[serde(default, skip_serializing_if = "is_empty_meta")]
    pub provider_options: ProviderMetadata,
}

/// Token counts as the provider reports them, the three input counts kept
/// apart: `input_tokens` is what the model read fresh, the cache counts are
/// what it read from or wrote to a cached prefix. The whole input side is
/// `input_total()`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

impl Usage {
    /// Every input token the model saw this round, cached or not.
    pub fn input_total(&self) -> u64 {
        self.input_tokens + self.cache_read_tokens + self.cache_write_tokens
    }

    pub fn add(&mut self, other: Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum UnifiedFinish {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Error,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FinishReason {
    pub unified: UnifiedFinish,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
}

impl FinishReason {
    pub fn unified(unified: UnifiedFinish) -> Self {
        Self { unified, raw: None }
    }
}

/// What an endpoint does with a request, as only the provider can know:
/// whether image parts reach the model, whether tokens can be counted ahead,
/// whether prefixes are cached. The model's own facts — window, output
/// budget, reasoning, vision — are the kernel catalogue's (ADR-0004).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EndpointCapabilities {
    #[serde(default)]
    pub images: bool,
    #[serde(default)]
    pub count_tokens: bool,
    #[serde(default)]
    pub caching: bool,
}

/// What a turn may assume about its model: the kernel's resolution of the
/// user's settings, the catalogue, the server's corrections and the
/// endpoint's facts (ADR-0004). The ruler and the gate read this.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    pub context_window: u64,
    pub max_output: u64,
    #[serde(default)]
    pub images: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub count_tokens: bool,
    #[serde(default)]
    pub caching: bool,
}

/// Provider stream events. Never published; the accumulator folds them into items.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ModelEvent {
    StreamStart {
        #[serde(default)]
        warnings: Vec<String>,
    },
    ResponseMetadata {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    TextStart {
        id: String,
    },
    TextDelta {
        id: String,
        delta: String,
    },
    TextEnd {
        id: String,
    },
    ReasoningStart {
        id: String,
    },
    ReasoningDelta {
        id: String,
        delta: String,
    },
    ReasoningEnd {
        id: String,
        #[serde(default, skip_serializing_if = "is_empty_meta")]
        provider_metadata: ProviderMetadata,
    },
    ToolInputStart {
        id: String,
        name: String,
    },
    ToolInputDelta {
        id: String,
        delta: String,
    },
    ToolInputEnd {
        id: String,
    },
    /// The complete call. `input` is the raw JSON text; the loop parses it once.
    ToolCall {
        id: String,
        name: String,
        input: String,
    },
    Finish {
        usage: Usage,
        finish_reason: FinishReason,
    },
}

pub type ModelStream = Pin<Box<dyn Stream<Item = Result<ModelEvent, ProviderError>> + Send>>;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProviderError {
    #[error("authentication required: {message}")]
    Auth { message: String },
    #[error("rate limited")]
    RateLimited {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_after_ms: Option<u64>,
    },
    #[error("context overflow: {message}")]
    ContextOverflow { message: String },
    #[error("server error {status}: {message}")]
    Server { status: u16, message: String },
    #[error("bad request: {message}")]
    Request { message: String },
    #[error("transport: {message}")]
    Transport { message: String },
    #[error("stream: {message}")]
    Stream { message: String },
    #[error("timeout")]
    Timeout,
    #[error("unsupported: {message}")]
    Unsupported { message: String },
    #[error("configuration: {message}")]
    Config { message: String },
}

impl ProviderError {
    pub fn retryable(&self) -> bool {
        matches!(
            self,
            ProviderError::RateLimited { .. }
                | ProviderError::Server { .. }
                | ProviderError::Transport { .. }
                | ProviderError::Stream { .. }
                | ProviderError::Timeout
        )
    }

    pub fn retry_after_ms(&self) -> Option<u64> {
        match self {
            ProviderError::RateLimited { retry_after_ms } => *retry_after_ms,
            _ => None,
        }
    }

    pub fn code(&self) -> ErrorCode {
        match self {
            ProviderError::Auth { .. } => ErrorCode::AuthRequired,
            ProviderError::RateLimited { .. } => ErrorCode::RateLimited,
            ProviderError::ContextOverflow { .. } => ErrorCode::ContextOverflow,
            ProviderError::Server { .. } => ErrorCode::ServerError,
            ProviderError::Request { .. }
            | ProviderError::Config { .. }
            | ProviderError::Unsupported { .. } => ErrorCode::InvalidInput,
            ProviderError::Transport { .. } | ProviderError::Stream { .. } => ErrorCode::Offline,
            ProviderError::Timeout => ErrorCode::Timeout,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_result_nests_parts_and_round_trips() {
        let msg = Message::user(vec![ContentPart::ToolResult {
            tool_use_id: "call_1".into(),
            parts: vec![ContentPart::text("ok")],
            is_error: false,
        }]);
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["parts"][0]["type"], "toolResult");
        assert_eq!(serde_json::from_value::<Message>(json).unwrap(), msg);
    }

    #[test]
    fn empty_provider_metadata_is_omitted() {
        let part = ContentPart::Reasoning {
            text: "hm".into(),
            provider_metadata: ProviderMetadata::new(),
        };
        let json = serde_json::to_string(&part).unwrap();
        assert!(!json.contains("providerMetadata"));
    }

    #[test]
    fn retryability_follows_the_error_kind() {
        assert!(ProviderError::Timeout.retryable());
        assert!(
            ProviderError::Server {
                status: 503,
                message: String::new()
            }
            .retryable()
        );
        assert!(
            !ProviderError::Auth {
                message: String::new()
            }
            .retryable()
        );
        assert!(
            !ProviderError::ContextOverflow {
                message: String::new()
            }
            .retryable()
        );
    }
}

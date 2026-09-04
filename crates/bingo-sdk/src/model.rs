//! The provider-neutral model vocabulary: messages and content parts as the
//! kernel stores them, the request a provider receives, and the stream events
//! it yields. The stream mirrors the Vercel `@ai-sdk/provider` V4 part algebra:
//! per-block ids, start/delta/end triples, a `{unified, raw}` finish reason,
//! and provider metadata keyed by provider id so signatures and encrypted
//! reasoning round-trip without the kernel knowing what they are.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use base64::Engine;
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

/// Extensions a picture may arrive under, and the media type each is sent as
/// (ADR-0040): the one table a wire client, the fs tool and the TUI's
/// mention completion all read.
const MEDIA_TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

/// A picture handed to the model, already the shape the journal keeps and a
/// provider's request encodes: one struct, so a surface that reads a picture
/// off disk, a wire client that already holds the bytes, and a tool result
/// that returns one all produce the same thing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    pub media_type: String,
    /// Base64 payload.
    pub data: String,
}

impl Image {
    /// Decoded bytes, not the base64 that carries them: beyond this a
    /// picture is a mistake, not a request.
    pub const MAX_BYTES: usize = 5 * 1024 * 1024;

    /// The extensions [`Image::media_type_of`] recognises, read off the one
    /// table.
    pub fn extensions() -> impl Iterator<Item = &'static str> {
        MEDIA_TYPES.iter().map(|(ext, _)| *ext)
    }

    /// What a path's extension is sent as, when it names a picture at all.
    pub fn media_type_of(path: &Path) -> Option<&'static str> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        MEDIA_TYPES
            .iter()
            .find(|(name, _)| *name == ext)
            .map(|(_, media)| *media)
    }

    /// Whether a media type is one the table knows, whoever handed it in.
    pub fn is_known(media_type: &str) -> bool {
        MEDIA_TYPES.iter().any(|(_, known)| *known == media_type)
    }

    /// Bytes, checked against the table and the cap, then base64-encoded —
    /// the one place a picture becomes this shape.
    pub fn from_bytes(media_type: impl Into<String>, bytes: &[u8]) -> Result<Image, ImageError> {
        let media_type = media_type.into();
        if !Self::is_known(&media_type) {
            return Err(ImageError::UnknownMediaType(media_type));
        }
        if bytes.len() > Self::MAX_BYTES {
            return Err(ImageError::TooLarge {
                bytes: bytes.len(),
                max: Self::MAX_BYTES,
            });
        }
        Ok(Image {
            media_type,
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        })
    }

    /// A picture read off disk (std, not tokio: a surface calls this off its
    /// own thread or accepts the blocking read); the extension says what it
    /// is sent as.
    pub fn read(path: &Path) -> Result<Image, ImageError> {
        let media_type =
            Self::media_type_of(path).ok_or_else(|| ImageError::NotAnImage(path.to_path_buf()))?;
        let bytes = std::fs::read(path).map_err(|source| ImageError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_bytes(media_type, &bytes)
    }

    /// The decoded size, from the base64 length alone — no decode needed.
    /// A payload that is not base64 is bounded, not trusted: the arithmetic
    /// saturates and the provider is where it fails.
    pub fn decoded_len(&self) -> usize {
        let padding = self.data.bytes().rev().take_while(|&b| b == b'=').count();
        ((self.data.len() / 4) * 3).saturating_sub(padding)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    #[error("not an image: {}", .0.display())]
    NotAnImage(PathBuf),
    #[error("unknown image media type: {0}")]
    UnknownMediaType(String),
    #[error("image too large: {bytes} bytes, the limit is {max}")]
    TooLarge { bytes: usize, max: usize },
    #[error("reading {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
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
    Image(Image),
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

impl Effort {
    /// Lowest first, which is the order a person reads them in.
    pub const ALL: [Effort; 6] = [
        Effort::Minimal,
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    /// The one spelling of a level: what a person types after `/think`, and
    /// what a status line shows them back. The wire spelling a provider wants
    /// is that provider's own business (their ladders differ).
    pub fn name(self) -> &'static str {
        match self {
            Effort::Minimal => "minimal",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// A level by that name, in any case; `None` is not a level.
    pub fn parse(word: &str) -> Option<Self> {
        let lower = word.to_ascii_lowercase();
        Self::ALL.into_iter().find(|e| e.name() == lower)
    }
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

    /// A level has two spellings and they are not the same: the wire is
    /// camelCase, as every enum on it is, while [`Effort::name`] is the word
    /// a person types and reads. A surface showing a level shows the name.
    #[test]
    fn a_level_is_spelled_one_way_for_a_person_and_another_on_the_wire() {
        for level in Effort::ALL {
            assert_eq!(Effort::parse(level.name()), Some(level));
            assert_eq!(Effort::parse(&level.name().to_uppercase()), Some(level));
        }
        assert_eq!(Effort::XHigh.name(), "xhigh");
        assert_eq!(
            serde_json::to_value(Effort::XHigh).unwrap(),
            Value::from("xHigh")
        );
        assert_eq!(Effort::parse("loud"), None);
    }

    /// The wire shape is load-bearing (ADR-0040): an internally tagged
    /// newtype variant flattens the struct's fields beside the tag, and this
    /// is the exact JSON a client and the journal both read.
    #[test]
    fn an_image_part_tags_and_flattens_beside_it() {
        let part = ContentPart::Image(Image {
            media_type: "image/png".into(),
            data: "iVBORw0KGgo=".into(),
        });
        let json = serde_json::to_value(&part).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "type": "image",
                "mediaType": "image/png",
                "data": "iVBORw0KGgo=",
            })
        );
        assert_eq!(serde_json::from_value::<ContentPart>(json).unwrap(), part);
    }

    #[test]
    fn a_payload_that_is_only_padding_is_bounded_not_a_panic() {
        let image = Image {
            media_type: "image/png".into(),
            data: "==".into(),
        };
        assert_eq!(image.decoded_len(), 0);
    }

    #[test]
    fn a_known_extension_resolves_and_an_unknown_one_does_not() {
        assert_eq!(
            Image::media_type_of(Path::new("shot.PNG")),
            Some("image/png")
        );
        assert_eq!(Image::media_type_of(Path::new("shot.txt")), None);
        assert_eq!(Image::media_type_of(Path::new("shot")), None);
    }

    #[test]
    fn from_bytes_refuses_an_unknown_type_and_an_oversized_payload() {
        assert!(matches!(
            Image::from_bytes("image/tiff", b"x"),
            Err(ImageError::UnknownMediaType(t)) if t == "image/tiff"
        ));
        let big = vec![0u8; Image::MAX_BYTES + 1];
        assert!(matches!(
            Image::from_bytes("image/png", &big),
            Err(ImageError::TooLarge { bytes, max })
                if bytes == Image::MAX_BYTES + 1 && max == Image::MAX_BYTES
        ));
    }

    #[test]
    fn from_bytes_encodes_a_known_type_within_the_cap() {
        let image = Image::from_bytes("image/png", b"abc").unwrap();
        assert_eq!(image.media_type, "image/png");
        assert_eq!(image.data, "YWJj");
        assert_eq!(image.decoded_len(), 3);
    }

    #[test]
    fn decoded_len_reads_the_base64_length_without_decoding() {
        assert_eq!(
            Image::from_bytes("image/png", b"").unwrap().decoded_len(),
            0
        );
        assert_eq!(
            Image::from_bytes("image/png", b"a").unwrap().decoded_len(),
            1
        );
        assert_eq!(
            Image::from_bytes("image/png", b"abcdefgh")
                .unwrap()
                .decoded_len(),
            8
        );
    }

    #[test]
    fn read_rejects_a_path_the_table_does_not_know() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-an-image.txt");
        std::fs::write(&path, b"hello").unwrap();
        assert!(matches!(Image::read(&path), Err(ImageError::NotAnImage(p)) if p == path));
    }

    #[test]
    fn read_loads_a_known_picture_off_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pixel.png");
        std::fs::write(&path, [0x89, b'P', b'N', b'G']).unwrap();
        let image = Image::read(&path).unwrap();
        assert_eq!(image.media_type, "image/png");
    }

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

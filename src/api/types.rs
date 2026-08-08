use serde::{Deserialize, Serialize};

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

#[cfg(test)]
mod tests {
    use super::*;

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
            ContentBlock::Image {
                source: ImageSource::base64("image/jpeg", "eA==")
            }
        );
    }
}

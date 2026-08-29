//! The Anthropic event stream as a state machine: one wire `event`/`data`
//! pair in, the `ModelEvent`s it produces out.
//!
//! Pure — no I/O and no clock — so the `fixtures/*.sse` files drive it
//! directly. It holds only what a block needs in order to close itself: the
//! accumulated tool-input JSON, the thinking signature, and the usage the
//! message reports in two halves.

use std::collections::BTreeMap;

use bingo_sdk::{FinishReason, ModelEvent, ProviderError, ProviderMetadata, UnifiedFinish, Usage};
use serde_json::{Map, Value};

/// The provider id `provider_metadata` is keyed by (ADR-0002 §7).
pub const PROVIDER: &str = "anthropic";

#[derive(Debug, Default)]
pub struct Decoder {
    open: BTreeMap<u64, Block>,
    usage: Usage,
    stop_reason: Option<String>,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// The events this wire event produces. `Err` ends the stream: either an
    /// `error` event the server sent, or a frame this protocol cannot be.
    pub fn decode(&mut self, event: &str, data: &str) -> Result<Vec<ModelEvent>, ProviderError> {
        match event {
            "message_start" => self.message_start(data),
            "content_block_start" => self.block_start(data),
            "content_block_delta" => self.block_delta(data),
            "content_block_stop" => self.block_stop(data),
            "message_delta" => self.message_delta(data),
            "message_stop" => Ok(vec![self.finish()]),
            "error" => Err(error_event(data)),
            // `ping`, and whatever the API adds after this was written.
            _ => Ok(Vec::new()),
        }
    }

    fn message_start(&mut self, data: &str) -> Result<Vec<ModelEvent>, ProviderError> {
        let value = parse(data, "message_start")?;
        let message = value.get("message").unwrap_or(&Value::Null);
        self.read_usage(message.get("usage"));
        Ok(vec![
            ModelEvent::StreamStart {
                warnings: Vec::new(),
            },
            ModelEvent::ResponseMetadata {
                id: str_at(message, "id").map(str::to_string),
                model: str_at(message, "model").map(str::to_string),
            },
        ])
    }

    fn block_start(&mut self, data: &str) -> Result<Vec<ModelEvent>, ProviderError> {
        let value = parse(data, "content_block_start")?;
        let content = value
            .get("content_block")
            .ok_or_else(|| malformed("content_block_start without a content_block"))?;
        let Some(block) = Block::open(index_of(&value), content)? else {
            return Ok(Vec::new());
        };
        let started = block.start();
        self.open.insert(index_of(&value), block);
        Ok(vec![started])
    }

    fn block_delta(&mut self, data: &str) -> Result<Vec<ModelEvent>, ProviderError> {
        let value = parse(data, "content_block_delta")?;
        let delta = value
            .get("delta")
            .ok_or_else(|| malformed("content_block_delta without a delta"))?;
        let Some(block) = self.open.get_mut(&index_of(&value)) else {
            return Ok(Vec::new());
        };
        Ok(block.delta(delta).into_iter().collect())
    }

    fn block_stop(&mut self, data: &str) -> Result<Vec<ModelEvent>, ProviderError> {
        let value = parse(data, "content_block_stop")?;
        Ok(self
            .open
            .remove(&index_of(&value))
            .map(Block::close)
            .unwrap_or_default())
    }

    fn message_delta(&mut self, data: &str) -> Result<Vec<ModelEvent>, ProviderError> {
        let value = parse(data, "message_delta")?;
        self.read_usage(value.get("usage"));
        if let Some(reason) = value.get("delta").and_then(|d| str_at(d, "stop_reason")) {
            self.stop_reason = Some(reason.to_string());
        }
        Ok(Vec::new())
    }

    fn finish(&self) -> ModelEvent {
        ModelEvent::Finish {
            usage: self.usage,
            finish_reason: finish_reason(self.stop_reason.as_deref()),
        }
    }

    /// Anthropic reports the three input counts apart and never sums them, and
    /// the sdk keeps them apart too, so the ruler can tell a cache read from a
    /// fresh prefix (the old `anthropic.rs:226` summed them into one field).
    fn read_usage(&mut self, usage: Option<&Value>) {
        let Some(usage) = usage else { return };
        if let Some(n) = u64_at(usage, "input_tokens") {
            self.usage.input_tokens = n;
        }
        if let Some(n) = u64_at(usage, "cache_read_input_tokens") {
            self.usage.cache_read_tokens = n;
        }
        if let Some(n) = u64_at(usage, "cache_creation_input_tokens") {
            self.usage.cache_write_tokens = n;
        }
        if let Some(n) = u64_at(usage, "output_tokens") {
            self.usage.output_tokens = n;
        }
    }
}

/// One open content block. Its `id` is the sdk's block id: the Anthropic block
/// index for text and thinking, the `tool_use.id` for a call, so the loop can
/// answer a call without a second table mapping index to id.
#[derive(Debug)]
struct Block {
    id: String,
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    Text,
    /// `signature_delta` arrives after the thinking text and has to travel
    /// back on the next turn; a redacted block carries opaque `data` instead.
    Reasoning {
        signature: String,
        redacted: Option<String>,
    },
    /// `input_json_delta` fragments, joined when the block stops.
    Tool {
        name: String,
        input: String,
    },
}

impl Block {
    /// `None` for a block type this adapter does not model; the stream stays
    /// forward-compatible instead of failing on it.
    fn open(index: u64, content: &Value) -> Result<Option<Self>, ProviderError> {
        let kind = match str_at(content, "type")
            .ok_or_else(|| malformed("content_block without a type"))?
        {
            "text" => Kind::Text,
            "thinking" => Kind::Reasoning {
                signature: String::new(),
                redacted: None,
            },
            "redacted_thinking" => Kind::Reasoning {
                signature: String::new(),
                redacted: Some(str_at(content, "data").unwrap_or_default().to_string()),
            },
            "tool_use" => Kind::Tool {
                name: str_at(content, "name")
                    .ok_or_else(|| malformed("tool_use without a name"))?
                    .to_string(),
                input: String::new(),
            },
            _ => return Ok(None),
        };
        let id = match kind {
            Kind::Tool { .. } => str_at(content, "id")
                .ok_or_else(|| malformed("tool_use without an id"))?
                .to_string(),
            _ => index.to_string(),
        };
        Ok(Some(Self { id, kind }))
    }

    fn start(&self) -> ModelEvent {
        let id = self.id.clone();
        match &self.kind {
            Kind::Text => ModelEvent::TextStart { id },
            Kind::Reasoning { .. } => ModelEvent::ReasoningStart { id },
            Kind::Tool { name, .. } => ModelEvent::ToolInputStart {
                id,
                name: name.clone(),
            },
        }
    }

    fn delta(&mut self, delta: &Value) -> Option<ModelEvent> {
        let id = self.id.clone();
        match (str_at(delta, "type")?, &mut self.kind) {
            ("text_delta", Kind::Text) => Some(ModelEvent::TextDelta {
                id,
                delta: str_at(delta, "text")?.to_string(),
            }),
            ("thinking_delta", Kind::Reasoning { .. }) => Some(ModelEvent::ReasoningDelta {
                id,
                delta: str_at(delta, "thinking")?.to_string(),
            }),
            ("signature_delta", Kind::Reasoning { signature, .. }) => {
                signature.push_str(str_at(delta, "signature")?);
                None
            }
            ("input_json_delta", Kind::Tool { input, .. }) => {
                let fragment = str_at(delta, "partial_json")?;
                input.push_str(fragment);
                Some(ModelEvent::ToolInputDelta {
                    id,
                    delta: fragment.to_string(),
                })
            }
            _ => None,
        }
    }

    fn close(self) -> Vec<ModelEvent> {
        let id = self.id;
        match self.kind {
            Kind::Text => vec![ModelEvent::TextEnd { id }],
            Kind::Reasoning {
                signature,
                redacted,
            } => vec![ModelEvent::ReasoningEnd {
                provider_metadata: reasoning_metadata(&signature, redacted.as_deref()),
                id,
            }],
            Kind::Tool { name, input } => vec![
                ModelEvent::ToolInputEnd { id: id.clone() },
                ModelEvent::ToolCall {
                    id,
                    name,
                    input: tool_input(input),
                },
            ],
        }
    }
}

/// What has to travel back to the API for a reasoning block to be replayable:
/// the signature it was signed with, or a redacted block's opaque payload.
fn reasoning_metadata(signature: &str, redacted: Option<&str>) -> ProviderMetadata {
    let mut mine = Map::new();
    if !signature.is_empty() {
        mine.insert("signature".into(), Value::String(signature.into()));
    }
    if let Some(data) = redacted {
        mine.insert("redacted".into(), Value::String(data.into()));
    }
    if mine.is_empty() {
        return ProviderMetadata::new();
    }
    ProviderMetadata::from([(PROVIDER.to_string(), mine)])
}

/// A tool called with no arguments sends no `input_json_delta` at all. The
/// loop parses this text exactly once, and `{}` is what "no arguments" means.
fn tool_input(input: String) -> String {
    if input.trim().is_empty() {
        return "{}".to_string();
    }
    input
}

pub fn finish_reason(raw: Option<&str>) -> FinishReason {
    let Some(raw) = raw else {
        return FinishReason::unified(UnifiedFinish::Other);
    };
    let unified = match raw {
        "end_turn" | "stop_sequence" => UnifiedFinish::Stop,
        "max_tokens" => UnifiedFinish::Length,
        "tool_use" => UnifiedFinish::ToolCalls,
        "refusal" => UnifiedFinish::ContentFilter,
        _ => UnifiedFinish::Other,
    };
    FinishReason {
        unified,
        raw: Some(raw.to_string()),
    }
}

fn error_event(data: &str) -> ProviderError {
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return crate::error::stream_error("", data);
    };
    let error = value.get("error").unwrap_or(&value);
    crate::error::stream_error(
        str_at(error, "type").unwrap_or_default(),
        str_at(error, "message").unwrap_or(data),
    )
}

fn parse(data: &str, what: &str) -> Result<Value, ProviderError> {
    serde_json::from_str(data).map_err(|e| malformed(format!("bad {what}: {e}")))
}

fn malformed(message: impl Into<String>) -> ProviderError {
    ProviderError::Stream {
        message: message.into(),
    }
}

fn index_of(value: &Value) -> u64 {
    value.get("index").and_then(Value::as_u64).unwrap_or(0)
}

fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn u64_at(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::SseParser;

    /// Every event a fixture yields, in order, up to the error that ends it.
    pub(crate) fn replay(fixture: &str) -> (Vec<ModelEvent>, Option<ProviderError>) {
        let body = std::fs::read(crate::tests::fixture(fixture)).expect("read the fixture");
        let mut parser = SseParser::new();
        let mut decoder = Decoder::new();
        let mut events = Vec::new();
        let frames = parser.feed(&body).expect("framed");
        for frame in frames.iter().chain(parser.finish().iter()) {
            match decoder.decode(&frame.event, &frame.data) {
                Ok(decoded) => events.extend(decoded),
                Err(error) => return (events, Some(error)),
            }
        }
        (events, None)
    }

    fn events(fixture: &str) -> Vec<ModelEvent> {
        let (events, error) = replay(fixture);
        assert_eq!(error, None, "{fixture} must not fail");
        events
    }

    fn usage(fixture: &str) -> Usage {
        match events(fixture).pop() {
            Some(ModelEvent::Finish { usage, .. }) => usage,
            other => panic!("{fixture} must end with a finish, got {other:?}"),
        }
    }

    #[test]
    fn a_text_turn_starts_streams_and_finishes() {
        assert_eq!(
            events("text.sse"),
            vec![
                ModelEvent::StreamStart {
                    warnings: Vec::new()
                },
                ModelEvent::ResponseMetadata {
                    id: Some("msg_01Text".into()),
                    model: Some("claude-sonnet-4-5-20250929".into()),
                },
                ModelEvent::TextStart { id: "0".into() },
                ModelEvent::TextDelta {
                    id: "0".into(),
                    delta: "Hello".into()
                },
                ModelEvent::TextDelta {
                    id: "0".into(),
                    delta: ", world.".into()
                },
                ModelEvent::TextEnd { id: "0".into() },
                ModelEvent::Finish {
                    usage: Usage {
                        input_tokens: 12,
                        output_tokens: 7,
                        cache_read_tokens: 2048,
                        cache_write_tokens: 320,
                        reasoning_tokens: 0,
                    },
                    finish_reason: FinishReason {
                        unified: UnifiedFinish::Stop,
                        raw: Some("end_turn".into()),
                    },
                },
            ],
            "a ping contributes nothing"
        );
    }

    #[test]
    fn the_three_input_counts_stay_apart() {
        assert_eq!(
            usage("text.sse"),
            Usage {
                input_tokens: 12,
                output_tokens: 7,
                cache_read_tokens: 2048,
                cache_write_tokens: 320,
                reasoning_tokens: 0,
            }
        );
    }

    #[test]
    fn a_tool_block_is_keyed_by_its_tool_use_id_and_ends_with_the_whole_call() {
        let events = events("tools.sse");
        assert_eq!(
            &events[events.len() - 6..],
            &[
                ModelEvent::ToolInputStart {
                    id: "toolu_01Read".into(),
                    name: "Read".into(),
                },
                ModelEvent::ToolInputDelta {
                    id: "toolu_01Read".into(),
                    delta: r#"{"file_path":"#.into(),
                },
                ModelEvent::ToolInputDelta {
                    id: "toolu_01Read".into(),
                    delta: r#""Cargo.toml"}"#.into(),
                },
                ModelEvent::ToolInputEnd {
                    id: "toolu_01Read".into()
                },
                ModelEvent::ToolCall {
                    id: "toolu_01Read".into(),
                    name: "Read".into(),
                    input: r#"{"file_path":"Cargo.toml"}"#.into(),
                },
                ModelEvent::Finish {
                    usage: Usage {
                        input_tokens: 1400,
                        output_tokens: 58,
                        cache_read_tokens: 11000,
                        cache_write_tokens: 0,
                        reasoning_tokens: 0,
                    },
                    finish_reason: FinishReason {
                        unified: UnifiedFinish::ToolCalls,
                        raw: Some("tool_use".into()),
                    },
                },
            ]
        );
    }

    #[test]
    fn a_signature_rides_home_on_the_reasoning_end() {
        let events = events("thinking.sse");
        let ends: Vec<&ModelEvent> = events
            .iter()
            .filter(|e| matches!(e, ModelEvent::ReasoningEnd { .. }))
            .collect();
        assert_eq!(
            ends,
            vec![
                &ModelEvent::ReasoningEnd {
                    id: "0".into(),
                    provider_metadata: reasoning_metadata("ErUBCkYIBBgCIkA=", None),
                },
                &ModelEvent::ReasoningEnd {
                    id: "1".into(),
                    provider_metadata: reasoning_metadata("", Some("EroBCkYIBBgCKkBRedacted==")),
                },
            ]
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ModelEvent::ReasoningDelta { delta, .. } if delta.is_empty())),
            "a signature delta is metadata, not reasoning text"
        );
    }

    #[test]
    fn a_max_tokens_stop_finishes_as_length() {
        assert!(matches!(
            events("max_tokens.sse").pop(),
            Some(ModelEvent::Finish {
                finish_reason: FinishReason {
                    unified: UnifiedFinish::Length,
                    ..
                },
                ..
            })
        ));
    }

    #[test]
    fn a_mid_stream_error_event_ends_the_stream_and_keeps_what_came_before() {
        let (events, error) = replay("error_mid_stream.sse");
        assert_eq!(
            error,
            Some(ProviderError::Server {
                status: 529,
                message: "overloaded_error: Overloaded".into(),
            })
        );
        assert!(error.is_some_and(|e| e.retryable()));
        assert!(matches!(events.last(), Some(ModelEvent::TextDelta { .. })));
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, ModelEvent::Finish { .. }))
        );
    }

    #[test]
    fn every_stop_reason_maps_and_an_unknown_one_is_kept_raw() {
        for (raw, unified) in [
            ("end_turn", UnifiedFinish::Stop),
            ("stop_sequence", UnifiedFinish::Stop),
            ("max_tokens", UnifiedFinish::Length),
            ("tool_use", UnifiedFinish::ToolCalls),
            ("refusal", UnifiedFinish::ContentFilter),
            ("pause_turn", UnifiedFinish::Other),
        ] {
            assert_eq!(
                finish_reason(Some(raw)),
                FinishReason {
                    unified,
                    raw: Some(raw.into())
                }
            );
        }
        assert_eq!(finish_reason(None).unified, UnifiedFinish::Other);
    }

    #[test]
    fn a_tool_called_with_no_arguments_still_carries_parsable_json() {
        let mut decoder = Decoder::new();
        decoder
            .decode(
                "content_block_start",
                r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"Now","input":{}}}"#,
            )
            .expect("a start");
        let closed = decoder
            .decode("content_block_stop", r#"{"index":0}"#)
            .expect("a stop");
        assert_eq!(
            closed[1],
            ModelEvent::ToolCall {
                id: "toolu_1".into(),
                name: "Now".into(),
                input: "{}".into(),
            }
        );
    }

    #[test]
    fn an_unknown_block_type_and_an_unknown_event_are_ignored() {
        let mut decoder = Decoder::new();
        assert!(
            decoder
                .decode(
                    "content_block_start",
                    r#"{"index":0,"content_block":{"type":"server_tool_use"}}"#
                )
                .expect("ignored")
                .is_empty()
        );
        assert!(
            decoder
                .decode("something_new", "{}")
                .expect("ignored")
                .is_empty()
        );
    }

    #[test]
    fn a_malformed_frame_is_a_stream_error() {
        let mut decoder = Decoder::new();
        assert!(matches!(
            decoder.decode("message_start", "not json"),
            Err(ProviderError::Stream { .. })
        ));
        assert!(matches!(
            decoder.decode("content_block_start", r#"{"index":0}"#),
            Err(ProviderError::Stream { .. })
        ));
    }
}

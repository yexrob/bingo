//! The Responses event stream as a state machine: one wire `event`/`data`
//! pair in, the `ModelEvent`s it produces out.
//!
//! Pure — no I/O and no clock — so the `fixtures/*.sse` files drive it
//! directly. It holds only what an output item needs in order to close
//! itself: which sdk block a wire `output_index` belongs to, and the tool
//! arguments accumulated so far. Event names verified against the streaming
//! reference, 2026-08-29.

use std::collections::BTreeMap;

use bingo_sdk::{FinishReason, ModelEvent, ProviderError, ProviderMetadata, UnifiedFinish, Usage};
use serde_json::{Map, Value};

/// The provider id `provider_metadata` is keyed by (ADR-0002 §7).
pub const PROVIDER: &str = "openai";

/// A tool called with no arguments streams no delta at all. The loop parses
/// this text exactly once, and `{}` is what "no arguments" means.
const NO_ARGUMENTS: &str = "{}";

#[derive(Debug, Default)]
pub struct Decoder {
    open: BTreeMap<u64, Item>,
    /// Whether the response asked for a tool: Responses says `completed`
    /// either way, so the finish reason is read from what was emitted.
    called_a_tool: bool,
}

impl Decoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// The events this wire event produces. `Err` ends the stream: either a
    /// failure the server announced, or a frame this protocol cannot be.
    pub fn decode(&mut self, event: &str, data: &str) -> Result<Vec<ModelEvent>, ProviderError> {
        match event {
            "response.created" => created(&parse(data, event)?),
            "response.output_item.added" => self.item_added(&parse(data, event)?),
            "response.output_item.done" => self.item_done(&parse(data, event)?),
            "response.output_text.delta" => self.text_delta(&parse(data, event)?),
            // Two event names, one surface: the API streams a *summary* for
            // models that keep their reasoning hidden and the reasoning text
            // itself for models that do not. Reading only one name lost every
            // token of the other kind under a thinking block that had already
            // opened — the affordance was there, always empty (old
            // `providers/openai.rs:557-565`).
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                self.reasoning_delta(&parse(data, event)?)
            }
            "response.function_call_arguments.delta" => self.tool_delta(&parse(data, event)?),
            "response.completed" | "response.incomplete" => {
                Ok(vec![self.finish(event, &parse(data, event)?)])
            }
            "response.failed" => Err(failed(&parse(data, event)?)),
            "error" => Err(error_event(&parse(data, event)?)),
            // `response.in_progress`, the `.done` echoes, and whatever the
            // API adds after this was written.
            _ => Ok(Vec::new()),
        }
    }

    fn item_added(&mut self, value: &Value) -> Result<Vec<ModelEvent>, ProviderError> {
        let item = value
            .get("item")
            .ok_or_else(|| malformed("output_item.added without an item"))?;
        let Some(item) = Item::open(item)? else {
            return Ok(Vec::new());
        };
        self.called_a_tool |= matches!(item.kind, Kind::Tool { .. });
        let started = item.start();
        self.open.insert(output_index(value), item);
        Ok(vec![started])
    }

    fn item_done(&mut self, value: &Value) -> Result<Vec<ModelEvent>, ProviderError> {
        Ok(self
            .open
            .remove(&output_index(value))
            .map(|item| item.close(value.get("item")))
            .unwrap_or_default())
    }

    fn text_delta(&mut self, value: &Value) -> Result<Vec<ModelEvent>, ProviderError> {
        Ok(self
            .delta_target(value, |kind| matches!(kind, Kind::Text))
            .map(|(id, delta)| vec![ModelEvent::TextDelta { id, delta }])
            .unwrap_or_default())
    }

    fn reasoning_delta(&mut self, value: &Value) -> Result<Vec<ModelEvent>, ProviderError> {
        Ok(self
            .delta_target(value, |kind| matches!(kind, Kind::Reasoning))
            .map(|(id, delta)| vec![ModelEvent::ReasoningDelta { id, delta }])
            .unwrap_or_default())
    }

    fn tool_delta(&mut self, value: &Value) -> Result<Vec<ModelEvent>, ProviderError> {
        let index = output_index(value);
        let Some(fragment) = str_at(value, "delta") else {
            return Ok(Vec::new());
        };
        let Some(item) = self.open.get_mut(&index) else {
            return Ok(Vec::new());
        };
        let Kind::Tool { arguments, .. } = &mut item.kind else {
            return Ok(Vec::new());
        };
        arguments.push_str(fragment);
        Ok(vec![ModelEvent::ToolInputDelta {
            id: item.id.clone(),
            delta: fragment.to_string(),
        }])
    }

    /// The block a delta belongs to, when one is open and is the kind this
    /// event name implies. A delta for an item type this adapter never
    /// started is ignored, so the stream stays forward-compatible.
    fn delta_target(
        &self,
        value: &Value,
        wanted: impl Fn(&Kind) -> bool,
    ) -> Option<(String, String)> {
        let item = self.open.get(&output_index(value))?;
        if !wanted(&item.kind) {
            return None;
        }
        Some((item.id.clone(), str_at(value, "delta")?.to_string()))
    }

    fn finish(&self, event: &str, value: &Value) -> ModelEvent {
        let response = value.get("response").unwrap_or(&Value::Null);
        ModelEvent::Finish {
            usage: usage_of(response),
            finish_reason: finish_reason(event, response, self.called_a_tool),
        }
    }
}

/// One open output item. Its `id` is the sdk's block id: the wire item id for
/// a message or a reasoning block, the `call_id` for a function call, so the
/// loop can answer a call without a second table mapping index to id.
#[derive(Debug)]
struct Item {
    id: String,
    kind: Kind,
}

#[derive(Debug)]
enum Kind {
    Text,
    /// The encrypted state and its id arrive whole on `output_item.done`.
    Reasoning,
    /// `function_call_arguments.delta` fragments, joined when the item ends.
    Tool {
        name: String,
        arguments: String,
    },
}

impl Item {
    /// `None` for an item type this adapter does not model (a web search, a
    /// code interpreter call); it occupies no block and its deltas are
    /// ignored.
    fn open(item: &Value) -> Result<Option<Self>, ProviderError> {
        let kind = match str_at(item, "type").ok_or_else(|| malformed("an item without a type"))? {
            "message" => Kind::Text,
            "reasoning" => Kind::Reasoning,
            "function_call" => Kind::Tool {
                name: str_at(item, "name")
                    .ok_or_else(|| malformed("function_call without a name"))?
                    .to_string(),
                arguments: String::new(),
            },
            _ => return Ok(None),
        };
        Ok(Some(Self {
            id: id_of(item, &kind)?,
            kind,
        }))
    }

    fn start(&self) -> ModelEvent {
        let id = self.id.clone();
        match &self.kind {
            Kind::Text => ModelEvent::TextStart { id },
            Kind::Reasoning => ModelEvent::ReasoningStart { id },
            Kind::Tool { name, .. } => ModelEvent::ToolInputStart {
                id,
                name: name.clone(),
            },
        }
    }

    /// `done` carries the item whole, which is where the encrypted reasoning
    /// state and the authoritative tool arguments live.
    fn close(self, done: Option<&Value>) -> Vec<ModelEvent> {
        let id = self.id;
        match self.kind {
            Kind::Text => vec![ModelEvent::TextEnd { id }],
            Kind::Reasoning => vec![ModelEvent::ReasoningEnd {
                provider_metadata: reasoning_metadata(done),
                id,
            }],
            Kind::Tool { name, arguments } => close_tool(id, name, arguments, done),
        }
    }
}

/// A call ends with the arguments the item reports, not the fragments: an
/// endpoint that skips the deltas for `{}` still yields a parsable call, and
/// the surface is handed one backfilled delta so the input is not blank.
fn close_tool(id: String, name: String, streamed: String, done: Option<&Value>) -> Vec<ModelEvent> {
    let authoritative = done
        .and_then(|item| str_at(item, "arguments"))
        .unwrap_or_default();
    let input = [authoritative, streamed.as_str(), NO_ARGUMENTS]
        .into_iter()
        .find(|text| !text.trim().is_empty())
        .unwrap_or(NO_ARGUMENTS)
        .to_string();
    let mut events = Vec::new();
    if streamed.is_empty() && !authoritative.is_empty() {
        events.push(ModelEvent::ToolInputDelta {
            id: id.clone(),
            delta: authoritative.to_string(),
        });
    }
    events.push(ModelEvent::ToolInputEnd { id: id.clone() });
    events.push(ModelEvent::ToolCall { id, name, input });
    events
}

/// A function call is keyed by the `call_id` the result must quote; a message
/// or a reasoning block by its own item id.
fn id_of(item: &Value, kind: &Kind) -> Result<String, ProviderError> {
    let key = match kind {
        Kind::Tool { .. } => "call_id",
        _ => "id",
    };
    str_at(item, key)
        .ok_or_else(|| malformed(format!("an output item without a {key}")))
        .map(str::to_string)
}

/// What has to travel back for a reasoning item to be replayable when nothing
/// is stored server-side: the item id and the encrypted chain of thought.
/// Without both there is nothing to replay and the request encoder drops it.
fn reasoning_metadata(done: Option<&Value>) -> ProviderMetadata {
    let Some(item) = done else {
        return ProviderMetadata::new();
    };
    let (Some(id), Some(encrypted)) = (str_at(item, "id"), str_at(item, "encrypted_content"))
    else {
        return ProviderMetadata::new();
    };
    ProviderMetadata::from([(
        PROVIDER.to_string(),
        Map::from_iter([
            ("id".to_string(), Value::String(id.into())),
            (
                "encrypted_content".to_string(),
                Value::String(encrypted.into()),
            ),
        ]),
    )])
}

/// The Responses wire reports `input_tokens` inclusive of the cached prefix;
/// the sdk keeps the two apart so the ruler can tell a cache read from fresh
/// input, and the kernel sums them back.
fn usage_of(response: &Value) -> Usage {
    let usage = response.get("usage").unwrap_or(&Value::Null);
    let cached = detail(usage, "input_tokens_details", "cached_tokens");
    Usage {
        input_tokens: u64_at(usage, "input_tokens").saturating_sub(cached),
        output_tokens: u64_at(usage, "output_tokens"),
        cache_read_tokens: cached,
        // Responses caches automatically and never bills a write.
        cache_write_tokens: 0,
        reasoning_tokens: detail(usage, "output_tokens_details", "reasoning_tokens"),
    }
}

fn finish_reason(event: &str, response: &Value, called_a_tool: bool) -> FinishReason {
    if event == "response.incomplete" {
        return incomplete_reason(response);
    }
    FinishReason {
        unified: if called_a_tool {
            UnifiedFinish::ToolCalls
        } else {
            UnifiedFinish::Stop
        },
        raw: Some("completed".to_string()),
    }
}

fn incomplete_reason(response: &Value) -> FinishReason {
    let raw = response
        .pointer("/incomplete_details/reason")
        .and_then(Value::as_str)
        .unwrap_or("incomplete");
    FinishReason {
        unified: match raw {
            "max_output_tokens" => UnifiedFinish::Length,
            "content_filter" => UnifiedFinish::ContentFilter,
            _ => UnifiedFinish::Other,
        },
        raw: Some(raw.to_string()),
    }
}

fn created(value: &Value) -> Result<Vec<ModelEvent>, ProviderError> {
    let response = value.get("response").unwrap_or(&Value::Null);
    Ok(vec![
        ModelEvent::StreamStart {
            warnings: Vec::new(),
        },
        ModelEvent::ResponseMetadata {
            id: str_at(response, "id").map(str::to_string),
            model: str_at(response, "model").map(str::to_string),
        },
    ])
}

fn failed(value: &Value) -> ProviderError {
    let response = value.get("response").unwrap_or(value);
    announced(response, response.get("error").unwrap_or(&Value::Null))
}

fn error_event(value: &Value) -> ProviderError {
    announced(value, value.get("error").unwrap_or(value))
}

/// A failure the server announced inside the body, whether as its own event
/// or as the state of a failed response. The delay, when there is one, may sit
/// beside the error rather than inside it.
fn announced(envelope: &Value, error: &Value) -> ProviderError {
    crate::error::stream_error(
        crate::error::code_in(error),
        str_at(error, "message").unwrap_or("the response failed"),
        crate::error::body_delay_ms(envelope),
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

fn output_index(value: &Value) -> u64 {
    u64_at(value, "output_index")
}

/// A count the wire nests under a `*_tokens_details` object.
fn detail(usage: &Value, details: &str, key: &str) -> u64 {
    usage
        .get(details)
        .map(|inner| u64_at(inner, key))
        .unwrap_or(0)
}

fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn u64_at(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sse::SseParser;
    use serde_json::json;

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

    fn finish_of(fixture: &str) -> ModelEvent {
        match events(fixture).pop() {
            Some(finish @ ModelEvent::Finish { .. }) => finish,
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
                    id: Some("resp_01Text".into()),
                    model: Some("gpt-5.4".into()),
                },
                ModelEvent::TextStart {
                    id: "msg_01Text".into()
                },
                ModelEvent::TextDelta {
                    id: "msg_01Text".into(),
                    delta: "Hello".into()
                },
                ModelEvent::TextDelta {
                    id: "msg_01Text".into(),
                    delta: ", world.".into()
                },
                ModelEvent::TextEnd {
                    id: "msg_01Text".into()
                },
                ModelEvent::Finish {
                    usage: Usage {
                        input_tokens: 964,
                        output_tokens: 7,
                        cache_read_tokens: 2048,
                        cache_write_tokens: 0,
                        reasoning_tokens: 0,
                    },
                    finish_reason: FinishReason {
                        unified: UnifiedFinish::Stop,
                        raw: Some("completed".into()),
                    },
                },
            ],
            "response.in_progress contributes nothing"
        );
    }

    /// `input_tokens` on the wire includes the cached prefix; the sdk keeps
    /// the two apart, so the uncached remainder is what `input_tokens` means.
    #[test]
    fn the_cached_prefix_is_subtracted_out_of_the_input_count() {
        assert_eq!(
            finish_of("text.sse"),
            ModelEvent::Finish {
                usage: Usage {
                    input_tokens: 964,
                    output_tokens: 7,
                    cache_read_tokens: 2048,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                },
                finish_reason: FinishReason {
                    unified: UnifiedFinish::Stop,
                    raw: Some("completed".into()),
                },
            },
            "3012 reported - 2048 cached = 964 fresh"
        );
    }

    #[test]
    fn a_tool_item_is_keyed_by_its_call_id_and_ends_with_the_whole_call() {
        let events = events("tools.sse");
        assert_eq!(
            &events[events.len() - 6..],
            &[
                ModelEvent::ToolInputStart {
                    id: "call_01Read".into(),
                    name: "Read".into(),
                },
                ModelEvent::ToolInputDelta {
                    id: "call_01Read".into(),
                    delta: r#"{"file_path":"#.into(),
                },
                ModelEvent::ToolInputDelta {
                    id: "call_01Read".into(),
                    delta: r#""Cargo.toml"}"#.into(),
                },
                ModelEvent::ToolInputEnd {
                    id: "call_01Read".into()
                },
                ModelEvent::ToolCall {
                    id: "call_01Read".into(),
                    name: "Read".into(),
                    input: r#"{"file_path":"Cargo.toml"}"#.into(),
                },
                ModelEvent::Finish {
                    usage: Usage {
                        input_tokens: 400,
                        output_tokens: 58,
                        cache_read_tokens: 11_000,
                        cache_write_tokens: 0,
                        reasoning_tokens: 0,
                    },
                    finish_reason: FinishReason {
                        unified: UnifiedFinish::ToolCalls,
                        raw: Some("completed".into()),
                    },
                },
            ]
        );
    }

    /// The regression this adapter exists to not repeat: a model that streams
    /// its reasoning under `response.reasoning_text.delta` lost every token
    /// when only the summary name was read.
    #[test]
    fn both_reasoning_delta_names_reach_the_same_block() {
        let deltas: Vec<ModelEvent> = events("reasoning.sse")
            .into_iter()
            .filter(|e| matches!(e, ModelEvent::ReasoningDelta { .. }))
            .collect();
        assert_eq!(
            deltas,
            vec![
                ModelEvent::ReasoningDelta {
                    id: "rs_01".into(),
                    delta: "Summarising: ".into(),
                },
                ModelEvent::ReasoningDelta {
                    id: "rs_01".into(),
                    delta: "weigh the options.".into(),
                },
                ModelEvent::ReasoningDelta {
                    id: "rs_02".into(),
                    delta: "The raw chain of thought.".into(),
                },
            ]
        );
    }

    #[test]
    fn the_encrypted_state_rides_home_on_the_reasoning_end() {
        let ends: Vec<ModelEvent> = events("reasoning.sse")
            .into_iter()
            .filter(|e| matches!(e, ModelEvent::ReasoningEnd { .. }))
            .collect();
        assert_eq!(
            ends,
            vec![
                ModelEvent::ReasoningEnd {
                    id: "rs_01".into(),
                    provider_metadata: metadata("rs_01", "gAAAAABsummary"),
                },
                ModelEvent::ReasoningEnd {
                    id: "rs_02".into(),
                    provider_metadata: metadata("rs_02", "gAAAAABraw"),
                },
            ]
        );
    }

    fn metadata(id: &str, encrypted: &str) -> ProviderMetadata {
        ProviderMetadata::from([(
            PROVIDER.to_string(),
            Map::from_iter([
                ("id".to_string(), json!(id)),
                ("encrypted_content".to_string(), json!(encrypted)),
            ]),
        )])
    }

    #[test]
    fn a_reasoning_turn_reports_the_tokens_it_spent_thinking() {
        assert_eq!(
            finish_of("reasoning.sse"),
            ModelEvent::Finish {
                usage: Usage {
                    input_tokens: 120,
                    output_tokens: 900,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 768,
                },
                finish_reason: FinishReason {
                    unified: UnifiedFinish::Stop,
                    raw: Some("completed".into()),
                },
            }
        );
    }

    #[test]
    fn an_incomplete_response_finishes_as_length() {
        assert_eq!(
            finish_of("incomplete.sse"),
            ModelEvent::Finish {
                usage: Usage {
                    input_tokens: 30,
                    output_tokens: 1024,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    reasoning_tokens: 0,
                },
                finish_reason: FinishReason {
                    unified: UnifiedFinish::Length,
                    raw: Some("max_output_tokens".into()),
                },
            }
        );
    }

    #[test]
    fn a_failed_response_ends_the_stream_and_keeps_what_came_before() {
        let (events, error) = replay("failed.sse");
        assert_eq!(
            error,
            Some(ProviderError::Server {
                status: 500,
                message: "server_error: The server had an error while processing your request."
                    .into(),
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
    fn an_error_event_ends_the_stream_too() {
        let mut decoder = Decoder::new();
        assert_eq!(
            decoder.decode(
                "error",
                r#"{"type":"error","code":"rate_limit_exceeded","message":"slow down","retry_after_ms":750}"#
            ),
            Err(ProviderError::RateLimited {
                retry_after_ms: Some(750)
            })
        );
    }

    #[test]
    fn every_incomplete_reason_maps_and_an_unknown_one_is_kept_raw() {
        for (raw, unified) in [
            ("max_output_tokens", UnifiedFinish::Length),
            ("content_filter", UnifiedFinish::ContentFilter),
            ("something_new", UnifiedFinish::Other),
        ] {
            let response = json!({ "incomplete_details": { "reason": raw } });
            assert_eq!(
                incomplete_reason(&response),
                FinishReason {
                    unified,
                    raw: Some(raw.into())
                }
            );
        }
        assert_eq!(
            incomplete_reason(&json!({})).raw.as_deref(),
            Some("incomplete")
        );
    }

    #[test]
    fn a_tool_called_with_no_arguments_still_carries_parsable_json() {
        let mut decoder = Decoder::new();
        decoder
            .decode(
                "response.output_item.added",
                r#"{"output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"Now","arguments":""}}"#,
            )
            .expect("a start");
        let closed = decoder
            .decode(
                "response.output_item.done",
                r#"{"output_index":0,"item":{"type":"function_call","id":"fc_1","call_id":"call_1","name":"Now","arguments":""}}"#,
            )
            .expect("a done");
        assert_eq!(
            closed,
            vec![
                ModelEvent::ToolInputEnd {
                    id: "call_1".into()
                },
                ModelEvent::ToolCall {
                    id: "call_1".into(),
                    name: "Now".into(),
                    input: NO_ARGUMENTS.into(),
                },
            ]
        );
    }

    /// Some endpoints skip the argument deltas entirely; the surface still
    /// has to be able to show what the model asked for.
    #[test]
    fn arguments_that_never_streamed_are_backfilled_as_one_delta() {
        let mut decoder = Decoder::new();
        decoder
            .decode(
                "response.output_item.added",
                r#"{"output_index":0,"item":{"type":"function_call","call_id":"call_2","name":"Read"}}"#,
            )
            .expect("a start");
        let closed = decoder
            .decode(
                "response.output_item.done",
                r#"{"output_index":0,"item":{"type":"function_call","call_id":"call_2","name":"Read","arguments":"{\"file_path\":\"a\"}"}}"#,
            )
            .expect("a done");
        assert_eq!(
            closed[0],
            ModelEvent::ToolInputDelta {
                id: "call_2".into(),
                delta: r#"{"file_path":"a"}"#.into(),
            }
        );
        assert_eq!(closed.len(), 3);
    }

    #[test]
    fn an_unknown_item_type_and_an_unknown_event_are_ignored() {
        let mut decoder = Decoder::new();
        assert!(
            decoder
                .decode(
                    "response.output_item.added",
                    r#"{"output_index":0,"item":{"type":"web_search_call","id":"ws_1"}}"#
                )
                .expect("ignored")
                .is_empty()
        );
        assert!(
            decoder
                .decode(
                    "response.output_text.delta",
                    r#"{"output_index":0,"delta":"x"}"#
                )
                .expect("ignored")
                .is_empty(),
            "a delta for an item we never opened has no block"
        );
        assert!(
            decoder
                .decode("response.something_new", "{}")
                .expect("ignored")
                .is_empty()
        );
    }

    #[test]
    fn a_malformed_frame_is_a_stream_error() {
        let mut decoder = Decoder::new();
        assert!(matches!(
            decoder.decode("response.created", "not json"),
            Err(ProviderError::Stream { .. })
        ));
        assert!(matches!(
            decoder.decode("response.output_item.added", r#"{"output_index":0}"#),
            Err(ProviderError::Stream { .. })
        ));
        assert!(matches!(
            decoder.decode(
                "response.output_item.added",
                r#"{"output_index":0,"item":{"type":"function_call","call_id":"c"}}"#
            ),
            Err(ProviderError::Stream { .. })
        ));
    }
}

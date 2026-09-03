//! The two shapes, one on each side of the bridge.
//!
//! Pure, like [`crate::method`] is pure: what a tool is on bingo's side is
//! `bingo_sdk`'s, what it is on the agent's side is MCP's, and the translation
//! between them is a function with a fixture rather than a branch inside a
//! server loop.

use bingo_sdk::{ContentPart, Image, ToolCall, ToolOutput, ToolSpec};
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, JsonObject, Tool};
use serde_json::Value;
use std::sync::Arc;

use super::doors::Refused;

/// A tool as the agent's client sees it.
///
/// `meta` does not cross: it is what a catalogue shows beside a tool and is
/// never sent to a model (`bingo_sdk::ToolSpec`), and the agent on the far
/// side of this bridge is one.
pub fn offered(spec: &ToolSpec) -> Tool {
    Tool::new(
        spec.name.clone(),
        spec.description.clone(),
        Arc::new(object(&spec.input_schema)),
    )
}

/// The call the agent asked for, under the id the bridge minted for it. MCP
/// names its request by a JSON-RPC id the handler never sees, so the id a
/// bingo tool call is journaled under is ours to mint.
pub fn asked(request: CallToolRequestParams, call_id: impl Into<String>) -> ToolCall {
    ToolCall {
        call_id: call_id.into(),
        name: request.name.to_string(),
        input: Value::Object(request.arguments.unwrap_or_default()),
    }
}

/// What the tool answered. A tool's own `isError` crosses as MCP's, because
/// on both sides it means the same thing: the call ran and went wrong.
pub fn answered(output: ToolOutput) -> CallToolResult {
    let content = output.parts.iter().map(block).collect();
    match output.is_error {
        true => CallToolResult::error(content),
        false => CallToolResult::success(content),
    }
}

/// A call that never ran. An error *result*, not a protocol error: the agent
/// asked something well-formed and is owed an answer it can read and go on
/// from, not a transport fault (ADR-0036 §2).
pub fn refused(why: &Refused) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(why.to_string())])
}

/// One part of an answer. Text and images are what a tool returns; anything
/// else is carried as its own JSON rather than dropped, so nothing a tool
/// said is lost on the way out.
fn block(part: &ContentPart) -> ContentBlock {
    match part {
        ContentPart::Text { text } => ContentBlock::text(text.clone()),
        ContentPart::Reasoning { text, .. } => ContentBlock::text(text.clone()),
        ContentPart::Image(Image { media_type, data }) => {
            ContentBlock::image(data.clone(), media_type)
        }
        other => ContentBlock::text(
            serde_json::to_string(other).unwrap_or_else(|_| "[unreadable content]".to_string()),
        ),
    }
}

/// A schema MCP will take. A tool's input schema is an object; one that is
/// not becomes the empty object schema rather than a malformed offer, because
/// a client that cannot parse the schema drops the tool entirely.
fn object(schema: &Value) -> JsonObject {
    match schema {
        Value::Object(map) => map.clone(),
        _ => {
            let mut empty = JsonObject::new();
            empty.insert("type".to_string(), Value::String("object".into()));
            empty
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An invented tool: the translation is by shape, never by name, and a
    /// fixture built from a real bingo tool would not show that.
    fn spec() -> ToolSpec {
        ToolSpec {
            name: "Shout".into(),
            description: "Say something out loud.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
            meta: serde_json::Map::from_iter([(
                "source".to_string(),
                json!("a-catalogue-only-fact"),
            )]),
        }
    }

    fn wire<T: serde::Serialize>(value: &T) -> Value {
        serde_json::to_value(value).expect("it writes")
    }

    /// The recorded body a client parses: name, description and the schema
    /// verbatim. A change to any of the three is a change to what the agent
    /// is offered.
    #[test]
    fn a_tool_crosses_as_the_body_a_client_parses() {
        assert_eq!(
            wire(&offered(&spec())),
            json!({
                "name": "Shout",
                "description": "Say something out loud.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                }
            })
        );
    }

    /// `meta` is a catalogue's, not a model's — the far side of this bridge
    /// is a model.
    #[test]
    fn what_a_catalogue_shows_beside_a_tool_does_not_cross() {
        let written = wire(&offered(&spec())).to_string();
        assert!(!written.contains("a-catalogue-only-fact"), "{written}");
        assert!(!written.contains("_meta"), "{written}");
    }

    #[test]
    fn a_schema_that_is_not_an_object_is_offered_as_the_empty_object() {
        let mut odd = spec();
        odd.input_schema = json!("nonsense");
        assert_eq!(
            wire(&offered(&odd))["inputSchema"],
            json!({"type":"object"})
        );
    }

    #[test]
    fn a_call_carries_its_arguments_under_the_id_the_bridge_minted() {
        let request: CallToolRequestParams =
            serde_json::from_value(json!({ "name": "Shout", "arguments": { "text": "hi" } }))
                .expect("a recorded call");
        assert_eq!(
            asked(request, "acp_1_1"),
            ToolCall {
                call_id: "acp_1_1".into(),
                name: "Shout".into(),
                input: json!({ "text": "hi" }),
            }
        );
    }

    /// A client may omit `arguments` for a tool that takes none; a tool is
    /// still handed an object, because that is what its schema promises.
    #[test]
    fn a_call_with_no_arguments_is_an_empty_object_not_null() {
        let request: CallToolRequestParams =
            serde_json::from_value(json!({ "name": "Whisper" })).expect("a recorded call");
        assert_eq!(asked(request, "acp_1_2").input, json!({}));
    }

    #[test]
    fn an_answer_crosses_as_content_and_a_failure_says_so() {
        assert_eq!(
            wire(&answered(ToolOutput::text("posted"))),
            json!({
                "resultType": "complete",
                "content": [{ "type": "text", "text": "posted" }],
                "isError": false
            })
        );
        assert_eq!(
            wire(&answered(ToolOutput::error("no such room"))),
            json!({
                "resultType": "complete",
                "content": [{ "type": "text", "text": "no such room" }],
                "isError": true
            })
        );
    }

    /// What a *person* would have seen instead of the text is not what the
    /// agent gets: `display` is a view, and there is nobody at this end.
    #[test]
    fn the_view_a_person_would_have_seen_does_not_cross() {
        let output = ToolOutput {
            parts: vec![ContentPart::text("done")],
            is_error: false,
            display: Some(bingo_sdk::View::Text {
                text: "a pretty card".into(),
            }),
        };
        let written = wire(&answered(output)).to_string();
        assert!(!written.contains("pretty card"), "{written}");
    }

    #[test]
    fn an_image_keeps_its_media_type() {
        let output = ToolOutput {
            parts: vec![ContentPart::Image(Image {
                media_type: "image/png".into(),
                data: "AAAA".into(),
            })],
            is_error: false,
            display: None,
        };
        assert_eq!(
            wire(&answered(output))["content"],
            json!([{ "type": "image", "data": "AAAA", "mimeType": "image/png" }])
        );
    }

    /// A refusal is an answer, not a fault: `isError`, in words the agent can
    /// act on (ADR-0036 §2).
    #[test]
    fn a_refusal_is_an_error_result_in_words() {
        let written = wire(&refused(&Refused::new("no turn is in flight")));
        assert_eq!(written["isError"], json!(true));
        assert_eq!(
            written["content"],
            json!([{ "type": "text", "text": "no turn is in flight" }])
        );
    }
}

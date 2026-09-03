//! `Message`s → the Responses `input` item list.
//!
//! Where the Messages API nests blocks inside a role, Responses flattens
//! everything into one ordered item list: a tool result is its own item, not
//! a block inside the user's turn. Shapes verified against the API reference
//! and the reasoning cookbook, 2026-08-29 — `message`/`input_text`/
//! `input_image.image_url`, `function_call{call_id,name,arguments}`,
//! `function_call_output{call_id,output}`, `reasoning{id,summary,
//! encrypted_content}`.

use bingo_sdk::{ContentPart, Image, Message, ProviderMetadata, Role};
use serde_json::{Value, json};

use crate::events::PROVIDER;

/// What a tool result whose only payload is an image says on the wire; the
/// image itself follows as an `input_image` in the next user message.
const IMAGE_PLACEHOLDER: &str = "[image]";

pub fn items(messages: &[Message]) -> Vec<Value> {
    let mut input = Input::default();
    for message in messages {
        input.message(message);
    }
    input.items
}

#[derive(Default)]
struct Input {
    items: Vec<Value>,
}

impl Input {
    fn message(&mut self, message: &Message) {
        match message.role {
            Role::User => self.user(&message.parts),
            Role::Assistant => self.assistant(&message.parts),
        }
    }

    /// Tool results are items of their own and keep the order they were
    /// folded in; whatever the user said — and any image a tool returned —
    /// follows as the one `message` item that closes the turn.
    fn user(&mut self, parts: &[ContentPart]) {
        let mut content = Vec::new();
        for part in parts {
            match part {
                ContentPart::Text { text } => content.push(input_text(text)),
                ContentPart::Image(Image { media_type, data }) => {
                    content.push(input_image(media_type, data));
                }
                ContentPart::ToolResult {
                    tool_use_id,
                    parts,
                    is_error,
                } => {
                    self.items.push(call_output(tool_use_id, parts, *is_error));
                    content.extend(images(parts));
                }
                // An assistant-only part has no place in a user turn.
                ContentPart::ToolUse { .. } | ContentPart::Reasoning { .. } => {}
            }
        }
        if !content.is_empty() {
            self.items.push(user_message(content));
        }
    }

    fn assistant(&mut self, parts: &[ContentPart]) {
        self.items.extend(parts.iter().filter_map(assistant_item));
    }
}

fn user_message(content: Vec<Value>) -> Value {
    json!({ "type": "message", "role": "user", "content": content })
}

fn input_text(text: &str) -> Value {
    json!({ "type": "input_text", "text": text })
}

/// Responses takes an image as a URL; a base64 part travels as a data URL.
fn input_image(media_type: &str, data: &str) -> Value {
    json!({
        "type": "input_image",
        "image_url": format!("data:{media_type};base64,{data}"),
    })
}

/// `None` for a part with no assistant-side wire form: an image the model did
/// not produce, a tool result that belongs to the user turn, reasoning the
/// endpoint never encrypted for us.
fn assistant_item(part: &ContentPart) -> Option<Value> {
    match part {
        ContentPart::Text { text } if !text.is_empty() => Some(json!({
            "type": "message",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": text }],
        })),
        ContentPart::ToolUse { id, name, input } => Some(json!({
            "type": "function_call",
            "call_id": id,
            "name": name,
            "arguments": input.to_string(),
        })),
        ContentPart::Reasoning {
            provider_metadata, ..
        } => reasoning(provider_metadata),
        _ => None,
    }
}

/// A reasoning item replays as the encrypted state the endpoint handed back,
/// never as its summary: the summary is a display artifact, the encrypted
/// content is the chain of thought. Without both halves there is nothing to
/// replay, and a `reasoning` item the server cannot decrypt is a 400.
fn reasoning(metadata: &ProviderMetadata) -> Option<Value> {
    let mine = metadata.get(PROVIDER)?;
    let id = mine.get("id").and_then(Value::as_str)?;
    let encrypted = mine.get("encrypted_content").and_then(Value::as_str)?;
    Some(json!({
        "type": "reasoning",
        "id": id,
        "summary": [],
        "encrypted_content": encrypted,
    }))
}

fn call_output(id: &str, parts: &[ContentPart], is_error: bool) -> Value {
    json!({
        "type": "function_call_output",
        "call_id": id,
        "output": output(parts, is_error),
    })
}

/// Responses carries a tool result as one string and has no `is_error` flag,
/// so a failure is encoded in the string the model reads (old
/// `providers/openai.rs:359-367`).
fn output(parts: &[ContentPart], is_error: bool) -> String {
    let text = text_of(parts);
    if is_error {
        return json!({ "is_error": true, "content": text }).to_string();
    }
    text
}

fn text_of(parts: &[ContentPart]) -> String {
    let text = parts
        .iter()
        .filter_map(ContentPart::as_text)
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() && parts.iter().any(is_image) {
        return IMAGE_PLACEHOLDER.to_string();
    }
    text
}

/// A tool that returned an image: the item list carries it in the next user
/// message, because `function_call_output.output` is a string here.
fn images(parts: &[ContentPart]) -> Vec<Value> {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Image(Image { media_type, data }) => Some(input_image(media_type, data)),
            _ => None,
        })
        .collect()
}

fn is_image(part: &ContentPart) -> bool {
    matches!(part, ContentPart::Image(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn meta(pairs: &[(&str, &str)]) -> ProviderMetadata {
        ProviderMetadata::from([(
            PROVIDER.to_string(),
            Map::from_iter(
                pairs
                    .iter()
                    .map(|(k, v)| (k.to_string(), Value::String((*v).into()))),
            ),
        )])
    }

    fn kinds(items: &[Value]) -> Vec<&str> {
        items
            .iter()
            .filter_map(|item| item.get("type").and_then(Value::as_str))
            .collect()
    }

    #[test]
    fn a_tool_result_is_its_own_item_and_precedes_what_the_user_said() {
        let items = items(&[Message::user(vec![
            ContentPart::ToolResult {
                tool_use_id: "call_1".into(),
                parts: vec![ContentPart::text("[package]")],
                is_error: false,
            },
            ContentPart::text("and now the lock file"),
        ])]);
        assert_eq!(kinds(&items), ["function_call_output", "message"]);
        assert_eq!(items[0]["call_id"], "call_1");
        assert_eq!(items[0]["output"], "[package]");
    }

    #[test]
    fn a_failed_tool_keeps_the_flag_the_wire_has_no_field_for() {
        let items = items(&[Message::user(vec![ContentPart::ToolResult {
            tool_use_id: "call_2".into(),
            parts: vec![ContentPart::text("no such file")],
            is_error: true,
        }])]);
        assert_eq!(
            items[0]["output"],
            json!(r#"{"is_error":true,"content":"no such file"}"#)
        );
    }

    #[test]
    fn an_image_a_tool_returned_follows_as_an_input_image() {
        let items = items(&[Message::user(vec![ContentPart::ToolResult {
            tool_use_id: "call_3".into(),
            parts: vec![ContentPart::Image(Image {
                media_type: "image/png".into(),
                data: "iVBORw0KGgo=".into(),
            })],
            is_error: false,
        }])]);
        assert_eq!(kinds(&items), ["function_call_output", "message"]);
        assert_eq!(items[0]["output"], IMAGE_PLACEHOLDER);
        assert_eq!(
            items[1]["content"][0],
            json!({
                "type": "input_image",
                "image_url": "data:image/png;base64,iVBORw0KGgo=",
            })
        );
    }

    #[test]
    fn reasoning_replays_only_when_both_halves_came_back() {
        let items = items(&[Message::assistant(vec![
            ContentPart::Reasoning {
                text: "weigh it".into(),
                provider_metadata: meta(&[("id", "rs_1"), ("encrypted_content", "gAAAAAB")]),
            },
            ContentPart::Reasoning {
                text: "no id".into(),
                provider_metadata: meta(&[("encrypted_content", "gAAAAAB")]),
            },
            ContentPart::Reasoning {
                text: "no encrypted content".into(),
                provider_metadata: meta(&[("id", "rs_3")]),
            },
            ContentPart::Reasoning {
                text: "another provider's".into(),
                provider_metadata: ProviderMetadata::from([(
                    "anthropic".to_string(),
                    Map::from_iter([("signature".to_string(), json!("sig"))]),
                )]),
            },
        ])]);
        assert_eq!(kinds(&items), ["reasoning"]);
        assert_eq!(
            items[0],
            json!({
                "type": "reasoning",
                "id": "rs_1",
                "summary": [],
                "encrypted_content": "gAAAAAB",
            })
        );
    }

    #[test]
    fn an_empty_user_turn_produces_no_item() {
        assert!(items(&[Message::user(Vec::new())]).is_empty());
        assert!(items(&[Message::assistant(vec![ContentPart::text("")])]).is_empty());
    }

    #[test]
    fn a_call_carries_its_arguments_as_a_json_string() {
        let items = items(&[Message::assistant(vec![ContentPart::ToolUse {
            id: "call_4".into(),
            name: "Read".into(),
            input: json!({ "file_path": "Cargo.toml" }),
        }])]);
        assert_eq!(
            items[0]["arguments"],
            json!(r#"{"file_path":"Cargo.toml"}"#)
        );
        assert_eq!(items[0]["call_id"], "call_4");
    }
}

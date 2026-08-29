//! `ModelRequest` → the Messages API wire body.
//!
//! Pure: the endpoint and the request decide everything, so a snapshot pins
//! every shape the API is picky about — the thinking parameters, the cache
//! breakpoints, and the blocks a reasoning turn has to replay.

use bingo_sdk::{
    ContentPart, Effort, EndpointCapabilities, Message, ModelRequest, ProviderMetadata, Role,
    SystemBlock, ToolSpec,
};
use serde_json::{Map, Value, json};

use crate::events::PROVIDER;

/// The Messages API rejects a request carrying more than four `cache_control`
/// breakpoints. One breakpoint caches the whole prefix up to it, so the budget
/// buys the tools-and-system prefix once and rolls the rest over the newest
/// messages, where the next turn's prefix will end.
const MAX_BREAKPOINTS: usize = 4;

/// Block types a breakpoint may sit on. A thinking block may not carry one.
const CACHEABLE: &[&str] = &["text", "image", "tool_use", "tool_result"];

/// The `POST /v1/messages` body.
pub fn encode(request: &ModelRequest, caps: &EndpointCapabilities) -> Value {
    let parts = Parts::of(request, caps);
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("max_tokens".into(), json!(request.max_tokens));
    parts.install(&mut body);
    body.insert("stream".into(), json!(true));
    if let Some(effort) = request.reasoning {
        add_thinking(&mut body, effort);
    }
    merge_provider_options(&mut body, request.provider_options.get(PROVIDER));
    Value::Object(body)
}

/// The `POST /v1/messages/count_tokens` body: the conversation the turn will
/// send, without the generation parameters. A count that skipped the tool
/// schemas read far under the real input and let the window overrun before
/// the compactor ever fired.
pub fn count_tokens(request: &ModelRequest, caps: &EndpointCapabilities) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    Parts::of(request, caps).install(&mut body);
    Value::Object(body)
}

/// The three arrays both endpoints share, with the breakpoints already placed:
/// one budget spans the system prefix and the messages together.
struct Parts {
    system: Vec<Value>,
    messages: Vec<Value>,
    tools: Vec<Value>,
}

impl Parts {
    fn of(request: &ModelRequest, caps: &EndpointCapabilities) -> Self {
        let mut budget = if caps.caching { MAX_BREAKPOINTS } else { 0 };
        Self {
            system: system_blocks(&request.system, &mut budget),
            messages: messages(&request.messages, &mut budget),
            tools: request.tools.iter().map(tool).collect(),
        }
    }

    /// Empty `system` and `tools` stay off the wire: some Anthropic-shaped
    /// endpoints reject the fields rather than ignore them.
    fn install(self, body: &mut Map<String, Value>) {
        if !self.system.is_empty() {
            body.insert("system".into(), self.system.into());
        }
        body.insert("messages".into(), self.messages.into());
        if !self.tools.is_empty() {
            body.insert("tools".into(), self.tools.into());
        }
    }
}

/// One breakpoint on the last block that asked for caching: it caches the
/// whole prefix — tools and system — up to itself.
fn system_blocks(blocks: &[SystemBlock], budget: &mut usize) -> Vec<Value> {
    let marked = blocks.iter().rposition(|b| b.cache).filter(|_| *budget > 0);
    if marked.is_some() {
        *budget -= 1;
    }
    blocks
        .iter()
        .enumerate()
        .map(|(index, block)| {
            let mut wire = json!({ "type": "text", "text": block.text });
            if Some(index) == marked {
                mark(&mut wire);
            }
            wire
        })
        .collect()
}

/// The rest of the budget rolls over the newest messages: each breakpoint is
/// where a later turn's prefix ends, so the turn after this one reads a cache
/// hit instead of paying for the conversation again.
fn messages(messages: &[Message], budget: &mut usize) -> Vec<Value> {
    let mut out: Vec<Value> = messages.iter().filter_map(message).collect();
    for message in out.iter_mut().rev() {
        if *budget == 0 {
            break;
        }
        if mark_last_cacheable(message) {
            *budget -= 1;
        }
    }
    out
}

/// True when a breakpoint was placed. A message of nothing but thinking blocks
/// has nowhere to put one and spends none of the budget.
fn mark_last_cacheable(message: &mut Value) -> bool {
    let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
        return false;
    };
    match content.iter_mut().rev().find(|b| is_cacheable(b)) {
        Some(block) => mark(block),
        None => false,
    }
}

fn is_cacheable(block: &Value) -> bool {
    block
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| CACHEABLE.contains(&kind))
}

/// True when the block took the breakpoint.
fn mark(block: &mut Value) -> bool {
    let Some(map) = block.as_object_mut() else {
        return false;
    };
    map.insert("cache_control".into(), json!({ "type": "ephemeral" }));
    true
}

/// The Claude 5 family rejects `{"type":"enabled","budget_tokens":N}` with a
/// 400, so every enabled level sends the same adaptive shape and the depth
/// travels in `output_config` instead (old `providers/anthropic.rs:189-204`,
/// pinned there by the tests at `:889-930`).
fn add_thinking(body: &mut Map<String, Value>, effort: Effort) {
    body.insert("thinking".into(), json!({ "type": "adaptive" }));
    body.insert(
        "output_config".into(),
        json!({ "effort": effort_name(effort) }),
    );
}

/// Anthropic's effort levels are `low|medium|high|xhigh|max` (old
/// `contract.rs:143-181`). `minimal` has no Anthropic level, so it takes the
/// shallowest one that exists rather than a name the API would 400 on.
fn effort_name(effort: Effort) -> &'static str {
    match effort {
        Effort::Minimal | Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
        Effort::XHigh => "xhigh",
        Effort::Max => "max",
    }
}

/// `provider_options["anthropic"]` goes on the body as it came, so a caller
/// can reach a parameter this adapter was written before.
fn merge_provider_options(body: &mut Map<String, Value>, extra: Option<&Map<String, Value>>) {
    let Some(extra) = extra else { return };
    for (key, value) in extra {
        body.insert(key.clone(), value.clone());
    }
}

/// `None` for a message nothing survives the encoding of: an assistant turn of
/// unsigned reasoning alone has no wire form, and empty content is a 400.
fn message(message: &Message) -> Option<Value> {
    let content: Vec<Value> = message.parts.iter().filter_map(part).collect();
    if content.is_empty() {
        return None;
    }
    Some(json!({ "role": role(message.role), "content": content }))
}

fn role(role: Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
    }
}

fn part(part: &ContentPart) -> Option<Value> {
    Some(match part {
        ContentPart::Text { text } => json!({ "type": "text", "text": text }),
        ContentPart::Image { media_type, data } => json!({
            "type": "image",
            "source": { "type": "base64", "media_type": media_type, "data": data },
        }),
        ContentPart::ToolUse { id, name, input } => json!({
            "type": "tool_use",
            "id": id,
            "name": name,
            "input": input,
        }),
        ContentPart::ToolResult {
            tool_use_id,
            parts,
            is_error,
        } => tool_result(tool_use_id, parts, *is_error),
        ContentPart::Reasoning {
            text,
            provider_metadata,
        } => thinking(text, provider_metadata)?,
    })
}

fn tool_result(id: &str, parts: &[ContentPart], is_error: bool) -> Value {
    let content: Vec<Value> = parts.iter().filter_map(part).collect();
    // The API takes a string or a block array here; an empty array is a 400.
    let content = if content.is_empty() {
        json!("")
    } else {
        json!(content)
    };
    let mut block = json!({ "type": "tool_result", "tool_use_id": id, "content": content });
    if is_error && let Some(map) = block.as_object_mut() {
        map.insert("is_error".into(), json!(true));
    }
    block
}

/// A reasoning block only goes back on the wire with the signature the model
/// signed it with; an unsigned one is dropped, because the API rejects it.
fn thinking(text: &str, metadata: &ProviderMetadata) -> Option<Value> {
    let mine = metadata.get(PROVIDER)?;
    if let Some(data) = mine.get("redacted").and_then(Value::as_str) {
        return Some(json!({ "type": "redacted_thinking", "data": data }));
    }
    let signature = mine.get("signature").and_then(Value::as_str)?;
    Some(json!({ "type": "thinking", "thinking": text, "signature": signature }))
}

fn tool(spec: &ToolSpec) -> Value {
    json!({
        "name": spec.name,
        "description": spec.description,
        "input_schema": spec.input_schema,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(caching: bool) -> EndpointCapabilities {
        EndpointCapabilities {
            images: true,
            count_tokens: true,
            caching,
        }
    }

    fn request(messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: "claude-sonnet-4-5-20250929".into(),
            max_tokens: 4096,
            system: Vec::new(),
            messages,
            tools: Vec::new(),
            reasoning: None,
            provider_options: ProviderMetadata::new(),
        }
    }

    fn meta(key: &str, value: &str) -> ProviderMetadata {
        ProviderMetadata::from([(
            PROVIDER.to_string(),
            Map::from_iter([(key.to_string(), Value::String(value.into()))]),
        )])
    }

    fn breakpoints(body: &Value) -> usize {
        body.to_string().matches("cache_control").count()
    }

    fn read_tool() -> ToolSpec {
        ToolSpec {
            name: "Read".into(),
            description: "Read a file from the filesystem.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "file_path": { "type": "string" } },
                "required": ["file_path"],
            }),
        }
    }

    #[test]
    fn a_text_turn_encodes_to_a_streaming_messages_body() {
        let mut request = request(vec![Message::text(Role::User, "hello")]);
        request.system = vec![SystemBlock {
            text: "You are bingo.".into(),
            cache: false,
        }];
        insta::assert_json_snapshot!(encode(&request, &caps(false)));
    }

    #[test]
    fn a_tool_round_encodes_the_schema_the_call_and_its_result() {
        let mut request = request(vec![
            Message::text(Role::User, "read Cargo.toml"),
            Message::assistant(vec![ContentPart::ToolUse {
                id: "toolu_01Read".into(),
                name: "Read".into(),
                input: json!({ "file_path": "Cargo.toml" }),
            }]),
            Message::user(vec![ContentPart::ToolResult {
                tool_use_id: "toolu_01Read".into(),
                parts: vec![ContentPart::text("[package]")],
                is_error: false,
            }]),
            Message::user(vec![ContentPart::ToolResult {
                tool_use_id: "toolu_01Miss".into(),
                parts: vec![ContentPart::text("no such file")],
                is_error: true,
            }]),
        ]);
        request.tools = vec![read_tool()];
        insta::assert_json_snapshot!(encode(&request, &caps(false)));
    }

    #[test]
    fn reasoning_sends_the_adaptive_shape_and_replays_only_signed_blocks() {
        let mut request = request(vec![
            Message::text(Role::User, "why?"),
            Message::assistant(vec![
                ContentPart::Reasoning {
                    text: "weigh the options".into(),
                    provider_metadata: meta("signature", "ErUBCkYIBBgCIkA="),
                },
                ContentPart::Reasoning {
                    text: String::new(),
                    provider_metadata: meta("redacted", "EroBCkYIBBgCKkBRedacted=="),
                },
                ContentPart::Reasoning {
                    text: "this one was never signed".into(),
                    provider_metadata: ProviderMetadata::new(),
                },
                ContentPart::text("Because it is simpler."),
            ]),
        ]);
        request.reasoning = Some(Effort::XHigh);
        let body = encode(&request, &caps(false));
        insta::assert_json_snapshot!(body);
        assert!(
            !body.to_string().contains("never signed"),
            "an unsigned reasoning block has no wire form"
        );
    }

    #[test]
    fn every_effort_level_maps_to_a_level_the_api_accepts() {
        for (effort, expected) in [
            (Effort::Minimal, "low"),
            (Effort::Low, "low"),
            (Effort::Medium, "medium"),
            (Effort::High, "high"),
            (Effort::XHigh, "xhigh"),
            (Effort::Max, "max"),
        ] {
            let mut request = request(vec![Message::text(Role::User, "hi")]);
            request.reasoning = Some(effort);
            let body = encode(&request, &caps(false));
            assert_eq!(
                body["thinking"],
                json!({ "type": "adaptive" }),
                "{effort:?}"
            );
            assert_eq!(body["output_config"], json!({ "effort": expected }));
            assert!(
                body["thinking"].get("budget_tokens").is_none(),
                "{effort:?} must not carry a budget"
            );
        }
    }

    #[test]
    fn no_reasoning_sends_neither_parameter() {
        let body = encode(
            &request(vec![Message::text(Role::User, "hi")]),
            &caps(false),
        );
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn an_image_becomes_a_base64_source_block() {
        let request = request(vec![Message::user(vec![
            ContentPart::text("what is this?"),
            ContentPart::Image {
                media_type: "image/png".into(),
                data: "iVBORw0KGgo=".into(),
            },
        ])]);
        insta::assert_json_snapshot!(encode(&request, &caps(false)));
    }

    #[test]
    fn caching_marks_the_last_system_block_and_the_newest_messages() {
        let mut request = request(vec![
            Message::text(Role::User, "one"),
            Message::text(Role::Assistant, "two"),
            Message::text(Role::User, "three"),
            Message::text(Role::Assistant, "four"),
            Message::text(Role::User, "five"),
        ]);
        request.system = vec![
            SystemBlock {
                text: "identity".into(),
                cache: true,
            },
            SystemBlock {
                text: "tool guidance".into(),
                cache: true,
            },
            SystemBlock {
                text: "the working directory changes every turn".into(),
                cache: false,
            },
        ];
        let body = encode(&request, &caps(true));
        insta::assert_json_snapshot!(body);
        assert_eq!(
            breakpoints(&body),
            MAX_BREAKPOINTS,
            "the api caps a request at four breakpoints"
        );
    }

    #[test]
    fn a_model_without_caching_gets_no_breakpoints() {
        let mut request = request(vec![Message::text(Role::User, "one")]);
        request.system = vec![SystemBlock {
            text: "identity".into(),
            cache: true,
        }];
        assert_eq!(breakpoints(&encode(&request, &caps(false))), 0);
    }

    #[test]
    fn the_budget_never_exceeds_four_however_long_the_conversation() {
        let messages: Vec<Message> = (0..40)
            .map(|i| Message::text(Role::User, format!("turn {i}")))
            .collect();
        let mut request = request(messages);
        request.system = vec![SystemBlock {
            text: "identity".into(),
            cache: true,
        }];
        assert_eq!(breakpoints(&encode(&request, &caps(true))), MAX_BREAKPOINTS);
    }

    #[test]
    fn a_message_with_no_cacheable_block_spends_none_of_the_budget() {
        let request = request(vec![
            Message::text(Role::User, "hi"),
            Message::assistant(vec![ContentPart::Reasoning {
                text: "hm".into(),
                provider_metadata: meta("signature", "sig"),
            }]),
        ]);
        let body = encode(&request, &caps(true));
        assert_eq!(
            body["messages"][0]["content"][0]["cache_control"],
            ephemeral()
        );
        assert_eq!(body["messages"][1]["content"][0].get("cache_control"), None);
    }

    #[test]
    fn provider_options_reach_the_body_unchanged() {
        let mut request = request(vec![Message::text(Role::User, "hi")]);
        request.provider_options = ProviderMetadata::from([(
            PROVIDER.to_string(),
            Map::from_iter([
                ("temperature".to_string(), json!(0.2)),
                ("service_tier".to_string(), json!("priority")),
            ]),
        )]);
        let body = encode(&request, &caps(false));
        assert_eq!(body["temperature"], json!(0.2));
        assert_eq!(body["service_tier"], json!("priority"));
    }

    #[test]
    fn counting_tokens_measures_the_same_conversation_without_generation_parameters() {
        let mut request = request(vec![Message::text(Role::User, "hi")]);
        request.tools = vec![read_tool()];
        request.reasoning = Some(Effort::High);
        let body = count_tokens(&request, &caps(false));
        assert_eq!(body["messages"], encode(&request, &caps(false))["messages"]);
        assert_eq!(body["tools"], encode(&request, &caps(false))["tools"]);
        for absent in ["max_tokens", "stream", "thinking", "output_config"] {
            assert!(body.get(absent).is_none(), "{absent} is not a count input");
        }
    }

    fn ephemeral() -> Value {
        json!({ "type": "ephemeral" })
    }
}

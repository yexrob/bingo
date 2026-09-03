//! `ModelRequest` → the `POST /v1/responses` wire body.
//!
//! Pure: the request and the variant decide everything, so a snapshot pins
//! every shape the API is picky about. Stateless by design — the journal is
//! the source of truth, so `store` is always `false` and the whole
//! conversation is re-sent; the encrypted reasoning state travels with it
//! rather than living in a response id on OpenAI's side. Verified against the
//! Responses reference and the reasoning guide, 2026-08-29.

use bingo_sdk::{Effort, ModelRequest, SystemBlock, ToolSpec};
use serde_json::{Map, Value, json};

use crate::effort::effort_for;
use crate::events::PROVIDER;
use crate::input;
use crate::variant::Variant;

/// What the endpoint must hand back for a reasoning turn to be replayable at
/// all when nothing is stored server-side.
const ENCRYPTED_REASONING: &str = "reasoning.encrypted_content";

/// The most detailed summariser the model offers; without it a reasoning turn
/// shows the user an empty thinking block.
const SUMMARY: &str = "auto";

pub fn encode(request: &ModelRequest, variant: Variant) -> Value {
    let mut body = Map::new();
    body.insert("model".into(), json!(request.model));
    body.insert("stream".into(), json!(true));
    // Nothing is kept on the server: every turn re-sends the conversation.
    body.insert("store".into(), json!(false));
    if variant.sends_max_output_tokens() {
        body.insert("max_output_tokens".into(), json!(request.max_tokens));
    }
    if let Some(instructions) = instructions(&request.system) {
        body.insert("instructions".into(), json!(instructions));
    }
    body.insert("input".into(), input::items(&request.messages).into());
    add_tools(&mut body, &request.tools);
    if let Some(effort) = request.reasoning {
        add_reasoning(&mut body, &request.model, effort);
    }
    merge_provider_options(&mut body, request.provider_options.get(PROVIDER));
    Value::Object(body)
}

/// Responses takes one `instructions` string, so the cacheable segmentation
/// the Messages API buys with breakpoints collapses to a join here.
fn instructions(system: &[SystemBlock]) -> Option<String> {
    if system.is_empty() {
        return None;
    }
    Some(
        system
            .iter()
            .map(|block| block.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

/// Empty `tools` stays off the wire: an endpoint that validates the field
/// rejects an empty array rather than ignoring it.
fn add_tools(body: &mut Map<String, Value>, tools: &[ToolSpec]) {
    if tools.is_empty() {
        return;
    }
    body.insert("tools".into(), tools.iter().map(tool).collect());
}

/// `strict: false` — a strict schema demands `additionalProperties: false`
/// and every property required, which the tool schemas do not promise.
fn tool(spec: &ToolSpec) -> Value {
    json!({
        "type": "function",
        "name": spec.name,
        "description": spec.description,
        "parameters": spec.input_schema,
        "strict": false,
    })
}

fn add_reasoning(body: &mut Map<String, Value>, model: &str, effort: Effort) {
    body.insert(
        "reasoning".into(),
        json!({ "effort": effort_for(model, effort), "summary": SUMMARY }),
    );
    body.insert("include".into(), json!([ENCRYPTED_REASONING]));
}

/// `provider_options["openai"]` goes on the body as it came, so a caller can
/// reach a parameter this adapter was written before.
fn merge_provider_options(body: &mut Map<String, Value>, extra: Option<&Map<String, Value>>) {
    let Some(extra) = extra else { return };
    for (key, value) in extra {
        body.insert(key.clone(), value.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{ContentPart, Image, Message, ProviderMetadata, Role};

    const MODEL: &str = "gpt-5.4";

    fn request(messages: Vec<Message>) -> ModelRequest {
        ModelRequest {
            model: MODEL.into(),
            max_tokens: 4096,
            system: Vec::new(),
            messages,
            tools: Vec::new(),
            reasoning: None,
            session: None,
            provider_options: ProviderMetadata::new(),
        }
    }

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

    fn read_tool() -> ToolSpec {
        ToolSpec {
            name: "Read".into(),
            description: "Read a file from the filesystem.".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "file_path": { "type": "string" } },
                "required": ["file_path"],
            }),
            meta: Default::default(),
        }
    }

    #[test]
    fn a_text_turn_encodes_to_a_stateless_streaming_body() {
        let mut request = request(vec![Message::text(Role::User, "hello")]);
        request.system = vec![
            SystemBlock {
                text: "You are bingo.".into(),
                cache: true,
            },
            SystemBlock {
                text: "The working directory is /repo.".into(),
                cache: false,
            },
        ];
        insta::assert_json_snapshot!(encode(&request, Variant::Default));
    }

    #[test]
    fn a_tool_round_encodes_the_schema_the_call_and_its_result() {
        let mut request = request(vec![
            Message::text(Role::User, "read Cargo.toml"),
            Message::assistant(vec![ContentPart::ToolUse {
                id: "call_01Read".into(),
                name: "Read".into(),
                input: json!({ "file_path": "Cargo.toml" }),
            }]),
            Message::user(vec![ContentPart::ToolResult {
                tool_use_id: "call_01Read".into(),
                parts: vec![ContentPart::text("[package]")],
                is_error: false,
            }]),
            Message::assistant(vec![ContentPart::text("It is a workspace manifest.")]),
            Message::user(vec![ContentPart::ToolResult {
                tool_use_id: "call_01Miss".into(),
                parts: vec![ContentPart::text("no such file")],
                is_error: true,
            }]),
        ]);
        request.tools = vec![read_tool()];
        insta::assert_json_snapshot!(encode(&request, Variant::Default));
    }

    #[test]
    fn reasoning_asks_for_encrypted_state_and_replays_only_what_carries_it() {
        let mut request = request(vec![
            Message::text(Role::User, "why?"),
            Message::assistant(vec![
                ContentPart::Reasoning {
                    text: "weigh the options".into(),
                    provider_metadata: meta(&[("id", "rs_01"), ("encrypted_content", "gAAAAAB")]),
                },
                ContentPart::Reasoning {
                    text: "this one came back unencrypted".into(),
                    provider_metadata: ProviderMetadata::new(),
                },
                ContentPart::text("Because it is simpler."),
            ]),
        ]);
        request.reasoning = Some(Effort::High);
        let body = encode(&request, Variant::Default);
        insta::assert_json_snapshot!(body);
        assert!(
            !body.to_string().contains("unencrypted"),
            "a reasoning part with no encrypted content has no wire form"
        );
    }

    #[test]
    fn an_image_becomes_a_data_url_input_image() {
        let request = request(vec![Message::user(vec![
            ContentPart::text("what is this?"),
            ContentPart::Image(Image {
                media_type: "image/png".into(),
                data: "iVBORw0KGgo=".into(),
            }),
        ])]);
        insta::assert_json_snapshot!(encode(&request, Variant::Default));
    }

    /// The subscription endpoint's departures, in one place: a different
    /// path (asserted in `variant`), no output budget, everything else the
    /// same body. Ported from the old `providers/openai.rs:985-1004`.
    #[test]
    fn the_codex_variant_differs_only_by_the_output_budget() {
        let mut request = request(vec![Message::text(Role::User, "hi")]);
        request.reasoning = Some(Effort::High);
        let codex = encode(&request, Variant::Codex);
        let default = encode(&request, Variant::Default);

        assert!(
            codex.get("max_output_tokens").is_none(),
            "the subscription endpoint 400s on max_output_tokens"
        );
        assert_eq!(default["max_output_tokens"], json!(4096));

        for shared in [
            "model",
            "stream",
            "store",
            "instructions",
            "input",
            "reasoning",
            "include",
        ] {
            assert_eq!(
                codex.get(shared),
                default.get(shared),
                "{shared} must not differ between variants"
            );
        }
        assert_eq!(codex["store"], json!(false));
        assert_eq!(codex["stream"], json!(true));
        assert_eq!(codex["include"], json!([ENCRYPTED_REASONING]));
        assert_eq!(
            codex["reasoning"],
            json!({ "effort": "high", "summary": SUMMARY })
        );
    }

    #[test]
    fn no_reasoning_sends_neither_the_object_nor_the_include() {
        let body = encode(
            &request(vec![Message::text(Role::User, "hi")]),
            Variant::Default,
        );
        assert!(body.get("reasoning").is_none());
        assert!(body.get("include").is_none());
    }

    #[test]
    fn no_system_and_no_tools_leave_the_fields_off_the_wire() {
        let body = encode(
            &request(vec![Message::text(Role::User, "hi")]),
            Variant::Default,
        );
        assert!(body.get("instructions").is_none());
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn the_system_blocks_join_into_one_instructions_string() {
        let mut request = request(vec![Message::text(Role::User, "hi")]);
        request.system = vec![
            SystemBlock {
                text: "one".into(),
                cache: true,
            },
            SystemBlock {
                text: "two".into(),
                cache: false,
            },
        ];
        assert_eq!(
            encode(&request, Variant::Default)["instructions"],
            json!("one\n\ntwo")
        );
    }

    #[test]
    fn the_effort_is_clamped_to_what_the_model_takes() {
        let mut request = request(vec![Message::text(Role::User, "hi")]);
        request.reasoning = Some(Effort::Max);
        assert_eq!(
            encode(&request, Variant::Default)["reasoning"]["effort"],
            json!("xhigh"),
            "gpt-5.4 stops at xhigh"
        );
        request.model = "gpt-5.6".into();
        assert_eq!(
            encode(&request, Variant::Default)["reasoning"]["effort"],
            json!("max")
        );
    }

    #[test]
    fn provider_options_reach_the_body_unchanged() {
        let mut request = request(vec![Message::text(Role::User, "hi")]);
        request.provider_options = ProviderMetadata::from([(
            PROVIDER.to_string(),
            Map::from_iter([
                ("temperature".to_string(), json!(0.2)),
                ("service_tier".to_string(), json!("priority")),
                ("store".to_string(), json!(true)),
            ]),
        )]);
        let body = encode(&request, Variant::Default);
        assert_eq!(body["temperature"], json!(0.2));
        assert_eq!(body["service_tier"], json!("priority"));
        assert_eq!(body["store"], json!(true), "the caller merges last");
    }
}

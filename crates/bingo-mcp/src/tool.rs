//! One server tool as a bingo tool.
//!
//! Everything a server says about itself is a claim: the traits are
//! [`ToolTraits::default`], the fail-closed reading, so the gate asks about
//! every call no matter what `readOnlyHint` said (ADR-0009 §2).

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ContentPart, Image, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits,
};
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock, JsonObject};
use serde_json::Value;

use crate::client::Asker;
use crate::dial::Service;

/// The model-visible name of a server's tool.
///
/// Both names are copied as they are. The permission grammar splits
/// `mcp__<server>__<tool>` on its first two separators, so a server or a tool
/// whose own name contains `__` still reaches the rule written for it, and a
/// rewritten name would reach none.
pub fn tool_name(server: &str, tool: &str) -> String {
    format!("mcp__{server}__{tool}")
}

/// A tool a connected server advertised, bound to the connection that answers it.
pub struct McpTool {
    server: String,
    /// The server's own name for it, which is what `tools/call` is sent.
    tool: String,
    description: String,
    input_schema: Value,
    service: Arc<Service>,
    /// Who a question this server raises mid-call reaches (ADR-0039 §1).
    asker: Arc<Asker>,
}

impl McpTool {
    pub fn new(
        server: &str,
        listed: &rmcp::model::Tool,
        service: Arc<Service>,
        asker: Arc<Asker>,
    ) -> Self {
        Self {
            server: server.to_string(),
            tool: listed.name.to_string(),
            description: listed
                .description
                .as_deref()
                .unwrap_or_default()
                .to_string(),
            input_schema: input_schema(&listed.input_schema),
            service,
            asker,
        }
    }
}

/// The server's schema as a model receives it: `$schema` is a document's own
/// dialect marker, and no provider wants it inside a tool definition.
fn input_schema(schema: &JsonObject) -> Value {
    let mut schema = schema.clone();
    schema.remove("$schema");
    Value::Object(schema)
}

/// What a catalogue may show beside the tool: which server it came from.
fn meta(server: &str) -> serde_json::Map<String, Value> {
    let mut meta = serde_json::Map::new();
    meta.insert("server".to_string(), Value::String(server.to_string()));
    meta
}

/// The model's input as `tools/call` arguments. An input that is not an object
/// is the model's mistake, not the server's, and saying so beats sending a call
/// with the arguments quietly dropped.
fn arguments(input: Value) -> Result<Option<JsonObject>, ToolError> {
    match input {
        Value::Object(map) => Ok(Some(map)),
        Value::Null => Ok(None),
        other => Err(ToolError::InvalidInput(format!(
            "arguments must be an object, not {}",
            shape(&other)
        ))),
    }
}

fn shape(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// The server's content blocks as the model sees them. Text and images pass
/// through; a block this kernel has no part for reaches the model as the JSON
/// it was, because content the model can read beats content it cannot.
pub fn output(result: CallToolResult) -> ToolOutput {
    ToolOutput {
        parts: result.content.iter().map(part).collect(),
        is_error: result.is_error.unwrap_or(false),
        display: None,
    }
}

fn part(block: &ContentBlock) -> ContentPart {
    match block {
        ContentBlock::Text(text) => ContentPart::Text {
            text: text.text.clone(),
        },
        ContentBlock::Image(image) => ContentPart::Image(Image {
            media_type: image.mime_type.clone(),
            data: image.data.clone(),
        }),
        other => ContentPart::text(
            serde_json::to_string(other)
                .unwrap_or_else(|_| "[a content block that will not serialize]".to_string()),
        ),
    }
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: tool_name(&self.server, &self.tool),
            description: self.description.clone(),
            input_schema: self.input_schema.clone(),
            meta: meta(&self.server),
        }
    }

    /// Untrusted, whatever the server claimed about itself.
    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::default()
    }

    fn subjects(&self, _input: &Value, _cwd: &Path) -> Vec<bingo_sdk::Subject> {
        Vec::new()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let mut params = CallToolRequestParams::new(self.tool.clone());
        params.arguments = arguments(input)?;
        // A question this server raises while it answers reaches the session
        // waiting on this call, and only for as long as it waits.
        let _asking = self.asker.during(Arc::clone(&cx.call), cx.host.clone());
        let answered = tokio::select! {
            biased;
            () = cx.cancel.cancelled() => return Err(ToolError::Cancelled),
            answered = self.service.call_tool(params) => answered,
        };
        let result = answered.map_err(|e| ToolError::Failed(format!("{}: {e}", self.server)))?;
        Ok(output(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use rmcp::model::{ImageContent, TextContent};
    use serde_json::json;

    fn result(content: Vec<ContentBlock>, is_error: Option<bool>) -> CallToolResult {
        let mut result = CallToolResult::success(content);
        result.is_error = is_error;
        result
    }

    #[test]
    fn a_tool_is_named_for_its_server_and_itself() {
        assert_eq!(tool_name("files", "read"), "mcp__files__read");
    }

    #[test]
    fn a_name_that_already_holds_the_separator_is_still_copied() {
        assert_eq!(tool_name("a__b", "c__d"), "mcp__a__b__c__d");
    }

    #[test]
    fn the_dialect_marker_leaves_the_schema_and_the_rest_stays() {
        let mut schema = JsonObject::new();
        schema.insert(
            "$schema".into(),
            json!("https://json-schema.org/draft/2020-12/schema"),
        );
        schema.insert("type".into(), json!("object"));
        schema.insert("title".into(), json!("Echo"));
        let cleaned = input_schema(&schema);
        assert_eq!(cleaned["type"], json!("object"));
        assert_eq!(cleaned["title"], json!("Echo"), "only $schema is dropped");
        assert!(cleaned.get("$schema").is_none());
    }

    #[test]
    fn the_catalogue_learns_which_server_a_tool_came_from() {
        assert_eq!(meta("files")["server"], json!("files"));
    }

    #[test]
    fn text_and_images_pass_through_and_anything_else_arrives_as_json() {
        let output = output(result(
            vec![
                ContentBlock::Text(TextContent::new("hello")),
                ContentBlock::Image(ImageContent::new("QUJD", "image/png")),
                ContentBlock::audio("QUJD", "audio/wav"),
            ],
            None,
        ));
        assert_eq!(
            output.parts[0],
            ContentPart::Text {
                text: "hello".into()
            }
        );
        assert_eq!(
            output.parts[1],
            ContentPart::Image(Image {
                media_type: "image/png".into(),
                data: "QUJD".into()
            })
        );
        let ContentPart::Text { text } = &output.parts[2] else {
            panic!("an audio block reaches the model as text");
        };
        assert!(text.contains("audio/wav"), "{text}");
        assert!(!output.is_error);
    }

    #[test]
    fn is_error_crosses_the_boundary() {
        assert!(output(result(Vec::new(), Some(true))).is_error);
        assert!(!output(result(Vec::new(), Some(false))).is_error);
        assert!(
            !output(result(Vec::new(), None)).is_error,
            "a server that says nothing said no error"
        );
    }

    #[test]
    fn arguments_are_the_object_the_model_sent() {
        let map = arguments(json!({ "text": "hi" })).expect("an object is arguments");
        assert_eq!(map.expect("some arguments")["text"], json!("hi"));
        assert!(
            arguments(Value::Null)
                .expect("null is no arguments")
                .is_none()
        );
        let error = arguments(json!("hi")).expect_err("a string is not arguments");
        assert!(matches!(error, ToolError::InvalidInput(_)), "{error}");
    }

    proptest! {
        /// A name is only ever the two the server and the tool were given,
        /// joined; nothing is rewritten on the way through.
        #[test]
        fn a_name_is_its_two_parts_and_nothing_else(
            server in ".{0,24}",
            tool in ".{0,24}",
        ) {
            let name = tool_name(&server, &tool);
            prop_assert!(name.starts_with("mcp__"));
            prop_assert_eq!(name, format!("mcp__{server}__{tool}"));
        }
    }
}

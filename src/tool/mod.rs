use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use thiserror::Error;

use crate::settings::HooksConfig;
use crate::tasks::TaskStore;

use crate::error::ErrorCode;

pub mod agent;
pub mod ask;
pub mod bash;
pub mod channel;
pub mod diff;
pub mod edit;
pub mod executor;
pub mod experience;
pub mod glob;
pub mod grep;
pub mod read;
pub mod skill;
pub mod task;
pub mod webfetch;
pub mod websearch;
pub mod write;

/// Tool execution context: shared across one queryLoop turn.
#[derive(Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    /// User home directory (the Experience tools locate the user-level experience root here).
    pub home: PathBuf,
    /// Watchable registry (background task lifecycle and notifications).
    pub watch: std::sync::Arc<crate::watch::WatchRegistry>,
    /// Shared HTTP client (WebFetch/WebSearch reuse the connection pool; does not follow redirects).
    pub http: reqwest::Client,
    /// Task store (Task tool family; shared with the TUI task panel).
    pub tasks: std::sync::Arc<TaskStore>,
    /// Hooks configuration (TaskCreated/TaskCompleted events).
    pub hooks: HooksConfig,
    /// Permission mode string (hook input contract).
    pub permission_mode: String,
    /// Task panel expand signal (no subscribers in headless mode).
    pub expand_tasks: tokio::sync::watch::Sender<bool>,
    /// Ask the user multiple-choice questions (AskUserQuestion tool): title + question + options
    /// → option index (None = user skipped/Esc). The TUI reuses the permission prompt modal.
    pub ask_question: std::sync::Arc<crate::query::AskQuestionFn>,
}

impl ToolContext {
    /// Notify the TUI to expand the task panel on a tool call.
    pub fn set_expanded_view_tasks(&self) {
        let _ = self.expand_tasks.send(true);
    }
}

/// Tool execution result: content is fed back to the model as tool_result content.
/// diff is optional unified diff text (UI preview for edit tools like Edit/Write;
/// not fed back to the model).
#[derive(Debug, Default)]
pub struct ToolResult {
    pub content: serde_json::Value,
    pub is_error: bool,
    pub diff: Option<String>,
}

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("{0}")]
    Failed(String),
}

impl ErrorCode for ToolError {
    fn error_code(&self) -> &'static str {
        match self {
            ToolError::Failed(_) => "TOOL_FAILED",
        }
    }
}

impl ToolError {
    pub fn failed(msg: impl Into<String>) -> Self {
        Self::Failed(msg.into())
    }
}

/// Tool contract (D2).
/// Defaults are fail-closed: not concurrency-safe, not read-only, allowed (permissions
/// are left to the unified gate).
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> String;
    fn description(&self) -> String;
    fn input_schema(&self) -> serde_json::Value;
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_destructive(&self, _input: &serde_json::Value) -> bool {
        false
    }
    /// Edit-type tools (Edit/Write etc.): automatically allowed in acceptEdits mode,
    /// asked as usual otherwise.
    fn is_edit_tool(&self, _input: &serde_json::Value) -> bool {
        false
    }
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError>;
}

/// Model-returned parameters → target type. Failure info is visible to the model (fed back via is_error).
pub fn parse_input<T: for<'a> Deserialize<'a>>(
    input: &serde_json::Value,
) -> Result<T, ToolError> {
    serde_json::from_value(input.clone()).map_err(|e| ToolError::failed(format!("bad input: {e}")))
}

/// Generate inputSchema from the input struct (schemars, single source of truth, D2).
/// Strip the `$schema` key: the tool schema is sent to the model with tool_params, keeping
/// its established shape. `#/definitions/...` references produced by nested types must be
/// carried along with the root schema, otherwise the model's $ref dangles (as with
/// AskUserQuestion's questions/options).
pub fn schema_for<T: schemars::JsonSchema>() -> serde_json::Value {
    let mut generator = schemars::r#gen::SchemaGenerator::default();
    let mut value = serde_json::to_value(T::json_schema(&mut generator))
        .unwrap_or_else(|_| serde_json::json!({ "type": "object" }));
    // `#/definitions/...` references produced by nested types must be carried along with
    // the root schema, otherwise the model's $ref dangles (as with AskUserQuestion's
    // questions/options).
    if !generator.definitions().is_empty()
        && let Some(obj) = value.as_object_mut()
    {
        obj.insert(
            "definitions".to_string(),
            serde_json::to_value(generator.definitions()).unwrap_or_default(),
        );
    }
    value
}

/// Find a tool by name in the registry.
pub fn find_tool<'a>(tools: &'a [Box<dyn Tool>], name: &str) -> Option<&'a dyn Tool> {
    tools.iter().map(|t| t.as_ref()).find(|t| t.name() == name)
}

/// Assemble the tools parameter sent to the API.
pub fn tool_params(tools: &[Box<dyn Tool>]) -> Vec<serde_json::Value> {
    tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name(),
                "description": t.description(),
                "input_schema": t.input_schema(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::tool::bash::BashTool;
    use crate::tool::read::ReadTool;

    #[test]
    fn agent_schema_matches_input_struct() {
        // Regression: schema and input struct share a single source (D2). It once drifted —
        // the struct was missing description.
        let schema = schema_for::<agent::AgentInput>();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["prompt"]));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert!(schema["properties"]["description"].is_object());
        assert_eq!(schema["properties"]["description"]["type"], json!(["string", "null"]));
    }

    #[test]
    fn read_schema_shape() {
        let schema = ReadTool::new().input_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["file_path"]));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["properties"]["file_path"]["type"], "string");
        assert_eq!(schema["properties"]["file_path"]["description"], "File path to read (absolute or relative)");
    }

    #[test]
    fn bash_schema_optional_timeout() {
        let schema = BashTool::new().input_schema();
        assert_eq!(schema["required"], json!(["command"]));
        // Option fields do not go into required
        assert!(!schema["required"].as_array().unwrap().contains(&json!("timeout")));
    }

    #[test]
    fn schema_has_no_dollar_schema_key() {
        let schema = ReadTool::new().input_schema();
        assert!(schema.get("$schema").is_none(), "发给模型的形状不含 $schema: {schema}");
    }
}

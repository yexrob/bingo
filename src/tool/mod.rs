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
pub mod team;
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
    /// Foreground liveness (D84): where a running shell command publishes its
    /// output tail, and where the host's ctrl+b reaches it. Defaults to a
    /// detached handle — a host with no foreground surface sees no difference.
    pub live: std::sync::Arc<crate::live::LiveBash>,
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
    /// Instance name of the calling session (None = main session). Watches registered here are
    /// addressed to it, so its notifications land at its own turn boundary.
    pub instance: Option<String>,
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
    /// A call whose consequence is the user's to accept in person: `Some(reason)` forces
    /// the permission prompt no matter the mode, and no allow rule can pre-authorize it
    /// (same standing as the sensitive-path safety check). Only a deny rule outranks it.
    ///
    /// Reserve it for decisions a permission table can't express — changing the shape of
    /// the project's crew, not writing another file. The reason is user-facing copy shown
    /// in the modal: one short line naming what is about to change.
    fn confirm_reason(&self, _input: &serde_json::Value) -> Option<String> {
        None
    }
    /// A dry run of the change this call would make, as a unified diff, so the
    /// approval prompt can show what it is approving instead of naming it.
    /// Reads the current file and nothing else — it must never write.
    /// `None`: nothing to preview (or the change cannot be computed yet).
    fn preview_diff(&self, _input: &serde_json::Value, _cwd: &std::path::Path) -> Option<String> {
        None
    }
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError>;
}

/// A tool's `file_path` against the session cwd: absolute paths stand, relative
/// ones hang off `cwd`. The approval preview and the write itself must resolve
/// the same way, or the diff shown belongs to a different file.
pub(crate) fn resolve_path(path: &str, cwd: &std::path::Path) -> std::path::PathBuf {
    let path = std::path::PathBuf::from(path);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

/// Model-returned parameters → target type. Failure info is visible to the model (fed back via is_error).
pub fn parse_input<T: for<'a> Deserialize<'a>>(input: &serde_json::Value) -> Result<T, ToolError> {
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
    use crate::tool::glob::GlobTool;
    use crate::tool::grep::GrepTool;
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
        assert_eq!(
            schema["properties"]["description"]["type"],
            json!(["string", "null"])
        );
    }

    #[test]
    fn read_schema_shape() {
        let schema = ReadTool::new().input_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["file_path"]));
        assert_eq!(schema["additionalProperties"], json!(false));
        assert_eq!(schema["properties"]["file_path"]["type"], "string");
        assert_eq!(
            schema["properties"]["file_path"]["description"],
            "File path to read (absolute or relative)"
        );
    }

    #[test]
    fn file_tool_optional_parameters_stay_optional() {
        let read = ReadTool::new().input_schema();
        assert_eq!(read["required"], json!(["file_path"]));
        for name in ["start_line", "end_line"] {
            assert!(
                read["properties"][name].is_object(),
                "missing {name}: {read}"
            );
        }

        let grep = GrepTool.input_schema();
        assert_eq!(grep["required"], json!(["pattern"]));
        for name in [
            "context",
            "case_insensitive",
            "whole_word",
            "fixed_string",
            "files_with_matches",
        ] {
            assert!(
                grep["properties"][name].is_object(),
                "missing {name}: {grep}"
            );
        }

        let glob = GlobTool.input_schema();
        assert_eq!(glob["required"], json!(["pattern"]));
        for name in ["exclude", "max_depth"] {
            assert!(
                glob["properties"][name].is_object(),
                "missing {name}: {glob}"
            );
        }
    }

    #[test]
    fn bash_schema_optional_timeout() {
        let schema = BashTool::new().input_schema();
        assert_eq!(schema["required"], json!(["command"]));
        // Option fields do not go into required
        assert!(
            !schema["required"]
                .as_array()
                .unwrap()
                .contains(&json!("timeout"))
        );
    }

    #[test]
    fn schema_has_no_dollar_schema_key() {
        let schema = ReadTool::new().input_schema();
        assert!(
            schema.get("$schema").is_none(),
            "schema shape sent to the model must not include $schema: {schema}"
        );
    }
}

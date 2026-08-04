use async_trait::async_trait;
use serde::Deserialize;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

#[derive(Debug, Deserialize)]
pub struct EditInput {
    #[serde(rename = "file_path")]
    pub file_path: String,
    #[serde(rename = "old_string")]
    pub old_string: String,
    #[serde(rename = "new_string")]
    pub new_string: String,
    #[serde(rename = "replace_all", default)]
    pub replace_all: bool,
}

/// Edit：old_string → new_string 精确替换（对标 Claude Code FileEditTool）。
pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> String {
        "Edit".into()
    }
    fn description(&self) -> String {
        "Replace an exact string in a file with a new one. old_string must appear in the file."
            .into()
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string"},
                "old_string": {"type": "string"},
                "new_string": {"type": "string"},
                "replace_all": {"type": "boolean", "description": "replace every occurrence"}
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_destructive(&self, _input: &serde_json::Value) -> bool {
        true
    }
    /// 编辑类工具：acceptEdits 模式下自动允许。
    fn is_edit_tool(&self, _input: &serde_json::Value) -> bool {
        true
    }
    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: EditInput = parse_input(&input)?;
        let path = std::path::PathBuf::from(&params.file_path);
        if params.old_string.is_empty() {
            return Err(ToolError::failed("old_string must not be empty"));
        }
        let content = std::fs::read_to_string(&path)
            .map_err(|e| ToolError::failed(format!("cannot read {}: {e}", path.display())))?;
        let count = content.matches(&params.old_string).count();
        if count == 0 {
            return Err(ToolError::failed(format!(
                "old_string not found in {}",
                params.file_path
            )));
        }
        let replaced = if params.replace_all {
            content.replace(&params.old_string, &params.new_string)
        } else {
            content.replacen(&params.old_string, &params.new_string, 1)
        };
        std::fs::write(&path, replaced)
            .map_err(|e| ToolError::failed(format!("cannot write {}: {e}", path.display())))?;
        let mut text = format!(
            "Edited {}: {} occurrence{} of old_string",
            params.file_path,
            count.min(if params.replace_all { count } else { 1 }),
            if count == 1 { "" } else { "s" }
        );
        if !params.replace_all && count > 1 {
            text.push_str(&format!(
                " ({count} total; use replace_all to replace all)"
            ));
        }
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
        })
    }
}

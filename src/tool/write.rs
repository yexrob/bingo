use async_trait::async_trait;
use serde::Deserialize;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

#[derive(Debug, Deserialize)]
pub struct WriteInput {
    #[serde(rename = "file_path")]
    pub file_path: String,
    pub content: String,
}

/// Write：覆盖写文件（自动创建父目录；对标 Claude Code FileWriteTool）。
pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> String {
        "Write".into()
    }
    fn description(&self) -> String {
        "Write full content to a file, creating parent directories as needed. Overwrites existing files."
            .into()
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {"type": "string"},
                "content": {"type": "string"}
            },
            "required": ["file_path", "content"]
        })
    }
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_destructive(&self, _input: &serde_json::Value) -> bool {
        true
    }
    fn is_edit_tool(&self, _input: &serde_json::Value) -> bool {
        true
    }
    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: WriteInput = parse_input(&input)?;
        let path = std::path::PathBuf::from(&params.file_path);
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            return Err(ToolError::failed(format!(
                "cannot create dir {}: {e}",
                parent.display()
            )));
        }
        std::fs::write(&path, &params.content)
            .map_err(|e| ToolError::failed(format!("cannot write {}: {e}", path.display())))?;
        let bytes = params.content.len();
        Ok(ToolResult {
            content: serde_json::Value::String(format!(
                "Wrote {} bytes to {}",
                bytes, params.file_path
            )),
            is_error: false,
            diff: super::diff::unified_diff(&params.file_path, &old, &params.content),
        })
    }
}

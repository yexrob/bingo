use std::path::PathBuf;

use serde::Deserialize;

use async_trait::async_trait;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// 单次读取的最大字符数，超出截断（Claude Code 同款策略）。
const MAX_READ_CHARS: usize = 20_000;

#[derive(Debug, Deserialize)]
struct ReadInput {
    file_path: String,
}

pub struct ReadTool;

impl ReadTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> String {
        "Read".to_string()
    }

    fn description(&self) -> String {
        "读取文件内容，支持绝对路径与相对路径。".to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "要读取的文件路径（绝对或相对）"
                }
            },
            "required": ["file_path"],
            "additionalProperties": false
        })
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        true
    }

    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        true
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: ReadInput = parse_input(&input)?;
        let path = PathBuf::from(&params.file_path);
        let path = if path.is_absolute() {
            path
        } else {
            ctx.cwd.join(&path)
        };

        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| ToolError::failed(format!("failed to read {}: {e}", path.display())))?;

        let truncated = content.chars().count() > MAX_READ_CHARS;
        let mut text = String::new();
        if truncated {
            text.push_str(&content.chars().take(MAX_READ_CHARS).collect::<String>());
            text.push_str(&format!(
                "\n[Content truncated: {} characters total, showing first {MAX_READ_CHARS}]",
                content.chars().count()
            ));
        } else {
            text.push_str(&content);
        }

        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
        })
    }
}

use std::path::PathBuf;

use serde::Deserialize;

use async_trait::async_trait;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// Max characters per read; anything beyond is truncated.
const MAX_READ_CHARS: usize = 20_000;
/// Byte cap for partial reads: a UTF-8 character is at most 4 bytes, so reading this much
/// is guaranteed to fill MAX_READ_CHARS (the extra bytes leave room for a trailing split character).
const MAX_READ_BYTES: u64 = MAX_READ_CHARS as u64 * 4 + 4;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ReadInput {
    #[schemars(description = "File path to read (absolute or relative)")]
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
        "Read file content; accepts absolute and relative paths.".to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<ReadInput>()
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

        // Check the size first: for oversized files only read the needed prefix, instead of
        // loading the whole content into memory and discarding it.
        let size = tokio::fs::metadata(&path)
            .await
            .map_err(|e| ToolError::failed(format!("failed to read {}: {e}", path.display())))?
            .len();

        let text = if size > MAX_READ_BYTES {
            let head = read_prefix(&path).await?;
            let mut text: String = head.chars().take(MAX_READ_CHARS).collect();
            text.push_str(&format!(
                "\n[Content truncated: file is {size} bytes, showing first {MAX_READ_CHARS} characters]"
            ));
            text
        } else {
            let content = tokio::fs::read_to_string(&path).await.map_err(|e| {
                ToolError::failed(format!("failed to read {}: {e}", path.display()))
            })?;
            let total = content.chars().count();
            if total > MAX_READ_CHARS {
                let mut text: String = content.chars().take(MAX_READ_CHARS).collect();
                text.push_str(&format!(
                    "\n[Content truncated: {total} characters total, showing first {MAX_READ_CHARS}]"
                ));
                text
            } else {
                content
            }
        };

        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: None,
        })
    }
}

/// Read only the first MAX_READ_BYTES bytes of the file (the tail may cut through a
/// multibyte character; lossy conversion).
async fn read_prefix(path: &std::path::Path) -> Result<String, ToolError> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| ToolError::failed(format!("failed to read {}: {e}", path.display())))?;
    let mut buf = Vec::with_capacity(MAX_READ_BYTES as usize);
    file.take(MAX_READ_BYTES)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| ToolError::failed(format!("failed to read {}: {e}", path.display())))?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            cwd: std::env::temp_dir(),
            home: std::env::temp_dir(),
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
        }
    }

    async fn read(path: &std::path::Path) -> String {
        ReadTool::new()
            .call(
                serde_json::json!({"file_path": path.to_string_lossy()}),
                &ctx(),
            )
            .await
            .unwrap()
            .content
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    /// L4: huge files only read the prefix, still truncated correctly by character (multibyte-safe).
    #[tokio::test]
    async fn huge_file_is_partially_read_and_truncated() {
        let path = std::env::temp_dir().join(format!("bingo-read-huge-{}", std::process::id()));
        // 3-byte-per-char Chinese, far exceeding MAX_READ_BYTES in total.
        let body = "中".repeat(MAX_READ_CHARS * 3);
        std::fs::write(&path, &body).unwrap();
        let text = read(&path).await;
        assert!(text.contains("[Content truncated: file is"), "{}", &text[..80]);
        let head: String = text.chars().take_while(|c| *c == '中').collect();
        assert_eq!(head.chars().count(), MAX_READ_CHARS);
        std::fs::remove_file(&path).unwrap();
    }

    /// Small files are returned verbatim without a truncation note.
    #[tokio::test]
    async fn small_file_is_returned_verbatim() {
        let path = std::env::temp_dir().join(format!("bingo-read-small-{}", std::process::id()));
        std::fs::write(&path, "hello 世界\n").unwrap();
        assert_eq!(read(&path).await, "hello 世界\n");
        std::fs::remove_file(&path).unwrap();
    }
}

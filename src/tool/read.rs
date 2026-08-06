use std::path::PathBuf;

use serde::Deserialize;

use async_trait::async_trait;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// 单次读取的最大字符数，超出截断。
const MAX_READ_CHARS: usize = 20_000;
/// 部分读取的字节上限：UTF-8 一个字符最多 4 字节，
/// 读到这么多就一定够填满 MAX_READ_CHARS（多出的余量留给尾部截断的半个字符）。
const MAX_READ_BYTES: u64 = MAX_READ_CHARS as u64 * 4 + 4;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ReadInput {
    #[schemars(description = "要读取的文件路径（绝对或相对）")]
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

        // 先看大小：超限的文件只读需要的前缀，不把整份内容读进内存再丢掉。
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

/// 只读文件开头的 MAX_READ_BYTES 字节（尾部可能切在多字节字符中间，lossy 转换）。
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

    /// L4：超大文件只读前缀，仍按字符正确截断（多字节安全）。
    #[tokio::test]
    async fn huge_file_is_partially_read_and_truncated() {
        let path = std::env::temp_dir().join(format!("bingo-read-huge-{}", std::process::id()));
        // 每字符 3 字节的中文，总量远超 MAX_READ_BYTES。
        let body = "中".repeat(MAX_READ_CHARS * 3);
        std::fs::write(&path, &body).unwrap();
        let text = read(&path).await;
        assert!(text.contains("[Content truncated: file is"), "{}", &text[..80]);
        let head: String = text.chars().take_while(|c| *c == '中').collect();
        assert_eq!(head.chars().count(), MAX_READ_CHARS);
        std::fs::remove_file(&path).unwrap();
    }

    /// 小文件原样返回，不加截断标注。
    #[tokio::test]
    async fn small_file_is_returned_verbatim() {
        let path = std::env::temp_dir().join(format!("bingo-read-small-{}", std::process::id()));
        std::fs::write(&path, "hello 世界\n").unwrap();
        assert_eq!(read(&path).await, "hello 世界\n");
        std::fs::remove_file(&path).unwrap();
    }
}

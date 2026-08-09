use async_trait::async_trait;
use serde::Deserialize;

use super::{Tool, ToolContext, ToolError, ToolResult, parse_input};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct WriteInput {
    #[serde(rename = "file_path")]
    pub file_path: String,
    pub content: String,
}

/// Write: overwrite a file (creating parent directories automatically).
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
        super::schema_for::<WriteInput>()
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
        // Failed reads of the old content were once treated as an empty file: binary/
        // non-UTF-8/permission-denied files would be silently overwritten and the diff would
        // show everything as added. Only "not found" means a new file.
        let old = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                return Err(ToolError::failed(format!(
                    "refusing to overwrite {}: cannot read existing file ({e})",
                    path.display()
                )));
            }
        };
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
            instance: None,
        }
    }

    /// L3 regression: non-UTF-8 files were once treated as empty and silently overwritten.
    #[tokio::test]
    async fn refuses_to_overwrite_unreadable_file() {
        let path = std::env::temp_dir().join(format!("bingo-write-binary-{}", std::process::id()));
        std::fs::write(&path, [0xff_u8, 0xfe, 0x00, 0x01]).unwrap();
        let err = WriteTool
            .call(
                serde_json::json!({"file_path": path.to_string_lossy(), "content": "new"}),
                &ctx(),
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("refusing to overwrite"), "{err}");
        // The original file is untouched.
        assert_eq!(std::fs::read(&path).unwrap(), vec![0xff, 0xfe, 0x00, 0x01]);
        std::fs::remove_file(&path).unwrap();
    }

    /// Missing files are written as new files as usual.
    #[tokio::test]
    async fn missing_file_is_treated_as_new() {
        let path = std::env::temp_dir().join(format!("bingo-write-new-{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let result = WriteTool
            .call(
                serde_json::json!({"file_path": path.to_string_lossy(), "content": "hello\n"}),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
        std::fs::remove_file(&path).unwrap();
    }
}

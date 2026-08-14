use async_trait::async_trait;
use serde::Deserialize;

use super::{Tool, ToolContext, ToolError, ToolResult, parse_input};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EditInput {
    #[serde(rename = "file_path")]
    pub file_path: String,
    #[serde(rename = "old_string")]
    pub old_string: String,
    #[serde(rename = "new_string")]
    pub new_string: String,
    #[serde(rename = "replace_all", default)]
    #[schemars(description = "replace every occurrence")]
    pub replace_all: bool,
}

/// The replacement itself. The approval preview and the write share it, so what
/// the user approves is exactly what lands.
fn replace(content: &str, params: &EditInput) -> String {
    if params.replace_all {
        content.replace(&params.old_string, &params.new_string)
    } else {
        content.replacen(&params.old_string, &params.new_string, 1)
    }
}

/// Edit: exact replacement of old_string → new_string.
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
        super::schema_for::<EditInput>()
    }
    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }
    fn is_destructive(&self, _input: &serde_json::Value) -> bool {
        true
    }
    /// Edit-type tools: automatically allowed in acceptEdits mode.
    fn is_edit_tool(&self, _input: &serde_json::Value) -> bool {
        true
    }
    fn preview_diff(&self, input: &serde_json::Value, cwd: &std::path::Path) -> Option<String> {
        let params: EditInput = parse_input(input).ok()?;
        if params.old_string.is_empty() {
            return None;
        }
        let content = std::fs::read_to_string(super::resolve_path(&params.file_path, cwd)).ok()?;
        if !content.contains(&params.old_string) {
            return None;
        }
        let replaced = replace(&content, &params);
        super::diff::unified_diff(&params.file_path, &content, &replaced)
    }
    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: EditInput = parse_input(&input)?;
        let path = super::resolve_path(&params.file_path, &ctx.cwd);
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
        let replaced = replace(&content, &params);
        std::fs::write(&path, &replaced)
            .map_err(|e| ToolError::failed(format!("cannot write {}: {e}", path.display())))?;
        let mut text = format!(
            "Edited {}: {} occurrence{} of old_string",
            params.file_path,
            count.min(if params.replace_all { count } else { 1 }),
            if count == 1 { "" } else { "s" }
        );
        if !params.replace_all && count > 1 {
            text.push_str(&format!(" ({count} total; use replace_all to replace all)"));
        }
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: super::diff::unified_diff(&params.file_path, &content, &replaced),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn relative_path_resolves_from_context_cwd() {
        let root = std::env::temp_dir().join(format!("bingo-edit-cwd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("file.txt"), "before").unwrap();
        let context = ToolContext {
            cwd: root.clone(),
            home: std::env::temp_dir(),
            watch: crate::watch::WatchRegistry::new(),
            live: Default::default(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
            instance: None,
        };

        EditTool
            .call(
                serde_json::json!({
                    "file_path": "file.txt",
                    "old_string": "before",
                    "new_string": "after",
                }),
                &context,
            )
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("file.txt")).unwrap(),
            "after"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn edit_produces_diff() {
        let path = std::env::temp_dir().join(format!("bingo-edit-diff-{}", std::process::id()));
        std::fs::write(&path, "line one\nline two\nline three\n").unwrap();
        let result = EditTool
            .call(
                serde_json::json!({
                    "file_path": path.to_string_lossy(),
                    "old_string": "line two",
                    "new_string": "line TWO",
                }),
                &ToolContext {
                    cwd: Default::default(),
                    home: std::env::temp_dir(),
                    watch: crate::watch::WatchRegistry::new(),
                    live: Default::default(),
                    http: reqwest::Client::new(),
                    tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                        &std::env::temp_dir(),
                        "test",
                    )),
                    hooks: Default::default(),
                    permission_mode: "default".into(),
                    expand_tasks: tokio::sync::watch::channel(false).0,
                    ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
                    instance: None,
                },
            )
            .await
            .unwrap();
        assert!(result.diff.is_some(), "edit should produce a diff");
        let d = result.diff.unwrap();
        assert!(d.contains("-line two"), "diff: {d}");
        assert!(d.contains("+line TWO"), "diff: {d}");
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn write_produces_diff() {
        let path = std::env::temp_dir().join(format!("bingo-write-diff-{}", std::process::id()));
        let result = crate::tool::write::WriteTool
            .call(
                serde_json::json!({
                    "file_path": path.to_string_lossy(),
                    "content": "hello\nworld\n",
                }),
                &ToolContext {
                    cwd: Default::default(),
                    home: std::env::temp_dir(),
                    watch: crate::watch::WatchRegistry::new(),
                    live: Default::default(),
                    http: reqwest::Client::new(),
                    tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(
                        &std::env::temp_dir(),
                        "test",
                    )),
                    hooks: Default::default(),
                    permission_mode: "default".into(),
                    expand_tasks: tokio::sync::watch::channel(false).0,
                    ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
                    instance: None,
                },
            )
            .await
            .unwrap();
        let d = result.diff.unwrap();
        assert!(d.contains("+hello"), "diff: {d}");
        std::fs::remove_file(&path).unwrap();
    }
}

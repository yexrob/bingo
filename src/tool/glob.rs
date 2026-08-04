use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// Glob 结果上限：防模型一次拿到超长列表（超出截断并注明）。
const MAX_GLOB_RESULTS: usize = 500;

#[derive(Debug, Deserialize)]
pub struct GlobInput {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
}

/// Glob：按 pattern 递归列文件（对标 Claude Code GlobTool）。
pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> String {
        "Glob".into()
    }
    fn description(&self) -> String {
        "Find files matching a glob pattern. Returns paths relative to the search root, one per line."
            .into()
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "glob pattern, e.g. **/*.rs"},
                "path": {"type": "string", "description": "directory to search (default: cwd)"}
            },
            "required": ["pattern"]
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
        let params: GlobInput = parse_input(&input)?;
        let root = params
            .path
            .map(PathBuf::from)
            .unwrap_or_else(|| ctx.cwd.clone());
        let matcher = globset::Glob::new(&params.pattern)
            .map_err(|e| ToolError::failed(format!("bad glob pattern: {e}")))?
            .compile_matcher();

        let mut matches = Vec::new();
        collect(&root, &root, &matcher, &mut matches, 0)?;
        matches.sort();
        let total = matches.len();
        matches.truncate(MAX_GLOB_RESULTS);
        let mut text = matches.join("\n");
        if total > matches.len() {
            text.push_str(&format!(
                "\n…{total} matches, showing first {}",
                matches.len()
            ));
        }
        if text.is_empty() {
            text = format!("no files matched {}", params.pattern);
        }
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: None,
        })
    }
}

/// 深度优先收集匹配文件（相对 root 路径）；跳过符号链接目录防循环。
fn collect(
    root: &Path,
    dir: &Path,
    matcher: &globset::GlobMatcher,
    out: &mut Vec<String>,
    depth: u32,
) -> Result<(), ToolError> {
    if depth > 24 {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|e| ToolError::failed(format!("cannot read dir {}: {e}", dir.display())))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| ToolError::failed(format!("cannot stat {}: {e}", path.display())))?;
        if file_type.is_dir() && !file_type.is_symlink() {
            collect(root, &path, matcher, out, depth + 1)?;
        } else if file_type.is_file() && matcher.is_match(&path) {
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            out.push(rel);
        }
    }
    Ok(())
}

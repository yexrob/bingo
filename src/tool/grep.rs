use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// 单文件上限：超过视为二进制/大文件跳过（对标 ripgrep 行为）。
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Grep 结果上限（行数）。
const MAX_GREP_LINES: usize = 200;

#[derive(Debug, Deserialize)]
pub struct GrepInput {
    pub pattern: String,
    #[serde(default)]
    pub path: Option<String>,
    /// 仅搜索匹配该 glob 的文件（如 "*.rs"）。
    #[serde(default)]
    pub glob: Option<String>,
}

/// Grep：正则递归搜索文件内容（对标 Claude Code GrepTool，ripgrep 语义）。
pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> String {
        "Grep".into()
    }
    fn description(&self) -> String {
        "Search file contents with a regular expression. Returns file:line:content matches, one per line."
            .into()
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "regular expression"},
                "path": {"type": "string", "description": "directory to search (default: cwd)"},
                "glob": {"type": "string", "description": "only search files matching this glob"}
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
        let params: GrepInput = parse_input(&input)?;
        let root = params
            .path
            .map(PathBuf::from)
            .unwrap_or_else(|| ctx.cwd.clone());
        let re = regex::Regex::new(&params.pattern)
            .map_err(|e| ToolError::failed(format!("bad regex pattern: {e}")))?;
        let filter = params
            .glob
            .as_deref()
            .map(globset::Glob::new)
            .transpose()
            .map_err(|e| ToolError::failed(format!("bad glob: {e}")))?
            .map(|g| g.compile_matcher());

        let mut lines = Vec::new();
        search_dir(&root, &re, filter.as_ref(), &mut lines, 0)?;
        if lines.is_empty() {
            return Ok(ToolResult {
                content: serde_json::Value::String("no matches".into()),
                is_error: false,
            });
        }
        let total = lines.len();
        lines.truncate(MAX_GREP_LINES);
        let mut text = lines.join("\n");
        if total > lines.len() {
            text.push_str(&format!(
                "\n…{total} matches, showing first {}",
                lines.len()
            ));
        }
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
        })
    }
}

fn search_dir(
    dir: &std::path::Path,
    re: &regex::Regex,
    filter: Option<&globset::GlobMatcher>,
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
            search_dir(&path, re, filter, out, depth + 1)?;
        } else if file_type.is_file()
            && filter.is_none_or(|f| f.is_match(&path))
        {
            search_file(&path, re, out)?;
        }
    }
    Ok(())
}

fn search_file(
    path: &std::path::Path,
    re: &regex::Regex,
    out: &mut Vec<String>,
) -> Result<(), ToolError> {
    let meta = std::fs::metadata(path)
        .map_err(|e| ToolError::failed(format!("cannot stat {}: {e}", path.display())))?;
    if meta.len() > MAX_FILE_BYTES {
        return Ok(());
    }
    let bytes = std::fs::read(path)
        .map_err(|e| ToolError::failed(format!("cannot read {}: {e}", path.display())))?;
    // 二进制检测：NUL 字节 → 跳过。
    if bytes.contains(&0) {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&bytes);
    for (idx, line) in text.lines().enumerate() {
        if re.is_match(line) {
            out.push(format!("{}:{}:{}", path.display(), idx + 1, line));
        }
    }
    Ok(())
}

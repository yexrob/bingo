use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use super::{Tool, ToolContext, ToolError, ToolResult, parse_input};

/// Per-file cap: larger files are treated as binary/too big and skipped (mirrors ripgrep behavior).
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Grep result cap (in lines).
const MAX_GREP_LINES: usize = 200;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GrepInput {
    #[schemars(description = "regular expression")]
    pub pattern: String,
    #[serde(default)]
    #[schemars(description = "directory to search (default: cwd)")]
    pub path: Option<String>,
    /// Only search files matching this glob (e.g. "*.rs").
    #[serde(default)]
    #[schemars(description = "only search files matching this glob")]
    pub glob: Option<String>,
}

/// Grep: recursively search file contents with a regex (ripgrep semantics).
pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> String {
        "Grep".into()
    }
    fn description(&self) -> String {
        "Search file contents with a regular expression. Returns file:line:content matches, one per line. \
         Skips .git, target, node_modules and hidden directories unless the search root is one of them."
            .into()
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<GrepInput>()
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
            .map(super::glob::PathGlob::new)
            .transpose()
            .map_err(|e| ToolError::failed(format!("bad glob: {e}")))?;

        // Traversal + file reads are synchronous IO: run in spawn_blocking, otherwise runtime
        // threads get stuck on large repos (hundreds of thousands of files in target/), freezing
        // the TUI and breaking cancel.
        let search_root = root.clone();
        let (lines, stopped_early) = tokio::task::spawn_blocking(move || {
            let mut lines = Vec::new();
            let stopped = search_dir(
                &search_root,
                &search_root,
                &re,
                filter.as_ref(),
                &mut lines,
                0,
            );
            (lines, stopped)
        })
        .await
        .map_err(|e| ToolError::failed(format!("grep task failed: {e}")))?;

        if lines.is_empty() {
            return Ok(ToolResult {
                content: serde_json::Value::String("no matches".into()),
                is_error: false,
                diff: None,
            });
        }
        let shown = lines.len();
        let mut text = lines.join("\n");
        if stopped_early {
            text.push_str(&format!(
                "\n…stopped at the {shown} match limit; narrow the pattern or path for more"
            ));
        }
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: None,
        })
    }
}

/// Directory names skipped by default: VCS internals, build artifacts, dependency trees.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];

/// Whether to skip this subdirectory during traversal (not consulted when the root itself
/// is explicitly pointed at).
pub fn should_skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name) || name.starts_with('.')
}

/// Read directory entries in sorted order: read_dir order is unspecified; sorting makes
/// truncated results stable.
pub fn sorted_entries(dir: &std::path::Path) -> Vec<std::fs::DirEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    entries
}

/// Returns true when the cap is reached and traversal stops early (no further traversal).
fn search_dir(
    root: &std::path::Path,
    dir: &std::path::Path,
    re: &regex::Regex,
    filter: Option<&super::glob::PathGlob>,
    out: &mut Vec<String>,
    depth: u32,
) -> bool {
    if depth > 24 || out.len() >= MAX_GREP_LINES {
        return out.len() >= MAX_GREP_LINES;
    }
    for entry in sorted_entries(dir) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() && !file_type.is_symlink() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if should_skip_dir(&name) {
                continue;
            }
            if search_dir(root, &path, re, filter, out, depth + 1) {
                return true;
            }
        } else if file_type.is_file()
            && filter.is_none_or(|f| f.is_match(root, &path))
            && search_file(&path, re, out)
        {
            return true;
        }
    }
    false
}

/// Returns true when the result cap is reached.
fn search_file(path: &std::path::Path, re: &regex::Regex, out: &mut Vec<String>) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() > MAX_FILE_BYTES {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    // Binary detection: NUL byte → skip.
    if bytes.contains(&0) {
        return false;
    }
    let text = String::from_utf8_lossy(&bytes);
    for (idx, line) in text.lines().enumerate() {
        if re.is_match(line) {
            out.push(format!("{}:{}:{}", path.display(), idx + 1, line));
            if out.len() >= MAX_GREP_LINES {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(cwd: PathBuf) -> ToolContext {
        ToolContext {
            cwd,
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

    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("bingo-grep-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, body) in [
            ("src/main.rs", "fn main() { needle(); }\n"),
            ("src/deep/lib.rs", "// needle in a subdir\n"),
            ("notes.md", "needle in markdown\n"),
            ("target/debug/build.rs", "needle in build output\n"),
            (".git/config", "needle in git internals\n"),
            ("node_modules/pkg/index.js", "needle in dependency\n"),
        ] {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, body).unwrap();
        }
        root
    }

    /// M1 regression: target/.git/node_modules/hidden directories are not traversed by default.
    #[tokio::test]
    async fn skips_build_and_vcs_directories() {
        let root = fixture("skip");
        let result = GrepTool
            .call(serde_json::json!({"pattern": "needle"}), &ctx(root.clone()))
            .await
            .unwrap();
        let text = result
            .content
            .as_str()
            .unwrap_or_default()
            .replace('\\', "/");
        assert!(text.contains("src/main.rs"), "{text}");
        assert!(text.contains("notes.md"), "{text}");
        assert!(!text.contains("target/"), "target 应跳过: {text}");
        assert!(!text.contains(".git/"), ".git 应跳过: {text}");
        assert!(
            !text.contains("node_modules"),
            "node_modules 应跳过: {text}"
        );
        // Searching works as usual when the root is explicitly pointed at.
        let result = GrepTool
            .call(
                serde_json::json!({
                    "pattern": "needle",
                    "path": root.join("target").to_string_lossy(),
                }),
                &ctx(root.clone()),
            )
            .await
            .unwrap();
        assert!(
            result
                .content
                .as_str()
                .unwrap_or_default()
                .contains("build.rs"),
            "显式指向 target 时不跳过"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// M6 regression: relative globs with a directory prefix once always matched zero because
    /// the whole-string anchored match compared against the absolute path.
    #[tokio::test]
    async fn glob_filter_matches_relative_paths() {
        let root = fixture("glob");
        let matched = |pattern: &str| {
            let root = root.clone();
            let pattern = pattern.to_string();
            async move {
                GrepTool
                    .call(
                        serde_json::json!({"pattern": "needle", "glob": pattern}),
                        &ctx(root),
                    )
                    .await
                    .unwrap()
                    .content
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            }
        };
        let text = matched("src/**/*.rs").await;
        assert!(text.contains("main.rs"), "src/**/*.rs 应命中: {text}");
        assert!(!text.contains("notes.md"), "{text}");
        // A pattern without `/` matches by file name (ripgrep -g semantics), effective at any depth.
        let text = matched("*.rs").await;
        assert!(
            text.contains("main.rs") && text.contains("lib.rs"),
            "{text}"
        );
        assert!(!text.contains("notes.md"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Result cap: stop traversal on reaching it and note it.
    #[tokio::test]
    async fn truncates_at_line_limit() {
        let root = std::env::temp_dir().join(format!("bingo-grep-{}-cap", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let body: String = (0..MAX_GREP_LINES * 3)
            .map(|i| format!("needle {i}\n"))
            .collect();
        std::fs::write(root.join("big.txt"), body).unwrap();
        let result = GrepTool
            .call(serde_json::json!({"pattern": "needle"}), &ctx(root.clone()))
            .await
            .unwrap();
        let text = result.content.as_str().unwrap_or_default();
        assert!(text.contains("stopped at the"), "{text}");
        assert_eq!(
            text.lines().filter(|l| l.contains("big.txt")).count(),
            MAX_GREP_LINES
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

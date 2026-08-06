use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// 单文件上限：超过视为二进制/大文件跳过（对标 ripgrep 行为）。
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Grep 结果上限（行数）。
const MAX_GREP_LINES: usize = 200;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GrepInput {
    #[schemars(description = "regular expression")]
    pub pattern: String,
    #[serde(default)]
    #[schemars(description = "directory to search (default: cwd)")]
    pub path: Option<String>,
    /// 仅搜索匹配该 glob 的文件（如 "*.rs"）。
    #[serde(default)]
    #[schemars(description = "only search files matching this glob")]
    pub glob: Option<String>,
}

/// Grep：正则递归搜索文件内容（ripgrep 语义）。
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

        // 遍历 + 读文件是同步 IO：放进 spawn_blocking，否则运行时线程被
        // 大仓库（target/ 几十万文件）卡死，TUI 冻结、取消失效。
        let search_root = root.clone();
        let (lines, stopped_early) = tokio::task::spawn_blocking(move || {
            let mut lines = Vec::new();
            let stopped = search_dir(&search_root, &search_root, &re, filter.as_ref(), &mut lines, 0);
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

/// 默认跳过的目录名：版本库内部、构建产物、依赖树。
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];

/// 遍历时是否跳过该子目录（根目录本身被显式指向时不经过这里）。
pub fn should_skip_dir(name: &str) -> bool {
    SKIP_DIRS.contains(&name) || name.starts_with('.')
}

/// 目录内条目排序读取：read_dir 顺序不定，排序后截断结果才稳定。
pub fn sorted_entries(dir: &std::path::Path) -> Vec<std::fs::DirEntry> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<std::fs::DirEntry> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    entries
}

/// 返回 true 表示达到上限提前终止（不再遍历）。
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

/// 返回 true 表示已达结果上限。
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
    // 二进制检测：NUL 字节 → 跳过。
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
            watch: crate::watch::WatchRegistry::new(),
            http: reqwest::Client::new(),
            tasks: std::sync::Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "test")),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: std::sync::Arc::new(|_t, _q, _o| Box::pin(async { None })),
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

    /// M1 回归：target/.git/node_modules/隐藏目录默认不遍历。
    #[tokio::test]
    async fn skips_build_and_vcs_directories() {
        let root = fixture("skip");
        let result = GrepTool
            .call(serde_json::json!({"pattern": "needle"}), &ctx(root.clone()))
            .await
            .unwrap();
        let text = result.content.as_str().unwrap_or_default().to_string();
        assert!(text.contains("src/main.rs"), "{text}");
        assert!(text.contains("notes.md"), "{text}");
        assert!(!text.contains("target/"), "target 应跳过: {text}");
        assert!(!text.contains(".git/"), ".git 应跳过: {text}");
        assert!(!text.contains("node_modules"), "node_modules 应跳过: {text}");
        // 根目录被显式指向时照常搜索。
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
            result.content.as_str().unwrap_or_default().contains("build.rs"),
            "显式指向 target 时不跳过"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// M6 回归：带目录前缀的相对 glob 曾因整串锚定比对绝对路径而永远零匹配。
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
        // 无 `/` 的 pattern 按文件名匹配（ripgrep -g 语义），任意深度生效。
        let text = matched("*.rs").await;
        assert!(text.contains("main.rs") && text.contains("lib.rs"), "{text}");
        assert!(!text.contains("notes.md"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 结果上限：达到即停止遍历并注明。
    #[tokio::test]
    async fn truncates_at_line_limit() {
        let root = std::env::temp_dir().join(format!("bingo-grep-{}-cap", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let body: String = (0..MAX_GREP_LINES * 3).map(|i| format!("needle {i}\n")).collect();
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

use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;

use super::{Tool, ToolContext, ToolError, ToolResult, parse_input};

/// Per-file cap: larger files are treated as binary/too big and skipped (mirrors ripgrep behavior).
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// Grep result cap (in lines).
const MAX_GREP_LINES: usize = 200;
const MAX_GREP_CONTEXT: usize = 10_000;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
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
    #[serde(default)]
    #[schemars(description = "include this many lines before and after each match")]
    pub context: Option<usize>,
    #[serde(default)]
    #[schemars(description = "match without regard to letter case")]
    pub case_insensitive: Option<bool>,
    #[serde(default)]
    #[schemars(description = "only match whole words")]
    pub whole_word: Option<bool>,
    #[serde(default)]
    #[schemars(
        description = "treat the pattern as a literal string instead of a regular expression"
    )]
    pub fixed_string: Option<bool>,
    #[serde(default)]
    #[schemars(description = "list matching file paths instead of matching lines")]
    pub files_with_matches: Option<bool>,
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
        let root = params.path.map(PathBuf::from).unwrap_or_default();
        let root = if root.as_os_str().is_empty() {
            ctx.cwd.clone()
        } else if root.is_absolute() {
            root
        } else {
            ctx.cwd.join(root)
        };
        let pattern = if params.fixed_string.unwrap_or(false) {
            regex::escape(&params.pattern)
        } else {
            params.pattern.clone()
        };
        let pattern = if params.whole_word.unwrap_or(false) {
            format!(r"(?:^|\W)(?:{pattern})(?:$|\W)")
        } else {
            pattern
        };
        let re = regex::RegexBuilder::new(&pattern)
            .case_insensitive(params.case_insensitive.unwrap_or(false))
            .build()
            .map_err(|e| ToolError::failed(format!("bad regex pattern: {e}")))?;
        let context = params.context.unwrap_or(0).min(MAX_GREP_CONTEXT);
        let files_with_matches = params.files_with_matches.unwrap_or(false);
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
            let options = SearchOptions {
                re: &re,
                filter: filter.as_ref(),
                context,
                files_with_matches,
            };
            let stopped = search_dir(&search_root, &search_root, &options, &mut lines, 0);
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

struct SearchOptions<'a> {
    re: &'a regex::Regex,
    filter: Option<&'a super::glob::PathGlob>,
    context: usize,
    files_with_matches: bool,
}

/// Returns true when the cap is reached and traversal stops early (no further traversal).
fn search_dir(
    root: &std::path::Path,
    dir: &std::path::Path,
    options: &SearchOptions<'_>,
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
            if search_dir(root, &path, options, out, depth + 1) {
                return true;
            }
        } else if file_type.is_file()
            && options.filter.is_none_or(|f| f.is_match(root, &path))
            && search_file(
                &path,
                options.re,
                options.context,
                options.files_with_matches,
                out,
            )
        {
            return true;
        }
    }
    false
}

/// Returns true when the result cap is reached.
fn search_file(
    path: &std::path::Path,
    re: &regex::Regex,
    context: usize,
    files_with_matches: bool,
    out: &mut Vec<String>,
) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if meta.len() > MAX_FILE_BYTES {
        return false;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    if bytes.contains(&0) {
        return false;
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let matches: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| re.is_match(line).then_some(idx))
        .collect();
    if matches.is_empty() {
        return false;
    }
    if files_with_matches {
        out.push(path.display().to_string());
        return out.len() >= MAX_GREP_LINES;
    }
    let mut emit = vec![false; lines.len()];
    for idx in matches {
        let start = idx.saturating_sub(context);
        let end = idx
            .saturating_add(context)
            .min(lines.len().saturating_sub(1));
        for selected in &mut emit[start..=end] {
            *selected = true;
        }
    }
    for (idx, line) in lines.iter().enumerate() {
        if emit[idx] {
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

    async fn search(root: &std::path::Path, input: serde_json::Value) -> String {
        GrepTool
            .call(input, &ctx(root.to_path_buf()))
            .await
            .unwrap()
            .content
            .as_str()
            .unwrap_or_default()
            .replace('\\', "/")
    }

    #[tokio::test]
    async fn context_returns_surrounding_lines_with_coordinates() {
        let root = fixture("context");
        std::fs::write(root.join("context.txt"), "before\nneedle\nafter\nfar\n").unwrap();
        let text = search(
            &root,
            serde_json::json!({"pattern": "needle", "context": 1}),
        )
        .await;
        assert!(text.contains("context.txt:1:before"), "{text}");
        assert!(text.contains("context.txt:2:needle"), "{text}");
        assert!(text.contains("context.txt:3:after"), "{text}");
        assert!(!text.contains("context.txt:4:far"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn case_insensitive_matches_letter_case() {
        let root = fixture("case");
        std::fs::write(root.join("case.txt"), "Needle\n").unwrap();
        let text = search(
            &root,
            serde_json::json!({"pattern": "needle", "case_insensitive": true}),
        )
        .await;
        assert!(text.contains("case.txt:1:Needle"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn whole_word_rejects_substrings() {
        let root = fixture("word");
        std::fs::write(root.join("word.txt"), "cat\nconcatenate\n").unwrap();
        let text = search(
            &root,
            serde_json::json!({"pattern": "cat", "whole_word": true}),
        )
        .await;
        assert!(text.contains("word.txt:1:cat"), "{text}");
        assert!(!text.contains("concatenate"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn whole_word_allows_longer_alternative_at_same_position() {
        let root = fixture("word-alternation");
        std::fs::write(root.join("alternation.txt"), "foobar\nfoo\n").unwrap();
        let text = search(
            &root,
            serde_json::json!({"pattern": "foo|foobar", "whole_word": true}),
        )
        .await;
        assert!(text.contains("alternation.txt:1:foobar"), "{text}");
        assert!(text.contains("alternation.txt:2:foo"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn whole_word_uses_match_boundaries_for_grouped_regex() {
        let root = fixture("word-group");
        std::fs::write(root.join("group.txt"), "cat\nconcatenate\n(cat)\n").unwrap();
        let text = search(
            &root,
            serde_json::json!({"pattern": "(?:cat)", "whole_word": true}),
        )
        .await;
        assert!(text.contains("group.txt:1:cat"), "{text}");
        assert!(text.contains("group.txt:3:(cat)"), "{text}");
        assert!(!text.contains("concatenate"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn whole_word_matches_punctuation_like_ripgrep() {
        let root = fixture("word-punctuation");
        std::fs::write(
            root.join("punctuation.txt"),
            "foo-\nfoo-x\nxfoo-\n(foo-)\nfoo--\n",
        )
        .unwrap();
        let text = search(
            &root,
            serde_json::json!({
                "pattern": "foo-",
                "whole_word": true,
                "fixed_string": true
            }),
        )
        .await;
        assert!(text.contains("punctuation.txt:1:foo-"), "{text}");
        assert!(text.contains("punctuation.txt:4:(foo-)"), "{text}");
        assert!(text.contains("punctuation.txt:5:foo--"), "{text}");
        assert!(!text.contains("punctuation.txt:2:foo-x"), "{text}");
        assert!(!text.contains("punctuation.txt:3:xfoo-"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn fixed_string_treats_metacharacters_literally() {
        let root = fixture("fixed");
        std::fs::write(root.join("fixed.txt"), "a.b\naxb\n").unwrap();
        let text = search(
            &root,
            serde_json::json!({"pattern": "a.b", "fixed_string": true}),
        )
        .await;
        assert!(text.contains("fixed.txt:1:a.b"), "{text}");
        assert!(!text.contains("axb"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn files_with_matches_lists_each_file_once() {
        let root = fixture("files");
        std::fs::write(root.join("many.txt"), "needle\nneedle\n").unwrap();
        let text = search(
            &root,
            serde_json::json!({"pattern": "needle", "files_with_matches": true}),
        )
        .await;
        assert!(
            text.lines().any(|line| line.ends_with("many.txt")),
            "{text}"
        );
        // Paths only, no `path:line:text` coordinates. Compared below the fixture root so a
        // Windows drive letter ("C:/…") is not read as a coordinate separator.
        let root_prefix = root.display().to_string().replace('\\', "/");
        assert!(
            !text
                .lines()
                .any(|line| line.trim_start_matches(&root_prefix).contains(':')),
            "files only: {text}"
        );
        assert_eq!(
            text.lines()
                .filter(|line| line.ends_with("many.txt"))
                .count(),
            1
        );
        let _ = std::fs::remove_dir_all(&root);
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
        assert!(
            !text.contains("target/"),
            "target should be skipped: {text}"
        );
        assert!(!text.contains(".git/"), ".git should be skipped: {text}");
        assert!(
            !text.contains("node_modules"),
            "node_modules should be skipped: {text}"
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
            "explicitly pointing at target must not skip"
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
        assert!(text.contains("main.rs"), "src/**/*.rs should match: {text}");
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

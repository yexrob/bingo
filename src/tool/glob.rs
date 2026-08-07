use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::Deserialize;

use super::{parse_input, Tool, ToolContext, ToolError, ToolResult};

/// Glob result cap: prevents the model from receiving an overly long list (truncated with a note when exceeded).
const MAX_GLOB_RESULTS: usize = 500;

/// Path glob matcher (shared by the Glob tool and Grep's glob filter).
///
/// globset's matcher anchors the whole string: matching an absolute path against a relative
/// pattern like `src/**/*.rs` always yields zero matches. Rules:
/// - pattern starting with `/` → match against the absolute path;
/// - pattern without `/` → match against the file name (ripgrep `-g` semantics, applies at any depth);
/// - otherwise → match against the path relative to root.
pub struct PathGlob {
    matcher: globset::GlobMatcher,
    absolute: bool,
    name_only: bool,
}

impl PathGlob {
    pub fn new(pattern: &str) -> Result<Self, globset::Error> {
        Ok(Self {
            matcher: globset::Glob::new(pattern)?.compile_matcher(),
            // Absolute on Windows too: `C:\...` patterns are drive-absolute, not relative.
            absolute: pattern.starts_with('/') || Path::new(pattern).is_absolute(),
            name_only: !pattern.contains('/'),
        })
    }

    pub fn is_match(&self, root: &Path, path: &Path) -> bool {
        if self.absolute {
            return self.matcher.is_match(path);
        }
        if self.name_only {
            return path
                .file_name()
                .is_some_and(|name| self.matcher.is_match(Path::new(name)));
        }
        match path.strip_prefix(root) {
            Ok(rel) => self.matcher.is_match(rel),
            Err(_) => self.matcher.is_match(path),
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GlobInput {
    #[schemars(description = "glob pattern, e.g. **/*.rs")]
    pub pattern: String,
    #[serde(default)]
    #[schemars(description = "directory to search (default: cwd)")]
    pub path: Option<String>,
}

/// Glob: recursively list files matching a pattern.
pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> String {
        "Glob".into()
    }
    fn description(&self) -> String {
        "Find files matching a glob pattern. Patterns are matched against paths relative to the \
         search root; a pattern without a slash matches the file name at any depth. Returns paths \
         relative to the search root, one per line. Skips .git, target, node_modules and hidden \
         directories unless the search root is one of them."
            .into()
    }
    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<GlobInput>()
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
        let matcher = PathGlob::new(&params.pattern)
            .map_err(|e| ToolError::failed(format!("bad glob pattern: {e}")))?;

        // The synchronous recursive walk goes into spawn_blocking: it must not block runtime
        // threads on large repos.
        let search_root = root.clone();
        let (mut matches, stopped_early) = tokio::task::spawn_blocking(move || {
            let mut out = Vec::new();
            let stopped = collect(&search_root, &search_root, &matcher, &mut out, 0);
            (out, stopped)
        })
        .await
        .map_err(|e| ToolError::failed(format!("glob task failed: {e}")))?;

        matches.sort();
        let shown = matches.len();
        let mut text = matches.join("\n");
        if stopped_early {
            text.push_str(&format!(
                "\n…stopped at the {shown} result limit; narrow the pattern or path for more"
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

/// Depth-first collection of matching files (paths relative to root); skips symlinked dirs to
/// prevent cycles, and skips .git/target/node_modules/hidden directories. Returns true when the
/// cap is reached and traversal stops early.
fn collect(
    root: &Path,
    dir: &Path,
    matcher: &PathGlob,
    out: &mut Vec<String>,
    depth: u32,
) -> bool {
    if depth > 24 || out.len() >= MAX_GLOB_RESULTS {
        return out.len() >= MAX_GLOB_RESULTS;
    }
    for entry in super::grep::sorted_entries(dir) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() && !file_type.is_symlink() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if super::grep::should_skip_dir(&name) {
                continue;
            }
            if collect(root, &path, matcher, out, depth + 1) {
                return true;
            }
        } else if file_type.is_file() && matcher.is_match(root, &path) {
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            out.push(rel);
            if out.len() >= MAX_GLOB_RESULTS {
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
        }
    }

    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("bingo-glob-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for rel in [
            "src/main.rs",
            "src/deep/lib.rs",
            "notes.md",
            "target/debug/build.rs",
            ".git/config",
            "node_modules/pkg/index.js",
        ] {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, "x").unwrap();
        }
        root
    }

    async fn run(root: &Path, pattern: &str) -> String {
        // Normalize separators so assertions are platform-independent (Windows outputs `\`).
        GlobTool
            .call(
                serde_json::json!({"pattern": pattern}),
                &ctx(root.to_path_buf()),
            )
            .await
            .unwrap()
            .content
            .as_str()
            .unwrap_or_default()
            .replace('\\', "/")
    }

    /// M6 regression: relative patterns with a directory prefix once always matched zero.
    #[tokio::test]
    async fn relative_patterns_match_against_root() {
        let root = fixture("rel");
        let text = run(&root, "src/**/*.rs").await;
        assert!(text.contains("src/main.rs"), "{text}");
        assert!(text.contains("src/deep/lib.rs"), "{text}");
        assert!(!text.contains("notes.md"), "{text}");
        // A pattern without `/` matches by file name, effective at any depth.
        let text = run(&root, "*.md").await;
        assert!(text.contains("notes.md"), "{text}");
        // `**/` prefixes work as usual (file-name assertions: separator style is platform-dependent).
        let text = run(&root, "**/*.rs").await;
        assert!(text.contains("main.rs") && text.contains("lib.rs"), "{text}");
        // Absolute patterns match against the absolute path. Forward slashes throughout:
        // globset treats `\` as an escape character, so a raw Windows path pattern
        // (`C:\...\src/**/*.rs`) would compile to garbage on Windows.
        let absolute = format!(
            "{}/src/**/*.rs",
            root.to_string_lossy().replace('\\', "/")
        );
        let text = run(&root, &absolute).await;
        assert!(text.contains("src/main.rs"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// M1 regression: build artifacts / VCS internals / hidden directories are not traversed by default.
    #[tokio::test]
    async fn skips_build_and_vcs_directories() {
        let root = fixture("skip");
        let text = run(&root, "**/*").await;
        assert!(text.contains("src/main.rs"), "{text}");
        assert!(!text.contains("target/"), "{text}");
        assert!(!text.contains(".git/"), "{text}");
        assert!(!text.contains("node_modules"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn truncates_at_result_limit() {
        let root = std::env::temp_dir().join(format!("bingo-glob-{}-cap", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..(MAX_GLOB_RESULTS + 50) {
            std::fs::write(root.join(format!("f{i:04}.txt")), "x").unwrap();
        }
        let text = run(&root, "*.txt").await;
        assert!(text.contains("stopped at the"), "{}", &text[..200.min(text.len())]);
        assert_eq!(
            text.lines().filter(|l| l.ends_with(".txt")).count(),
            MAX_GLOB_RESULTS
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

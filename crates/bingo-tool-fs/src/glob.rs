//! `Glob`: the files under a root whose path matches a pattern, newest first.
//! The walk obeys `.gitignore` and skips hidden entries, so what comes back is
//! the working tree as a person sees it.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use async_trait::async_trait;
use bingo_sdk::{
    Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use globset::GlobMatcher;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::output;
use crate::path;

/// Beyond this the list is noise; the model gets a note and a narrower pattern
/// to write.
const MAX_RESULTS: usize = 1_000;

const DESCRIPTION: &str = "\
Find files by name. The pattern is matched against each file's path relative \
to the search root, and `*` crosses directory boundaries: `*.rs` finds every \
Rust file at any depth, `src/**/*.ts` only those under `src`. Results are \
absolute paths, most recently modified first. Files ignored by `.gitignore` \
and hidden files are left out. Long results are truncated, and say so on the \
last line.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GlobArgs {
    /// Glob pattern to match against each file's path, e.g. `*.rs` or `src/**/*.ts`.
    pub pattern: String,
    /// Directory to search in. Defaults to the session's working directory.
    pub path: Option<String>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct GlobTool;

impl GlobTool {
    /// The directory a call searches, as the gate and the walk both see it.
    fn root(input: &Value, cwd: &Path) -> Option<PathBuf> {
        let args: GlobArgs = serde_json::from_value(input.clone()).ok()?;
        Some(root(args.path.as_deref(), cwd))
    }
}

fn root(path: Option<&str>, cwd: &Path) -> PathBuf {
    match path {
        Some(path) => path::resolve(path, cwd),
        None => cwd.to_path_buf(),
    }
}

/// Every matching file with the modification time the ordering needs. A file
/// whose metadata cannot be read sorts oldest rather than disappearing.
fn walk(root: &Path, matcher: &GlobMatcher) -> Vec<(SystemTime, PathBuf)> {
    let mut found = Vec::new();
    for entry in path::walker(root).build().flatten() {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if !path::matches(matcher, root, entry.path()) {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|meta| meta.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        found.push((mtime, entry.path().to_path_buf()));
    }
    found
}

/// Newest first; equal timestamps fall back to the path so the list is stable.
fn newest_first(found: &mut [(SystemTime, PathBuf)]) {
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
}

#[async_trait]
impl Tool for GlobTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Glob".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<GlobArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::read_only()
    }

    fn subjects(&self, input: &Value, cwd: &Path) -> Vec<Subject> {
        Self::root(input, cwd)
            .map(|path| vec![Subject::Path { path }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: GlobArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let root = root(args.path.as_deref(), &cx.cwd);
        if !root.is_dir() {
            return Err(ToolError::Failed(format!(
                "not a directory: {}",
                root.display()
            )));
        }
        let matcher = path::matcher(&args.pattern)
            .map_err(|e| ToolError::InvalidInput(format!("bad glob pattern: {e}")))?;

        // The walk is blocking and can be long on a large tree; a runtime
        // thread is not the place for it.
        let mut found = tokio::task::spawn_blocking(move || walk(&root, &matcher))
            .await
            .map_err(|e| ToolError::Failed(format!("the search did not finish: {e}")))?;
        if found.is_empty() {
            return Ok(ToolOutput::text(format!(
                "No files matched {}",
                args.pattern
            )));
        }
        newest_first(&mut found);
        let paths: Vec<String> = found
            .into_iter()
            .map(|(_, path)| path.display().to_string())
            .collect();
        Ok(ToolOutput::text(output::join(&paths, MAX_RESULTS, "files")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{context, write};
    use std::time::Duration;

    /// Give each file a distinct mtime so the ordering assertion means something.
    fn touch(dir: &Path, name: &str, seconds_ago: u64) {
        let path = dir.join(name);
        let when = SystemTime::now() - Duration::from_secs(seconds_ago);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .expect("the fixture exists")
            .set_modified(when)
            .expect("set the modification time");
    }

    #[test]
    fn the_subject_is_the_search_root() {
        let subjects = GlobTool.subjects(
            &serde_json::json!({ "pattern": "*.rs", "path": "src" }),
            Path::new("/work"),
        );
        assert_eq!(
            subjects,
            vec![Subject::Path {
                path: PathBuf::from("/work/src")
            }]
        );
        let default_root = GlobTool.subjects(
            &serde_json::json!({ "pattern": "*.rs" }),
            Path::new("/work"),
        );
        assert_eq!(
            default_root,
            vec![Subject::Path {
                path: PathBuf::from("/work")
            }]
        );
    }

    #[test]
    fn the_spec_advertises_the_argument_schema() {
        let spec = GlobTool.spec();
        assert_eq!(spec.name, "Glob");
        assert!(spec.input_schema["properties"]["pattern"]["description"].is_string());
        assert!(spec.input_schema["properties"]["path"]["description"].is_string());
        let traits = GlobTool.traits(&Value::Null);
        assert!(traits.read_only && traits.concurrency_safe && traits.trusted);
    }

    #[tokio::test]
    async fn matches_are_absolute_paths_newest_first() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join("src")).expect("mkdir");
        write(dir.path(), "old.rs", "");
        write(dir.path(), "src/new.rs", "");
        write(dir.path(), "notes.md", "");
        touch(dir.path(), "old.rs", 600);
        touch(dir.path(), "src/new.rs", 1);
        let cx = context(dir.path());

        let out = GlobTool
            .call(serde_json::json!({ "pattern": "*.rs" }), &cx)
            .await
            .expect("glob");
        let text = out.parts[0].as_text().expect("text").to_string();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "got {text}");
        assert!(lines[0].ends_with("src/new.rs"), "got {text}");
        assert!(lines[1].ends_with("old.rs"), "got {text}");
        assert!(lines[0].starts_with('/'), "got {text}");
    }

    #[tokio::test]
    async fn gitignored_and_hidden_files_are_not_matched() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), ".gitignore", "ignored.rs\n");
        write(dir.path(), "ignored.rs", "");
        write(dir.path(), ".hidden.rs", "");
        write(dir.path(), "kept.rs", "");
        let cx = context(dir.path());

        let out = GlobTool
            .call(serde_json::json!({ "pattern": "*.rs" }), &cx)
            .await
            .expect("glob");
        let text = out.parts[0].as_text().expect("text");
        assert_eq!(text.lines().count(), 1, "got {text}");
        assert!(text.ends_with("kept.rs"), "got {text}");
    }

    #[tokio::test]
    async fn the_path_argument_moves_the_root_and_the_pattern_with_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join("src")).expect("mkdir");
        write(dir.path(), "src/a.rs", "");
        write(dir.path(), "b.rs", "");
        let cx = context(dir.path());

        let out = GlobTool
            .call(serde_json::json!({ "pattern": "*.rs", "path": "src" }), &cx)
            .await
            .expect("glob");
        let text = out.parts[0].as_text().expect("text");
        assert_eq!(text.lines().count(), 1, "got {text}");
        assert!(text.ends_with("src/a.rs"), "got {text}");
    }

    #[tokio::test]
    async fn nothing_matching_says_so_rather_than_returning_nothing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cx = context(dir.path());
        let out = GlobTool
            .call(serde_json::json!({ "pattern": "*.rs" }), &cx)
            .await
            .expect("glob");
        assert_eq!(out.parts[0].as_text(), Some("No files matched *.rs"));
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn a_result_over_the_bounds_is_truncated_and_counts_what_is_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        // Past the entry bound, not merely the character one. What these lines
        // cost in characters is the length of a temporary directory's path,
        // which is a property of the machine and not of the code: macOS hands
        // out `/var/folders/…`, long enough that 600 short names ran past
        // `MAX_CHARS`, while a Linux `/tmp` is short enough that they did not
        // and nothing was truncated at all. Counting past `MAX_RESULTS`
        // truncates the same way everywhere.
        let total = MAX_RESULTS + 200;
        for i in 0..total {
            write(dir.path(), &format!("f{i:04}.rs"), "");
        }
        let cx = context(dir.path());
        let out = GlobTool
            .call(serde_json::json!({ "pattern": "*.rs" }), &cx)
            .await
            .expect("glob");
        let text = out.parts[0].as_text().expect("text");
        let note = text.lines().last().expect("a last line");
        assert!(note.starts_with("[truncated: "), "got {note}");
        assert!(note.ends_with(" more files]"), "got {note}");
        let shown = text.lines().count() - 1;
        assert_eq!(note, format!("[truncated: {} more files]", total - shown));
    }

    #[tokio::test]
    async fn a_broken_pattern_is_invalid_input() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cx = context(dir.path());
        let error = GlobTool
            .call(serde_json::json!({ "pattern": "[" }), &cx)
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::InvalidInput(m)) if m.starts_with("bad glob pattern:")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_root_that_is_not_a_directory_fails_by_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.txt", "");
        let cx = context(dir.path());
        let error = GlobTool
            .call(
                serde_json::json!({ "pattern": "*.rs", "path": "a.txt" }),
                &cx,
            )
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("not a directory:")),
            "got {error:?}"
        );
    }
}

//! `Grep`: ripgrep's engine — `grep-regex` over `grep-searcher` — across the
//! same walk `Glob` uses. Three shapes of answer: which files matched, the
//! lines themselves, or how many per file.

use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bingo_sdk::{
    Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema,
};
use globset::GlobMatcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::types::{Types, TypesBuilder};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::output;
use crate::path;

const DESCRIPTION: &str = "\
Search file contents with a regular expression, on ripgrep's engine. Returns \
the paths of the matching files by default; `output_mode: \"content\"` returns \
the matching lines as `path:line:text` (with `-n`) or `path:text` without it, \
and `output_mode: \"count\"` returns `path:count`. Narrow the files with \
`glob` (matched against the path relative to the search root, so `*.rs` \
matches at any depth) or with `type` (a ripgrep file type such as `rust`, \
`js`, `py`). Files ignored by `.gitignore`, hidden files and binary files are \
skipped. Long results are truncated, and say so on the last line.";

/// What the model gets back.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    /// The path of every file with at least one match.
    #[default]
    FilesWithMatches,
    /// The matching lines themselves.
    Content,
    /// The number of matching lines per file.
    Count,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GrepArgs {
    /// The regular expression to search for.
    pub pattern: String,
    /// File or directory to search in. Defaults to the session's working directory.
    pub path: Option<String>,
    /// Only search files whose path matches this glob, e.g. `*.rs`.
    pub glob: Option<String>,
    /// Only search files of this type, e.g. `rust`, `js`, `py`.
    #[serde(rename = "type")]
    pub file_type: Option<String>,
    /// What to return: the matching file paths, the matching lines, or a count per file.
    #[serde(default)]
    pub output_mode: OutputMode,
    /// Match without regard to case.
    #[serde(rename = "-i")]
    pub case_insensitive: Option<bool>,
    /// Prefix each line with its line number. Content mode only.
    #[serde(rename = "-n")]
    pub line_numbers: Option<bool>,
    /// Lines of context to show after each match. Content mode only.
    #[serde(rename = "-A")]
    pub after_context: Option<usize>,
    /// Lines of context to show before each match. Content mode only.
    #[serde(rename = "-B")]
    pub before_context: Option<usize>,
    /// Lines of context to show on both sides of each match. Content mode only.
    #[serde(rename = "-C")]
    pub context: Option<usize>,
    /// Return at most this many lines, files or counts.
    pub head_limit: Option<usize>,
    /// Let the pattern match across line boundaries.
    pub multiline: Option<bool>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct GrepTool;

impl GrepTool {
    /// The path a call searches, as the gate and the walk both see it.
    fn root(input: &Value, cwd: &Path) -> Option<PathBuf> {
        let args: GrepArgs = serde_json::from_value(input.clone()).ok()?;
        Some(root(args.path.as_deref(), cwd))
    }
}

fn root(path: Option<&str>, cwd: &Path) -> PathBuf {
    match path {
        Some(path) => path::resolve(path, cwd),
        None => cwd.to_path_buf(),
    }
}

/// The pattern as ripgrep's engine sees it. Without multiline, telling the
/// matcher about the line terminator is what lets the searcher work line by
/// line.
fn regex(args: &GrepArgs, multiline: bool) -> Result<RegexMatcher, ToolError> {
    let mut builder = RegexMatcherBuilder::new();
    builder.case_insensitive(args.case_insensitive.unwrap_or(false));
    if multiline {
        builder.dot_matches_new_line(true);
    } else {
        builder.line_terminator(Some(b'\n'));
    }
    builder
        .build(&args.pattern)
        .map_err(|e| ToolError::InvalidInput(format!("bad pattern: {e}")))
}

/// ripgrep's file-type table, narrowed to the one type the call named.
fn file_types(name: Option<&str>) -> Result<Option<Types>, ToolError> {
    let Some(name) = name else {
        return Ok(None);
    };
    let mut builder = TypesBuilder::new();
    builder.add_defaults();
    builder.select(name);
    builder
        .build()
        .map(Some)
        .map_err(|e| ToolError::InvalidInput(format!("bad file type: {e}")))
}

/// Everything the blocking search needs, settled from the arguments once.
struct Search {
    root: PathBuf,
    matcher: RegexMatcher,
    types: Option<Types>,
    glob: Option<GlobMatcher>,
    mode: OutputMode,
    line_numbers: bool,
    before: usize,
    after: usize,
    multiline: bool,
    head_limit: usize,
}

impl Search {
    fn new(args: &GrepArgs, cwd: &Path) -> Result<Self, ToolError> {
        let multiline = args.multiline.unwrap_or(false);
        let content = args.output_mode == OutputMode::Content;
        // Context is a content-mode idea; the other modes report one line per
        // file whatever the call asked for.
        let (before, after) = if content {
            (
                args.before_context.or(args.context).unwrap_or(0),
                args.after_context.or(args.context).unwrap_or(0),
            )
        } else {
            (0, 0)
        };
        Ok(Self {
            root: root(args.path.as_deref(), cwd),
            matcher: regex(args, multiline)?,
            types: file_types(args.file_type.as_deref())?,
            glob: args
                .glob
                .as_deref()
                .map(path::matcher)
                .transpose()
                .map_err(|e| ToolError::InvalidInput(format!("bad glob: {e}")))?,
            mode: args.output_mode,
            line_numbers: content && args.line_numbers.unwrap_or(false),
            before,
            after,
            multiline,
            head_limit: args.head_limit.unwrap_or(usize::MAX),
        })
    }

    fn searcher(&self) -> Searcher {
        let mut builder = SearcherBuilder::new();
        builder
            .binary_detection(BinaryDetection::quit(0))
            .line_number(self.line_numbers)
            .multi_line(self.multiline)
            .before_context(self.before)
            .after_context(self.after);
        builder.build()
    }

    /// Whether a file is in scope: the type filter is the walk's job, the glob
    /// is ours.
    fn included(&self, file: &Path) -> bool {
        self.glob
            .as_ref()
            .is_none_or(|glob| path::matches(glob, &self.root, file))
    }

    /// One file's matches, rendered as the mode asks for them.
    fn search_file(&self, file: &Path, out: &mut Vec<String>) {
        let mut hits = Hits {
            path: file.display().to_string(),
            mode: self.mode,
            line_numbers: self.line_numbers,
            count: 0,
            lines: Vec::new(),
        };
        // An unreadable file is not a match and not an error: the search is
        // over a tree, not over this file.
        if self
            .searcher()
            .search_path(&self.matcher, file, &mut hits)
            .is_err()
        {
            return;
        }
        if hits.count == 0 {
            return;
        }
        match self.mode {
            OutputMode::FilesWithMatches => out.push(hits.path),
            OutputMode::Count => out.push(format!("{}:{}", hits.path, hits.count)),
            OutputMode::Content => out.extend(hits.lines),
        }
    }

    fn run(&self) -> Vec<String> {
        let mut walk = path::walker(&self.root);
        if let Some(types) = &self.types {
            walk.types(types.clone());
        }
        let mut out = Vec::new();
        for entry in walk.build().flatten() {
            if out.len() >= self.head_limit {
                break;
            }
            if !entry.file_type().is_some_and(|kind| kind.is_file()) {
                continue;
            }
            if self.included(entry.path()) {
                self.search_file(entry.path(), &mut out);
            }
        }
        out.truncate(self.head_limit);
        out
    }
}

/// One file's worth of matches, collected straight into the rendered form.
struct Hits {
    path: String,
    mode: OutputMode,
    line_numbers: bool,
    count: usize,
    lines: Vec<String>,
}

impl Hits {
    fn push(&mut self, bytes: &[u8], line: Option<u64>, separator: char) {
        let text = String::from_utf8_lossy(bytes);
        let text = text.trim_end_matches('\n').trim_end_matches('\r');
        let rendered = match line {
            Some(n) if self.line_numbers => {
                format!("{}{separator}{n}{separator}{text}", self.path)
            }
            _ => format!("{}{separator}{text}", self.path),
        };
        self.lines.push(rendered);
    }
}

impl Sink for Hits {
    type Error = io::Error;

    fn matched(&mut self, _searcher: &Searcher, m: &SinkMatch<'_>) -> Result<bool, io::Error> {
        self.count += 1;
        if self.mode == OutputMode::Content {
            let mut line = m.line_number();
            for bytes in m.lines() {
                self.push(bytes, line, ':');
                line = line.map(|n| n + 1);
            }
        }
        // One match settles a file when only its name is wanted.
        Ok(self.mode != OutputMode::FilesWithMatches)
    }

    fn context(&mut self, _searcher: &Searcher, c: &SinkContext<'_>) -> Result<bool, io::Error> {
        if self.mode == OutputMode::Content {
            self.push(c.bytes(), c.line_number(), '-');
        }
        Ok(true)
    }
}

/// What the truncation note counts, in the mode's own noun.
fn noun(mode: OutputMode) -> &'static str {
    match mode {
        OutputMode::Content => "lines",
        OutputMode::FilesWithMatches | OutputMode::Count => "files",
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Grep".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<GrepArgs>(),
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
        let args: GrepArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let search = Search::new(&args, &cx.cwd)?;
        if !search.root.exists() {
            return Err(ToolError::Failed(format!(
                "no such file or directory: {}",
                search.root.display()
            )));
        }
        let mode = search.mode;

        // Walking and reading a tree is blocking work; a runtime thread is not
        // the place for it.
        let found = tokio::task::spawn_blocking(move || search.run())
            .await
            .map_err(|e| ToolError::Failed(format!("the search did not finish: {e}")))?;
        if found.is_empty() {
            return Ok(ToolOutput::text(format!("No matches for {}", args.pattern)));
        }
        Ok(ToolOutput::text(output::join(
            &found,
            usize::MAX,
            noun(mode),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{context, write};

    /// A tree with two source files and a note, one match each.
    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            "a.rs",
            "fn alpha() {}\nlet needle = 1;\nfn omega() {}\n",
        );
        write(dir.path(), "b.rs", "// nothing here\n");
        write(dir.path(), "notes.md", "the NEEDLE again\n");
        dir
    }

    async fn grep(dir: &Path, args: Value) -> String {
        let cx = context(dir);
        let out = GrepTool.call(args, &cx).await.expect("grep");
        out.parts[0].as_text().expect("text").to_string()
    }

    #[test]
    fn the_spec_advertises_the_argument_schema() {
        let spec = GrepTool.spec();
        assert_eq!(spec.name, "Grep");
        let properties = &spec.input_schema["properties"];
        for field in [
            "pattern",
            "path",
            "glob",
            "type",
            "output_mode",
            "-i",
            "-n",
            "-A",
            "head_limit",
        ] {
            assert!(properties[field].is_object(), "missing {field}");
        }
        assert!(properties["pattern"]["description"].is_string());
        assert!(GrepTool.traits(&Value::Null).read_only);
    }

    #[test]
    fn the_subject_is_the_search_root() {
        let subjects = GrepTool.subjects(
            &serde_json::json!({ "pattern": "x", "path": "src" }),
            Path::new("/work"),
        );
        assert_eq!(
            subjects,
            vec![Subject::Path {
                path: PathBuf::from("/work/src")
            }]
        );
    }

    #[tokio::test]
    async fn the_default_mode_names_the_files_that_matched() {
        let dir = tree();
        let text = grep(dir.path(), serde_json::json!({ "pattern": "needle" })).await;
        assert_eq!(text.lines().count(), 1, "got {text}");
        assert!(text.ends_with("a.rs"), "got {text}");
    }

    #[tokio::test]
    async fn content_mode_numbers_its_lines_only_when_asked() {
        let dir = tree();
        let bare = grep(
            dir.path(),
            serde_json::json!({ "pattern": "needle", "output_mode": "content" }),
        )
        .await;
        assert!(bare.ends_with(":let needle = 1;"), "got {bare}");

        let numbered = grep(
            dir.path(),
            serde_json::json!({ "pattern": "needle", "output_mode": "content", "-n": true }),
        )
        .await;
        assert!(numbered.ends_with(":2:let needle = 1;"), "got {numbered}");
    }

    #[tokio::test]
    async fn count_mode_gives_one_number_per_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.rs", "needle\nneedle\nno\n");
        let text = grep(
            dir.path(),
            serde_json::json!({ "pattern": "needle", "output_mode": "count" }),
        )
        .await;
        assert!(text.ends_with("a.rs:2"), "got {text}");
    }

    #[tokio::test]
    async fn case_insensitive_matching_is_opt_in() {
        let dir = tree();
        let sensitive = grep(dir.path(), serde_json::json!({ "pattern": "NEEDLE" })).await;
        assert!(sensitive.ends_with("notes.md"), "got {sensitive}");

        let insensitive = grep(
            dir.path(),
            serde_json::json!({ "pattern": "NEEDLE", "-i": true }),
        )
        .await;
        assert_eq!(insensitive.lines().count(), 2, "got {insensitive}");
    }

    #[tokio::test]
    async fn the_glob_and_type_filters_narrow_the_files_searched() {
        let dir = tree();
        let by_glob = grep(
            dir.path(),
            serde_json::json!({ "pattern": "needle", "-i": true, "glob": "*.md" }),
        )
        .await;
        assert!(by_glob.ends_with("notes.md"), "got {by_glob}");

        let by_type = grep(
            dir.path(),
            serde_json::json!({ "pattern": "needle", "-i": true, "type": "rust" }),
        )
        .await;
        assert!(by_type.ends_with("a.rs"), "got {by_type}");
    }

    #[tokio::test]
    async fn context_lines_come_back_with_a_dash_separator() {
        let dir = tree();
        let text = grep(
            dir.path(),
            serde_json::json!({
                "pattern": "needle", "output_mode": "content", "-n": true, "-C": 1
            }),
        )
        .await;
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "got {text}");
        assert!(lines[0].ends_with("-1-fn alpha() {}"), "got {text}");
        assert!(lines[1].ends_with(":2:let needle = 1;"), "got {text}");
        assert!(lines[2].ends_with("-3-fn omega() {}"), "got {text}");
    }

    #[tokio::test]
    async fn head_limit_caps_the_result() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.rs", "needle\nneedle\nneedle\n");
        let text = grep(
            dir.path(),
            serde_json::json!({
                "pattern": "needle", "output_mode": "content", "head_limit": 2
            }),
        )
        .await;
        assert_eq!(text.lines().count(), 2, "got {text}");
    }

    #[tokio::test]
    async fn multiline_lets_a_pattern_cross_a_line_break() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.rs", "start\nend\n");
        let single = grep(dir.path(), serde_json::json!({ "pattern": "start.end" })).await;
        assert_eq!(single, "No matches for start.end");

        let multi = grep(
            dir.path(),
            serde_json::json!({ "pattern": "start.end", "multiline": true }),
        )
        .await;
        assert!(multi.ends_with("a.rs"), "got {multi}");
    }

    #[tokio::test]
    async fn gitignored_hidden_and_binary_files_are_skipped() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), ".gitignore", "ignored.rs\n");
        write(dir.path(), "ignored.rs", "needle\n");
        write(dir.path(), ".hidden.rs", "needle\n");
        std::fs::write(dir.path().join("blob.bin"), b"needle\x00\x01").expect("write");
        write(dir.path(), "kept.rs", "needle\n");
        let text = grep(dir.path(), serde_json::json!({ "pattern": "needle" })).await;
        assert_eq!(text.lines().count(), 1, "got {text}");
        assert!(text.ends_with("kept.rs"), "got {text}");
    }

    #[tokio::test]
    async fn a_single_file_can_be_the_search_root() {
        let dir = tree();
        let text = grep(
            dir.path(),
            serde_json::json!({ "pattern": "needle", "path": "a.rs" }),
        )
        .await;
        assert_eq!(text.lines().count(), 1, "got {text}");
        assert!(text.ends_with("a.rs"), "got {text}");
    }

    #[tokio::test]
    async fn a_broken_pattern_or_type_is_invalid_input() {
        let dir = tree();
        let cx = context(dir.path());
        let bad_pattern = GrepTool
            .call(serde_json::json!({ "pattern": "a(" }), &cx)
            .await
            .err();
        assert!(
            matches!(&bad_pattern, Some(ToolError::InvalidInput(m)) if m.starts_with("bad pattern:")),
            "got {bad_pattern:?}"
        );
        let bad_type = GrepTool
            .call(
                serde_json::json!({ "pattern": "a", "type": "klingon" }),
                &cx,
            )
            .await
            .err();
        assert!(
            matches!(&bad_type, Some(ToolError::InvalidInput(m)) if m.starts_with("bad file type:")),
            "got {bad_type:?}"
        );
    }

    #[tokio::test]
    async fn a_missing_root_fails_by_name() {
        let dir = tree();
        let cx = context(dir.path());
        let error = GrepTool
            .call(serde_json::json!({ "pattern": "a", "path": "absent" }), &cx)
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("no such file or directory:")),
            "got {error:?}"
        );
    }
}

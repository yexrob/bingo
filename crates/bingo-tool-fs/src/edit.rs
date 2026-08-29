//! `Edit`: one exact string becomes another. The replacement is a pure
//! function of the file's text, so the diff a person approves and the bytes
//! that land are the same decision computed twice.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bingo_sdk::{
    Display, Preview, Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits,
    input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::diff;
use crate::path;

const DESCRIPTION: &str = "\
Replace an exact string in a file. `old_string` must appear in the file and be \
unique — the edit fails otherwise; add the surrounding lines to make it \
unique, or set `replace_all` to replace every occurrence. Prefer this over \
Write for a change to part of an existing file. The file's line endings and \
its trailing newline are left exactly as they are, so `old_string` must match \
the file byte for byte.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EditArgs {
    /// Path of the file to edit.
    pub file_path: String,
    /// The exact text to replace, copied from the file.
    pub old_string: String,
    /// The text to put in its place.
    pub new_string: String,
    /// Replace every occurrence instead of requiring a unique one.
    #[serde(default)]
    pub replace_all: bool,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct EditTool;

impl EditTool {
    /// The path a call edits, as the gate and the writer both see it.
    fn target(input: &Value, cwd: &Path) -> Option<PathBuf> {
        let args: EditArgs = serde_json::from_value(input.clone()).ok()?;
        Some(path::resolve(&args.file_path, cwd))
    }

    /// The file after the edit, and how many occurrences moved.
    fn planned(input: &Value, cwd: &Path) -> Option<(PathBuf, String, String)> {
        let args: EditArgs = serde_json::from_value(input.clone()).ok()?;
        let path = path::resolve(&args.file_path, cwd);
        let content = std::fs::read_to_string(&path).ok()?;
        let (updated, _) = replace(&content, &args).ok()?;
        Some((path, content, updated))
    }
}

/// The one place an edit is decided: the new text and the number of
/// replacements, or the refusal the model gets back.
fn replace(content: &str, args: &EditArgs) -> Result<(String, usize), ToolError> {
    if args.old_string.is_empty() {
        return Err(ToolError::InvalidInput(
            "old_string must not be empty; use Write to create a file".into(),
        ));
    }
    if args.old_string == args.new_string {
        return Err(ToolError::InvalidInput(
            "old_string and new_string are identical; there is nothing to change".into(),
        ));
    }
    let count = content.matches(&args.old_string).count();
    if count == 0 {
        return Err(ToolError::Failed(format!(
            "old_string was not found in {}",
            args.file_path
        )));
    }
    // Refused rather than silently editing the first of several: the caller
    // cannot see which one it would have hit.
    if count > 1 && !args.replace_all {
        return Err(ToolError::Failed(format!(
            "old_string appears {count} times in {}; add surrounding lines to make it unique, or set replace_all",
            args.file_path
        )));
    }
    let updated = if args.replace_all {
        content.replace(&args.old_string, &args.new_string)
    } else {
        content.replacen(&args.old_string, &args.new_string, 1)
    };
    Ok((updated, count))
}

fn summary(path: &Path, count: usize) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!("Edited {}: {count} replacement{plural}", path.display())
}

#[async_trait]
impl Tool for EditTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Edit".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<EditArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::edit()
    }

    fn subjects(&self, input: &Value, cwd: &Path) -> Vec<Subject> {
        Self::target(input, cwd)
            .map(|path| vec![Subject::Path { path }])
            .unwrap_or_default()
    }

    /// No preview for a call that will refuse: there is nothing to approve.
    fn preview(&self, input: &Value, cwd: &Path) -> Option<Preview> {
        let (path, content, updated) = Self::planned(input, cwd)?;
        Some(Preview::Diff {
            unified: diff::unified(&path, &content, &updated),
        })
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: EditArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let path = path::resolve(&args.file_path, &cx.cwd);
        let content = tokio::fs::read_to_string(&path)
            .await
            .map_err(|e| ToolError::Failed(format!("cannot read {}: {e}", path.display())))?;
        let (updated, count) = replace(&content, &args)?;
        let unified = diff::unified(&path, &content, &updated);
        tokio::fs::write(&path, &updated)
            .await
            .map_err(|e| ToolError::Failed(format!("cannot write {}: {e}", path.display())))?;

        let mut out = ToolOutput::text(summary(&path, count));
        out.display = Some(Display::Diff { unified });
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{context, write};

    fn args(file_path: &str, old: &str, new: &str, replace_all: bool) -> Value {
        serde_json::json!({
            "file_path": file_path,
            "old_string": old,
            "new_string": new,
            "replace_all": replace_all,
        })
    }

    #[test]
    fn the_spec_advertises_the_argument_schema() {
        let spec = EditTool.spec();
        assert_eq!(spec.name, "Edit");
        let properties = &spec.input_schema["properties"];
        for field in ["file_path", "old_string", "new_string", "replace_all"] {
            assert!(
                properties[field]["description"].is_string(),
                "missing {field}"
            );
        }
        let traits = EditTool.traits(&Value::Null);
        assert!(traits.edit && traits.trusted && !traits.read_only);
    }

    #[test]
    fn the_subject_is_the_resolved_path() {
        let subjects = EditTool.subjects(&args("a.txt", "a", "b", false), Path::new("/work"));
        assert_eq!(
            subjects,
            vec![Subject::Path {
                path: PathBuf::from("/work/a.txt")
            }]
        );
    }

    #[tokio::test]
    async fn a_unique_string_is_replaced_and_the_result_says_how_many() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.txt", "one\ntwo\nthree\n");
        let cx = context(dir.path());
        let out = EditTool
            .call(args("a.txt", "two", "too", false), &cx)
            .await
            .expect("edit");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).expect("read"),
            "one\ntoo\nthree\n"
        );
        let text = out.parts[0].as_text().expect("text");
        assert!(text.ends_with(": 1 replacement"), "got {text}");
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn the_result_carries_the_diff_the_preview_showed() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.txt", "one\ntwo\n");
        let cx = context(dir.path());
        let input = args("a.txt", "two", "too", false);
        let preview = EditTool.preview(&input, dir.path()).expect("a preview");
        let out = EditTool.call(input, &cx).await.expect("edit");
        let Some(Display::Diff { unified }) = out.display else {
            panic!("expected a diff, got {:?}", out.display);
        };
        assert_eq!(
            preview,
            Preview::Diff {
                unified: unified.clone()
            }
        );
        assert!(unified.contains("-two\n"), "got {unified}");
        assert!(unified.contains("+too\n"), "got {unified}");
    }

    #[tokio::test]
    async fn replace_all_moves_every_occurrence_and_counts_them() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.txt", "x\nx\nx\n");
        let cx = context(dir.path());
        let out = EditTool
            .call(args("a.txt", "x", "y", true), &cx)
            .await
            .expect("edit");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).expect("read"),
            "y\ny\ny\n"
        );
        let text = out.parts[0].as_text().expect("text");
        assert!(text.ends_with(": 3 replacements"), "got {text}");
    }

    #[tokio::test]
    async fn a_string_that_is_not_unique_is_refused_with_its_count() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.txt", "x\nx\n");
        let cx = context(dir.path());
        let input = args("a.txt", "x", "y", false);
        let error = EditTool.call(input.clone(), &cx).await.err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("old_string appears 2 times")),
            "got {error:?}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).expect("read"),
            "x\nx\n",
            "the file must be untouched"
        );
        assert!(EditTool.preview(&input, dir.path()).is_none());
    }

    #[tokio::test]
    async fn a_string_that_is_not_there_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.txt", "one\n");
        let cx = context(dir.path());
        let input = args("a.txt", "absent", "x", false);
        let error = EditTool.call(input.clone(), &cx).await.err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("old_string was not found")),
            "got {error:?}"
        );
        assert!(EditTool.preview(&input, dir.path()).is_none());
    }

    #[tokio::test]
    async fn an_edit_that_changes_nothing_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.txt", "one\n");
        let cx = context(dir.path());
        let identical = EditTool
            .call(args("a.txt", "one", "one", false), &cx)
            .await
            .err();
        assert!(
            matches!(&identical, Some(ToolError::InvalidInput(m)) if m.contains("identical")),
            "got {identical:?}"
        );
        let empty = EditTool
            .call(args("a.txt", "", "new", false), &cx)
            .await
            .err();
        assert!(
            matches!(&empty, Some(ToolError::InvalidInput(m)) if m.contains("must not be empty")),
            "got {empty:?}"
        );
    }

    #[tokio::test]
    async fn line_endings_and_the_trailing_newline_survive_the_edit() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "crlf.txt", "one\r\ntwo\r\n");
        write(dir.path(), "bare.txt", "one\ntwo");
        let cx = context(dir.path());

        EditTool
            .call(args("crlf.txt", "two", "too", false), &cx)
            .await
            .expect("edit");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("crlf.txt")).expect("read"),
            "one\r\ntoo\r\n"
        );

        EditTool
            .call(args("bare.txt", "two", "too", false), &cx)
            .await
            .expect("edit");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("bare.txt")).expect("read"),
            "one\ntoo"
        );
    }

    #[tokio::test]
    async fn a_file_that_cannot_be_read_is_not_edited() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cx = context(dir.path());
        let error = EditTool
            .call(args("absent.txt", "a", "b", false), &cx)
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("cannot read")),
            "got {error:?}"
        );
    }
}

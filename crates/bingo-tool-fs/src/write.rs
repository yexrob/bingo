//! `Write`: a file's whole content, with the directories above it. A file
//! that exists but cannot be read as text is not overwritten: "everything
//! added" is not a diff anyone can approve.

use std::io;
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
Write content to a file, creating any missing parent directories. The content \
replaces the whole file, so use Edit instead for a change to part of an \
existing file. A file that exists but cannot be read back as text is left \
alone rather than overwritten.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteArgs {
    /// Path of the file to write.
    pub file_path: String,
    /// The complete content of the file.
    pub content: String,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct WriteTool;

impl WriteTool {
    /// The path a call writes, as the gate and the writer both see it.
    fn target(input: &Value, cwd: &Path) -> Option<PathBuf> {
        let args: WriteArgs = serde_json::from_value(input.clone()).ok()?;
        Some(path::resolve(&args.file_path, cwd))
    }
}

/// What a read of the target means: nothing there yet, the text about to be
/// replaced, or a refusal. Only "not found" makes a new file — a file that
/// cannot be read is one whose loss nobody could review.
fn existing(path: &Path, read: io::Result<String>) -> Result<Option<String>, ToolError> {
    match read {
        Ok(content) => Ok(Some(content)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ToolError::Failed(format!(
            "refusing to overwrite {}: cannot read the existing file ({e})",
            path.display()
        ))),
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Write".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<WriteArgs>(),
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

    /// A new file has nothing to diff against; a refusal has nothing to approve.
    fn preview(&self, input: &Value, cwd: &Path) -> Option<Preview> {
        let args: WriteArgs = serde_json::from_value(input.clone()).ok()?;
        let path = path::resolve(&args.file_path, cwd);
        let Ok(Some(old)) = existing(&path, std::fs::read_to_string(&path)) else {
            return None;
        };
        Some(Preview::Diff {
            unified: diff::unified(&path, &old, &args.content),
        })
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: WriteArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let path = path::resolve(&args.file_path, &cx.cwd);
        let old = existing(&path, tokio::fs::read_to_string(&path).await)?;

        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await.map_err(|e| {
                ToolError::Failed(format!("cannot create {}: {e}", parent.display()))
            })?;
        }
        tokio::fs::write(&path, &args.content)
            .await
            .map_err(|e| ToolError::Failed(format!("cannot write {}: {e}", path.display())))?;

        let mut out = ToolOutput::text(format!(
            "Wrote {} bytes to {}",
            args.content.len(),
            path.display()
        ));
        if let Some(old) = old {
            out.display = Some(Display::Diff {
                unified: diff::unified(&path, &old, &args.content),
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{context, write};

    fn args(file_path: &str, content: &str) -> Value {
        serde_json::json!({ "file_path": file_path, "content": content })
    }

    #[test]
    fn the_spec_advertises_the_argument_schema() {
        let spec = WriteTool.spec();
        assert_eq!(spec.name, "Write");
        let properties = &spec.input_schema["properties"];
        assert!(properties["file_path"]["description"].is_string());
        assert!(properties["content"]["description"].is_string());
        let traits = WriteTool.traits(&Value::Null);
        assert!(traits.edit && traits.trusted && !traits.read_only);
    }

    #[test]
    fn the_subject_is_the_resolved_path() {
        let subjects = WriteTool.subjects(&args("out/a.txt", "x"), Path::new("/work"));
        assert_eq!(
            subjects,
            vec![Subject::Path {
                path: PathBuf::from("/work/out/a.txt")
            }]
        );
    }

    #[tokio::test]
    async fn a_new_file_brings_its_directories_and_has_nothing_to_diff() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cx = context(dir.path());
        let input = args("deep/inside/a.txt", "hello\n");
        assert!(WriteTool.preview(&input, dir.path()).is_none());

        let out = WriteTool.call(input, &cx).await.expect("write");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("deep/inside/a.txt")).expect("read"),
            "hello\n"
        );
        assert_eq!(
            out.parts[0]
                .as_text()
                .map(|t| t.starts_with("Wrote 6 bytes")),
            Some(true)
        );
        assert_eq!(out.display, None);
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn overwriting_carries_the_diff_the_preview_showed() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.txt", "one\n");
        let cx = context(dir.path());
        let input = args("a.txt", "two\n");
        let preview = WriteTool.preview(&input, dir.path()).expect("a preview");

        let out = WriteTool.call(input, &cx).await.expect("write");
        let Some(Display::Diff { unified }) = out.display else {
            panic!("expected a diff, got {:?}", out.display);
        };
        assert_eq!(
            preview,
            Preview::Diff {
                unified: unified.clone()
            }
        );
        assert!(unified.contains("-one\n"), "got {unified}");
        assert!(unified.contains("+two\n"), "got {unified}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).expect("read"),
            "two\n"
        );
    }

    #[tokio::test]
    async fn a_file_that_cannot_be_read_is_not_overwritten() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("blob.bin"), [0xff, 0xfe, 0x00]).expect("write");
        let cx = context(dir.path());
        let input = args("blob.bin", "text\n");

        let error = WriteTool.call(input.clone(), &cx).await.err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("refusing to overwrite")),
            "got {error:?}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("blob.bin")).expect("read"),
            vec![0xff, 0xfe, 0x00],
            "the file must be untouched"
        );
        assert!(WriteTool.preview(&input, dir.path()).is_none());
    }

    #[tokio::test]
    async fn a_directory_in_the_way_is_a_refusal_not_a_write() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        let cx = context(dir.path());
        let error = WriteTool.call(args("sub", "text\n"), &cx).await.err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("refusing to overwrite")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn arguments_that_do_not_match_the_schema_are_invalid_input() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cx = context(dir.path());
        let error = WriteTool
            .call(serde_json::json!({ "file_path": "a.txt" }), &cx)
            .await
            .err();
        assert!(matches!(error, Some(ToolError::InvalidInput(_))));
    }
}

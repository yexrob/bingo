//! `Read`: one file, numbered like `cat -n`, bounded twice — by the line
//! window the model asks for and by a character cap the tool enforces itself.
//! Images bypass both and travel as an image part.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use base64::Engine;
use bingo_sdk::{
    ContentPart, Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits,
    input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::output;
use crate::path::resolve;

/// Beyond this a read is a mistake, not a request; the model gets the size back.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// Extensions the model can look at, and what to call them on the wire.
const IMAGE_TYPES: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

const DESCRIPTION: &str = "\
Read a file from the filesystem. Give an absolute path, or one relative to the \
session's working directory. Text is returned with line numbers, starting at \
line 1; use `offset` and `limit` to read a window of a long file. Images are \
returned as images. Long results are truncated, and say so on the last line.";

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadArgs {
    /// Path of the file to read.
    pub file_path: String,
    /// First line to return, 1-based. Defaults to the start of the file.
    pub offset: Option<usize>,
    /// How many lines to return. Defaults to the rest of the file.
    pub limit: Option<usize>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ReadTool;

impl ReadTool {
    /// The path a call names, as the gate and the reader both see it.
    fn target(input: &Value, cwd: &Path) -> Option<PathBuf> {
        let args: ReadArgs = serde_json::from_value(input.clone()).ok()?;
        Some(resolve(&args.file_path, cwd))
    }
}

fn media_type(path: &Path) -> Option<&'static str> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    IMAGE_TYPES
        .iter()
        .find(|(name, _)| *name == ext)
        .map(|(_, media)| *media)
}

/// `cat -n` layout: the number right-aligned in six columns, then a tab.
fn numbered(line: &str, n: usize) -> String {
    format!("{n:>6}\t{line}")
}

/// Render the window `offset..offset + limit`, bounded by the shared cap.
fn render(text: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    let first = offset.unwrap_or(1).max(1);
    let window: Vec<String> = text
        .lines()
        .enumerate()
        .skip(first - 1)
        .take(limit.unwrap_or(usize::MAX))
        .map(|(i, line)| numbered(line, i + 1))
        .collect();
    output::join(&window, usize::MAX, "lines")
}

#[async_trait]
impl Tool for ReadTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "Read".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<ReadArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::read_only()
    }

    fn subjects(&self, input: &Value, cwd: &Path) -> Vec<Subject> {
        Self::target(input, cwd)
            .map(|path| vec![Subject::Path { path }])
            .unwrap_or_default()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: ReadArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let path = resolve(&args.file_path, &cx.cwd);
        let shown = path.display().to_string();

        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|_| ToolError::Failed(format!("file not found: {shown}")))?;
        if meta.is_dir() {
            return Err(ToolError::Failed(format!("is a directory: {shown}")));
        }
        if meta.len() > MAX_BYTES {
            return Err(ToolError::Failed(format!(
                "file too large: {} bytes, the limit is {MAX_BYTES}",
                meta.len()
            )));
        }

        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| ToolError::Failed(format!("reading {shown}: {e}")))?;

        if let Some(media_type) = media_type(&path) {
            return Ok(ToolOutput {
                parts: vec![ContentPart::Image {
                    media_type: media_type.into(),
                    data: base64::engine::general_purpose::STANDARD.encode(&bytes),
                }],
                is_error: false,
                display: None,
            });
        }

        let text = String::from_utf8(bytes)
            .map_err(|_| ToolError::Failed(format!("not valid UTF-8: {shown}")))?;
        Ok(ToolOutput::text(render(&text, args.offset, args.limit)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{context, write};

    #[test]
    fn a_relative_path_resolves_against_the_working_directory() {
        let cwd = Path::new("/work");
        assert_eq!(
            resolve("src/lib.rs", cwd),
            PathBuf::from("/work/src/lib.rs")
        );
        assert_eq!(resolve("/etc/hosts", cwd), PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn the_subject_is_the_resolved_path() {
        let subjects = ReadTool.subjects(
            &serde_json::json!({ "file_path": "a.txt" }),
            Path::new("/work"),
        );
        assert_eq!(
            subjects,
            vec![Subject::Path {
                path: PathBuf::from("/work/a.txt")
            }]
        );
    }

    #[test]
    fn the_spec_advertises_the_argument_schema() {
        let spec = ReadTool.spec();
        assert_eq!(spec.name, "Read");
        assert_eq!(spec.input_schema["type"], "object");
        assert!(spec.input_schema["properties"]["file_path"].is_object());
        assert!(spec.input_schema["properties"]["limit"].is_object());
        assert!(ReadTool.traits(&Value::Null).read_only);
        assert!(ReadTool.preview(&Value::Null, Path::new("/")).is_none());
        assert!(ReadTool.confirm(&Value::Null).is_none());
    }

    #[tokio::test]
    async fn a_text_file_comes_back_numbered_like_cat_n() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.txt", "first\nsecond\n");
        let cx = context(dir.path());
        let out = ReadTool
            .call(serde_json::json!({ "file_path": "a.txt" }), &cx)
            .await
            .expect("read");
        assert_eq!(
            out.parts[0].as_text(),
            Some("     1\tfirst\n     2\tsecond")
        );
        assert!(!out.is_error);
    }

    #[tokio::test]
    async fn offset_and_limit_cut_a_window_keeping_the_real_line_numbers() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.txt", "1\n2\n3\n4\n5\n");
        let cx = context(dir.path());
        let out = ReadTool
            .call(
                serde_json::json!({ "file_path": "a.txt", "offset": 2, "limit": 2 }),
                &cx,
            )
            .await
            .expect("read");
        assert_eq!(out.parts[0].as_text(), Some("     2\t2\n     3\t3"));
    }

    #[tokio::test]
    async fn a_long_file_is_truncated_and_says_how_many_lines_are_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let body = (0..5_000)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        write(dir.path(), "a.txt", &body);
        let cx = context(dir.path());
        let out = ReadTool
            .call(serde_json::json!({ "file_path": "a.txt" }), &cx)
            .await
            .expect("read");
        let text = out.parts[0].as_text().expect("text");
        let note = text.lines().last().expect("a last line");
        assert!(note.starts_with("[truncated: "), "got {note}");
        assert!(note.ends_with(" more lines]"));
        let body_chars = text
            .rsplit_once('\n')
            .map(|(head, _)| head.chars().count())
            .unwrap_or(0);
        assert!(
            body_chars <= output::MAX_CHARS,
            "{body_chars} characters kept"
        );
    }

    #[tokio::test]
    async fn a_missing_file_fails_by_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cx = context(dir.path());
        let error = ReadTool
            .call(serde_json::json!({ "file_path": "absent.txt" }), &cx)
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("file not found:")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn a_directory_is_not_a_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        let cx = context(dir.path());
        let error = ReadTool
            .call(serde_json::json!({ "file_path": "sub" }), &cx)
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("is a directory:")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn an_image_comes_back_as_an_image_part() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("pixel.PNG"), [0x89, b'P', b'N', b'G']).expect("write");
        let cx = context(dir.path());
        let out = ReadTool
            .call(serde_json::json!({ "file_path": "pixel.PNG" }), &cx)
            .await
            .expect("read");
        assert_eq!(
            out.parts,
            vec![ContentPart::Image {
                media_type: "image/png".into(),
                data: "iVBORw==".into(),
            }]
        );
    }

    #[tokio::test]
    async fn a_binary_file_that_is_not_an_image_fails_as_invalid_utf8() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("blob.bin"), [0xff, 0xfe, 0x00]).expect("write");
        let cx = context(dir.path());
        let error = ReadTool
            .call(serde_json::json!({ "file_path": "blob.bin" }), &cx)
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("not valid UTF-8:")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn arguments_that_do_not_match_the_schema_are_invalid_input() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cx = context(dir.path());
        let error = ReadTool.call(serde_json::json!({}), &cx).await.err();
        assert!(matches!(error, Some(ToolError::InvalidInput(_))));
    }
}

//! `Read`: one file, numbered like `cat -n`, bounded twice — by the line
//! window the model asks for and by a character cap the tool enforces itself.
//! Images bypass both and travel as an image part, bounded to what a model is
//! sent unless the call asks for the file as it is.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bingo_sdk::{
    ContentPart, Image, Subject, Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits,
    bytes::words, input_schema,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::output;
use crate::path::resolve;

/// Beyond this a read is a mistake, not a request; the model gets the size
/// back. It is the only cap a picture meets here (ADR-0062 §4): what the
/// journal keeps is the bounded rendering, so a photograph this large is read
/// rather than refused.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

/// What the tool tells the model it does. The picture extensions are read off
/// the sdk's table rather than spelled again here: a format added there is in
/// this sentence the same day.
fn description() -> String {
    let (width, height) = bingo_pictures::MODEL_BOX;
    format!(
        "Read a file from the filesystem. Give an absolute path, or one relative to the \
session's working directory. Text is returned with line numbers, starting at line 1; use \
`offset` and `limit` to read a window of a long file. A picture ({}) comes back as the \
picture itself, bounded to what a model is sent (inside {width}×{height} pixels, under {}); \
when that changed it, the result says so and names the file's own size, and `original: true` \
returns the file as it is, up to {}. It is placed in the user's transcript beside this call, \
where their surface can draw it. Long results are truncated, and say so on the last line.",
        extensions(),
        words(bingo_pictures::MODEL_BUDGET),
        words(Image::MAX_BYTES)
    )
}

/// The extensions a picture may arrive under, as the description spells them.
fn extensions() -> String {
    Image::extensions()
        .map(|ext| format!(".{ext}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadArgs {
    /// Path of the file to read.
    pub file_path: String,
    /// First line to return, 1-based. Defaults to the start of the file.
    pub offset: Option<usize>,
    /// How many lines to return. Defaults to the rest of the file.
    pub limit: Option<usize>,
    /// For a picture: `true` returns the file as it is instead of the bounded
    /// rendering; refused above the journal's cap.
    pub original: Option<bool>,
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

/// The file as the model is shown it: bounded to the one box and budget
/// (ADR-0062 §2), and knowing the file it is (ADR-0052 §2). The file's own
/// cap is what refuses a photograph too large to read; what the journal keeps
/// is the bounded rendering, however heavy the file was — so when the
/// rendering is not the file, the result says so and where the file is.
///
/// The bound is a decode, a resize and up to six encodings — hundreds of
/// milliseconds on a photograph — so it runs off the runtime's threads.
async fn bounded(
    media_type: &'static str,
    bytes: Vec<u8>,
    path: &Path,
) -> Result<ToolOutput, ToolError> {
    let shown = path.display().to_string();
    let len = bytes.len();
    let seen = tokio::task::spawn_blocking(move || bingo_pictures::bounded(media_type, &bytes))
        .await
        .map_err(|e| ToolError::Failed(format!("the picture did not finish: {e}")))?
        .map_err(|e| ToolError::Failed(format!("{shown}: {e}")))?;
    let remark = rendered(&seen, media_type, len).then(|| note(&seen, media_type, len));
    let mut parts = vec![ContentPart::Image(seen.at(path))];
    parts.extend(remark.map(ContentPart::text));
    Ok(ToolOutput {
        parts,
        is_error: false,
        display: None,
    })
}

/// Whether what the model was shown is a rendering rather than the file:
/// another format, or another count of bytes. A picture that already fitted
/// the box and the budget came back untouched and needs no words.
fn rendered(seen: &Image, media_type: &str, len: usize) -> bool {
    seen.media_type != media_type || seen.decoded_len() != len
}

/// What the model is told beside a bounded picture: what it is looking at,
/// what the file is, and the one way to ask for the file itself (ADR-0062 §3).
fn note(seen: &Image, media_type: &str, len: usize) -> String {
    format!(
        "[shown bounded: {} {}; the file is {media_type} {}. \
Read it with original: true for the file as it is]",
        seen.media_type,
        words(seen.decoded_len()),
        words(len)
    )
}

/// The file as it is, because the model asked for it: no box and no budget,
/// only the cap on what the journal carries, whose error already names the
/// bytes and the cap.
fn original(media_type: &'static str, bytes: &[u8], path: &Path) -> Result<ToolOutput, ToolError> {
    let shown = path.display().to_string();
    let seen = Image::from_bytes(media_type, bytes)
        .map_err(|e| ToolError::Failed(format!("{shown}: {e}")))?;
    Ok(ToolOutput {
        parts: vec![ContentPart::Image(seen.at(path))],
        is_error: false,
        display: None,
    })
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
            description: description(),
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

        if let Some(media_type) = Image::media_type_of(&path) {
            return match args.original.unwrap_or(false) {
                true => original(media_type, &bytes, &path),
                false => bounded(media_type, bytes, &path).await,
            };
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
        assert!(spec.input_schema["properties"]["original"].is_object());
        assert!(ReadTool.traits(&Value::Null).read_only);
        assert!(ReadTool.preview(&Value::Null, Path::new("/")).is_none());
        assert!(ReadTool.confirm(&Value::Null).is_none());
    }

    /// The one table and this sentence cannot drift: every extension a picture
    /// may arrive under is named, and none of them is a second list.
    #[test]
    fn the_description_names_every_extension_a_picture_may_arrive_under() {
        let description = ReadTool.spec().description;
        for ext in Image::extensions() {
            assert!(
                description.contains(&format!(".{ext}")),
                ".{ext} is missing from {description}"
            );
        }
    }

    /// The bound the description promises is the bound the constants hold: a
    /// box or a budget changed there is changed in this sentence the same day,
    /// and the flag that undoes it is named where the model reads about it.
    #[test]
    fn the_description_names_the_flag_and_the_caps_it_is_bounded_by() {
        let description = ReadTool.spec().description;
        let (width, height) = bingo_pictures::MODEL_BOX;
        for named in [
            "original: true".to_string(),
            format!("{width}×{height} pixels"),
            words(bingo_pictures::MODEL_BUDGET),
            words(Image::MAX_BYTES),
        ] {
            assert!(description.contains(&named), "{named} is missing");
        }
        assert!(description.contains("1.0 MB"), "the budget reads 1.0 MB");
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

    /// The picture the call answers with, or the test's failure.
    async fn read_picture(dir: &Path, name: &str) -> Image {
        let out = ReadTool
            .call(serde_json::json!({ "file_path": name }), &context(dir))
            .await
            .expect("read");
        match out.parts.as_slice() {
            [ContentPart::Image(image)] => image.clone(),
            other => panic!("a picture, not {other:?}"),
        }
    }

    /// A picture a model can read as it is reaches it as the file's own
    /// bytes: nothing is re-encoded and nothing is softened. A screenshot of
    /// two hundred kilobytes is such a picture, not only a tiny one.
    #[tokio::test]
    async fn an_image_comes_back_as_an_image_part() {
        let dir = tempfile::tempdir().expect("temp dir");
        for bytes in [
            bingo_pictures::testing::png_bytes(40, 30),
            bingo_pictures::testing::noise(220, 220),
        ] {
            std::fs::write(dir.path().join("pixel.PNG"), &bytes).expect("write");
            let image = read_picture(dir.path(), "pixel.PNG").await;
            assert_eq!(
                image,
                Image::from_bytes("image/png", &bytes)
                    .expect("within the cap")
                    .at(dir.path().join("pixel.PNG")),
                "{} bytes: and the picture knows the file it is (ADR-0052)",
                bytes.len()
            );
        }
    }

    /// The parts a `Read` answers with, `original` spelled as the model would
    /// spell it.
    async fn read_parts(dir: &Path, name: &str, original: bool) -> Vec<ContentPart> {
        ReadTool
            .call(
                serde_json::json!({ "file_path": name, "original": original }),
                &context(dir),
            )
            .await
            .expect("read")
            .parts
    }

    /// A noisy PNG no budget fits: the file a model is handed is not the file
    /// on disk, so the result says what it is looking at, what the file is,
    /// and the one way to ask for the file itself (ADR-0062 §3).
    #[tokio::test]
    async fn a_bounded_picture_says_what_the_file_is_and_how_to_ask_for_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = bingo_pictures::testing::noise(700, 700);
        assert!(
            bytes.len() > bingo_pictures::MODEL_BUDGET && bytes.len() <= Image::MAX_BYTES,
            "{} bytes: over the budget, under the journal's cap",
            bytes.len()
        );
        std::fs::write(dir.path().join("noisy.png"), &bytes).expect("write");
        let parts = read_parts(dir.path(), "noisy.png", false).await;
        let [ContentPart::Image(image), ContentPart::Text { text }] = parts.as_slice() else {
            panic!("a picture and its words, not {parts:?}");
        };
        assert_eq!(image.media_type, "image/jpeg", "the ladder went lossy");
        assert_eq!(image.path, Some(dir.path().join("noisy.png")));
        assert_eq!(
            text,
            &format!(
                "[shown bounded: image/jpeg {}; the file is image/png {}. \
Read it with original: true for the file as it is]",
                words(image.decoded_len()),
                words(bytes.len())
            )
        );
    }

    /// `original: true` is the way back to the file: the bytes on disk, the
    /// type they are, and no words, because nothing was changed to explain.
    #[tokio::test]
    async fn original_answers_with_the_file_as_it_is() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = bingo_pictures::testing::noise(700, 700);
        std::fs::write(dir.path().join("noisy.png"), &bytes).expect("write");
        let parts = read_parts(dir.path(), "noisy.png", true).await;
        assert_eq!(
            parts,
            vec![ContentPart::Image(
                Image::from_bytes("image/png", &bytes)
                    .expect("within the cap")
                    .at(dir.path().join("noisy.png"))
            )]
        );
    }

    /// A photograph heavier than the wire carries is read and bounded, not
    /// refused: the file's cap is the only one it meets (ADR-0062 §4).
    #[tokio::test]
    async fn a_photograph_over_the_wire_cap_is_read_and_bounded() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = bingo_pictures::testing::noise(1250, 1250);
        assert!(bytes.len() > Image::MAX_BYTES, "{} bytes", bytes.len());
        std::fs::write(dir.path().join("big.png"), &bytes).expect("write");
        let parts = read_parts(dir.path(), "big.png", false).await;
        let [ContentPart::Image(image), ContentPart::Text { .. }] = parts.as_slice() else {
            panic!("a picture and its words, not {parts:?}");
        };
        assert!(
            image.decoded_len() <= bingo_pictures::MODEL_BUDGET,
            "{} bytes reached the model",
            image.decoded_len()
        );
        assert!(Image::is_known(&image.media_type), "{}", image.media_type);
        assert_eq!(image.path, Some(dir.path().join("big.png")));
    }

    /// The way back to the file ends at the journal's own cap: a file the
    /// wire cannot carry is refused by name, and the bounded rendering the
    /// same call without the flag returns is the model's other option.
    #[tokio::test]
    async fn original_above_the_journals_cap_is_refused_by_size() {
        let dir = tempfile::tempdir().expect("temp dir");
        let bytes = bingo_pictures::testing::noise(1250, 1250);
        assert!(
            bytes.len() > Image::MAX_BYTES && bytes.len() as u64 <= MAX_BYTES,
            "{} bytes: over the journal's cap, under the file cap",
            bytes.len()
        );
        std::fs::write(dir.path().join("big.png"), &bytes).expect("write");
        let error = ReadTool
            .call(
                serde_json::json!({ "file_path": "big.png", "original": true }),
                &context(dir.path()),
            )
            .await
            .err();
        let named = format!(
            "image too large: {} bytes, the limit is {}",
            bytes.len(),
            Image::MAX_BYTES
        );
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.ends_with(&named)),
            "got {error:?}"
        );
    }

    /// The flag is a picture's; a text file is read as it always was.
    #[tokio::test]
    async fn original_on_a_text_file_reads_the_text() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(dir.path(), "a.txt", "first\nsecond\n");
        let parts = read_parts(dir.path(), "a.txt", true).await;
        assert_eq!(parts[0].as_text(), Some("     1\tfirst\n     2\tsecond"));
    }

    /// The file cap still refuses a read that is a mistake rather than a
    /// request, whatever the file's name says it is.
    #[tokio::test]
    async fn a_file_over_the_file_cap_fails_by_size() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("huge.png"),
            vec![0u8; MAX_BYTES as usize + 1],
        )
        .expect("write");
        let cx = context(dir.path());
        let error = ReadTool
            .call(serde_json::json!({ "file_path": "huge.png" }), &cx)
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("file too large:")),
            "got {error:?}"
        );
    }

    /// A name is not evidence here either: a `.png` no decoder reads is said
    /// so, rather than journaled for a provider to refuse.
    #[tokio::test]
    async fn a_file_named_a_picture_that_is_not_one_says_so() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("cut.png"), [0x89, b'P', b'N', b'G']).expect("write");
        let cx = context(dir.path());
        let error = ReadTool
            .call(serde_json::json!({ "file_path": "cut.png" }), &cx)
            .await
            .err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.contains("no decoder read this picture")),
            "got {error:?}"
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

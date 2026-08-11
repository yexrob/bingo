use std::path::PathBuf;

use serde::Deserialize;

use async_trait::async_trait;

use super::{Tool, ToolContext, ToolError, ToolResult, parse_input};

/// Max characters per read; anything beyond is truncated.
const MAX_READ_CHARS: usize = 20_000;
/// Byte cap for partial reads: a UTF-8 character is at most 4 bytes, so reading this much
/// is guaranteed to fill MAX_READ_CHARS (the extra bytes leave room for a trailing split character).
const MAX_READ_BYTES: u64 = MAX_READ_CHARS as u64 * 4 + 4;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ReadInput {
    #[schemars(description = "File path to read (absolute or relative)")]
    file_path: String,
    #[serde(default)]
    #[schemars(description = "First line to return, 1-based (default: first line)")]
    start_line: Option<usize>,
    #[serde(default)]
    #[schemars(description = "Last line to return, 1-based and inclusive (default: last line)")]
    end_line: Option<usize>,
}

pub struct ReadTool;

impl ReadTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ReadTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> String {
        "Read".to_string()
    }

    fn description(&self) -> String {
        "Read file content; accepts absolute and relative paths. Image files (png/jpeg/gif/webp) come back as an image you can actually look at, so this is how you inspect a screenshot or a rendered chart."
            .to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<ReadInput>()
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
        let params: ReadInput = parse_input(&input)?;
        let path = PathBuf::from(&params.file_path);
        let path = if path.is_absolute() {
            path
        } else {
            ctx.cwd.join(&path)
        };

        // Image files: hand back a real image block instead of the mojibake that decoding PNG
        // bytes as UTF-8 would produce. This is the only way a session without an input box —
        // a subagent, or the model looking at a screenshot it just took — gets to see one.
        if is_image_path(&path) {
            if params.start_line.is_some() || params.end_line.is_some() {
                return Err(ToolError::failed(
                    "line ranges are only supported for text files",
                ));
            }
            let bytes = tokio::fs::read(&path).await.map_err(|e| {
                ToolError::failed(format!("failed to read {}: {e}", path.display()))
            })?;
            let prepared = crate::api::image::prepare_image(&bytes).ok_or_else(|| {
                ToolError::failed(format!(
                    "{} is not a decodable image (or exceeds the size limit)",
                    path.display()
                ))
            })?;
            return Ok(ToolResult {
                content: crate::api::types::tool_result_blocks(
                    &format!("Image {} ({} bytes on disk)", path.display(), bytes.len()),
                    &[crate::api::types::ImageAttachment {
                        media_type: prepared.media_type,
                        data: prepared.data,
                    }],
                ),
                is_error: false,
                diff: None,
            });
        }

        let text = if params.start_line.is_some() || params.end_line.is_some() {
            read_file_range(&path, params.start_line, params.end_line).await?
        } else {
            // Check the size first: for oversized files only read the needed prefix, instead of
            // loading the whole content into memory and discarding it.
            let size = tokio::fs::metadata(&path)
                .await
                .map_err(|e| ToolError::failed(format!("failed to read {}: {e}", path.display())))?
                .len();
            if size > MAX_READ_BYTES {
                let head = read_prefix(&path).await?;
                let mut text: String = head.chars().take(MAX_READ_CHARS).collect();
                text.push_str(&format!(
                    "\n[Content truncated: file is {size} bytes, showing first {MAX_READ_CHARS} characters]"
                ));
                text
            } else {
                let content = tokio::fs::read_to_string(&path).await.map_err(|e| {
                    ToolError::failed(format!("failed to read {}: {e}", path.display()))
                })?;
                truncate_read_content(content)
            }
        };

        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: None,
        })
    }
}

fn truncate_read_content(content: String) -> String {
    let total = content.chars().count();
    if total > MAX_READ_CHARS {
        let mut text: String = content.chars().take(MAX_READ_CHARS).collect();
        text.push_str(&format!(
            "\n[Content truncated: {total} characters total, showing first {MAX_READ_CHARS}]"
        ));
        text
    } else {
        content
    }
}

fn push_range_text(selected: &mut String, total_chars: &mut usize, text: &str) {
    let count = text.chars().count();
    let remaining = MAX_READ_CHARS.saturating_sub(selected.chars().count());
    selected.extend(text.chars().take(remaining));
    *total_chars = total_chars.saturating_add(count);
}

async fn read_file_range(
    path: &std::path::Path,
    start_line: Option<usize>,
    end_line: Option<usize>,
) -> Result<String, ToolError> {
    use tokio::io::AsyncReadExt;

    let start = start_line.unwrap_or(1);
    let requested_end = end_line.unwrap_or(usize::MAX);
    if start == 0 || requested_end == 0 {
        return Err(ToolError::failed(
            "line numbers are 1-based; start_line and end_line must be at least 1",
        ));
    }
    if start > requested_end {
        return Err(ToolError::failed(format!(
            "invalid line range: start_line {start} is after end_line {requested_end}"
        )));
    }

    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| ToolError::failed(format!("failed to read {}: {e}", path.display())))?;
    let mut buffer = [0u8; 8 * 1024];
    let mut line_number = 1usize;
    let mut selected = String::new();
    let mut total_chars = 0usize;
    let mut saw_any = false;
    let mut reached_requested_end = false;
    let mut ended_with_newline = false;
    let mut utf8_tail = Vec::new();
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|e| ToolError::failed(format!("failed to read {}: {e}", path.display())))?;
        if read == 0 {
            if !utf8_tail.is_empty() {
                return Err(ToolError::failed(format!(
                    "failed to read {}: incomplete UTF-8 sequence at end of file",
                    path.display()
                )));
            }
            break;
        }
        saw_any = true;
        utf8_tail.extend_from_slice(&buffer[..read]);
        let valid_len = match std::str::from_utf8(&utf8_tail) {
            Ok(_) => utf8_tail.len(),
            Err(error) if error.error_len().is_none() => error.valid_up_to(),
            Err(error) => {
                return Err(ToolError::failed(format!(
                    "failed to read {}: invalid UTF-8 at byte {}",
                    path.display(),
                    error.valid_up_to()
                )));
            }
        };
        let text = std::str::from_utf8(&utf8_tail[..valid_len])
            .map_err(|e| ToolError::failed(format!("failed to read {}: {e}", path.display())))?;
        ended_with_newline = text.ends_with('\n');
        let mut segment_start = 0usize;
        for (index, byte) in text.as_bytes().iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }
            if line_number >= start && line_number <= requested_end {
                push_range_text(
                    &mut selected,
                    &mut total_chars,
                    &text[segment_start..=index],
                );
            }
            if line_number >= requested_end {
                reached_requested_end = true;
                break;
            }
            line_number += 1;
            segment_start = index + 1;
        }
        if !reached_requested_end
            && segment_start < text.len()
            && line_number >= start
            && line_number <= requested_end
        {
            push_range_text(&mut selected, &mut total_chars, &text[segment_start..]);
        }
        utf8_tail.drain(..valid_len);
        if reached_requested_end {
            break;
        }
    }

    let total_lines = if !saw_any {
        0
    } else if reached_requested_end {
        requested_end
    } else if ended_with_newline {
        line_number.saturating_sub(1)
    } else {
        line_number
    };
    if start > total_lines {
        return Ok(format!(
            "[Line range out of bounds: file has {total_lines} lines; start_line {start} is past the end]"
        ));
    }
    let shown_end = requested_end.min(total_lines);
    if total_chars > MAX_READ_CHARS {
        selected.push_str(&format!(
            "\n[Content truncated: {total_chars} characters in the requested range, showing first {MAX_READ_CHARS}]"
        ));
    }
    if requested_end != usize::MAX && requested_end > total_lines {
        selected.push_str(&format!(
            "\n[Line range out of bounds: file has {total_lines} lines; showing lines {start}-{shown_end}]"
        ));
    }
    Ok(selected)
}

/// Extension-based image detection: matches what `prepare_image` can decode, and keeps text
/// files off the decode path entirely.
fn is_image_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp"))
}

/// Read only the first MAX_READ_BYTES bytes of the file (the tail may cut through a
/// multibyte character; lossy conversion).
async fn read_prefix(path: &std::path::Path) -> Result<String, ToolError> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| ToolError::failed(format!("failed to read {}: {e}", path.display())))?;
    let mut buf = Vec::with_capacity(MAX_READ_BYTES as usize);
    file.take(MAX_READ_BYTES)
        .read_to_end(&mut buf)
        .await
        .map_err(|e| ToolError::failed(format!("failed to read {}: {e}", path.display())))?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext {
            cwd: std::env::temp_dir(),
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

    async fn read_input(path: &std::path::Path, input: serde_json::Value) -> String {
        let mut input = input;
        input["file_path"] = serde_json::json!(path.to_string_lossy());
        ReadTool::new()
            .call(input, &ctx())
            .await
            .unwrap()
            .content
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    async fn read(path: &std::path::Path) -> String {
        read_input(path, serde_json::json!({})).await
    }

    #[tokio::test]
    async fn returns_inclusive_line_range() {
        let path = std::env::temp_dir().join(format!("bingo-read-range-{}", std::process::id()));
        std::fs::write(
            &path,
            "one
two
three
four
",
        )
        .unwrap();
        let text = read_input(&path, serde_json::json!({"start_line": 2, "end_line": 3})).await;
        assert_eq!(
            text,
            "two
three
"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn start_line_without_end_reads_to_eof() {
        let path =
            std::env::temp_dir().join(format!("bingo-read-start-only-{}", std::process::id()));
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let text = read_input(&path, serde_json::json!({"start_line": 2})).await;
        assert_eq!(text, "two\nthree\n");
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn end_line_without_start_reads_from_first_line() {
        let path = std::env::temp_dir().join(format!("bingo-read-end-only-{}", std::process::id()));
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let text = read_input(&path, serde_json::json!({"end_line": 2})).await;
        assert_eq!(text, "one\ntwo\n");
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn line_range_preserves_crlf_and_unterminated_last_line() {
        let path =
            std::env::temp_dir().join(format!("bingo-read-range-endings-{}", std::process::id()));
        std::fs::write(&path, "one\r\ntwo\r\nthree").unwrap();
        let text = read_input(&path, serde_json::json!({"start_line": 2, "end_line": 3})).await;
        assert_eq!(text, "two\r\nthree");
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn line_range_preserves_utf8_split_across_read_chunks() {
        let path = std::env::temp_dir().join(format!(
            "bingo-read-range-utf8-boundary-{}",
            std::process::id()
        ));
        let body = format!("{}❤\nnext\n", "x".repeat(8191));
        std::fs::write(&path, &body).unwrap();
        let text = read_input(&path, serde_json::json!({"start_line": 1, "end_line": 1})).await;
        assert_eq!(text, format!("{}❤\n", "x".repeat(8191)));
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn huge_single_line_range_is_bounded() {
        let path = std::env::temp_dir().join(format!(
            "bingo-read-range-single-line-{}",
            std::process::id()
        ));
        std::fs::write(&path, "x".repeat(MAX_READ_CHARS * 10)).unwrap();
        let text = read_input(&path, serde_json::json!({"start_line": 1, "end_line": 1})).await;
        assert!(text.contains("[Content truncated:"), "{text}");
        assert!(
            text.chars().count() < MAX_READ_CHARS + 200,
            "{}",
            text.chars().count()
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn capped_range_keeps_out_of_bounds_note() {
        let path = std::env::temp_dir().join(format!(
            "bingo-read-range-capped-oob-{}",
            std::process::id()
        ));
        std::fs::write(&path, format!("{}\nlast", "x".repeat(MAX_READ_CHARS + 100))).unwrap();
        let text = read_input(&path, serde_json::json!({"end_line": 9})).await;
        assert!(text.contains("[Content truncated:"), "{text}");
        assert!(
            text.ends_with("[Line range out of bounds: file has 2 lines; showing lines 1-2]"),
            "{text}"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn line_range_reports_out_of_bounds() {
        let path =
            std::env::temp_dir().join(format!("bingo-read-range-oob-{}", std::process::id()));
        std::fs::write(
            &path,
            "one
two
three
",
        )
        .unwrap();
        let text = read_input(&path, serde_json::json!({"start_line": 2, "end_line": 9})).await;
        assert_eq!(
            text,
            "two
three

[Line range out of bounds: file has 3 lines; showing lines 2-3]"
        );
        std::fs::remove_file(&path).unwrap();
    }

    /// L4: huge files only read the prefix, still truncated correctly by character (multibyte-safe).
    #[tokio::test]
    async fn huge_file_is_partially_read_and_truncated() {
        let path = std::env::temp_dir().join(format!("bingo-read-huge-{}", std::process::id()));
        // 3-byte-per-char non-ASCII, far exceeding MAX_READ_BYTES in total.
        let body = "❤".repeat(MAX_READ_CHARS * 3);
        std::fs::write(&path, &body).unwrap();
        let text = read(&path).await;
        assert!(
            text.contains("[Content truncated: file is"),
            "{}",
            &text[..80]
        );
        let head: String = text.chars().take_while(|c| *c == '❤').collect();
        assert_eq!(head.chars().count(), MAX_READ_CHARS);
        std::fs::remove_file(&path).unwrap();
    }

    /// Image files come back as a real image block, so a session with no input box (a subagent,
    /// or the model inspecting a screenshot it just took) can actually look at one.
    #[tokio::test]
    async fn image_file_returns_an_image_block() {
        let path = std::env::temp_dir().join(format!("bingo-read-img-{}.png", std::process::id()));
        let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([0u8, 128, 255, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(&path, &bytes).unwrap_or_else(|e| panic!("{e}"));

        let result = ReadTool::new()
            .call(
                serde_json::json!({"file_path": path.to_string_lossy()}),
                &ctx(),
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let blocks = result
            .content
            .as_array()
            .unwrap_or_else(|| panic!("image should return a block array, got {}", result.content));
        assert_eq!(blocks.len(), 2, "one caption + one image");
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["source"]["media_type"], "image/png");
        assert!(
            !blocks[1]["source"]["data"]
                .as_str()
                .unwrap_or_default()
                .is_empty()
        );
        // Anywhere that reads rather than transmits gets a size note, never the base64.
        let flat = crate::api::types::tool_result_text(&result.content);
        assert!(flat.contains("[image:"), "{flat}");
        assert!(!flat.contains(blocks[1]["source"]["data"].as_str().unwrap_or("x")));
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("{e}"));
    }

    /// Small files are returned verbatim without a truncation note.
    #[tokio::test]
    async fn small_file_is_returned_verbatim() {
        let path = std::env::temp_dir().join(format!("bingo-read-small-{}", std::process::id()));
        std::fs::write(&path, "hello world\n").unwrap();
        assert_eq!(read(&path).await, "hello world\n");
        std::fs::remove_file(&path).unwrap();
    }
}

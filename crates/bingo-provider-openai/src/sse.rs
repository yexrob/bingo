//! Server-sent-event framing: a byte stream in, `event`/`data` pairs out.
//!
//! Pure and incremental — the parser holds only the unframed tail, so a
//! fixture can drive it one byte at a time and a live body can hand it
//! whatever the socket produced.
//!
//! The same grammar the Messages API speaks, and the same parser as
//! `bingo-provider-anthropic::sse`: a plugin may not import another plugin,
//! so the framing is duplicated until it earns a place in the sdk.

/// Unframed-buffer ceiling: past this the body is judged a protocol error
/// rather than grown until the process runs out of memory.
const MAX_BUFFERED: usize = 8 * 1024 * 1024;

/// A frame boundary is at most four bytes (`\r\n\r\n`), so the rescan rolls
/// back three: a boundary split across two chunks is still found, and the
/// scan stays linear instead of re-reading the whole buffer each round.
const BOUNDARY_OVERLAP: usize = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseFrame {
    pub event: String,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    /// Length of the prefix already known to hold no boundary.
    scanned: usize,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// The frames completed by this chunk. `Err` is a protocol error: the body
    /// grew past [`MAX_BUFFERED`] without ever framing.
    pub fn feed(&mut self, chunk: &[u8]) -> Result<Vec<SseFrame>, String> {
        self.buf.extend_from_slice(chunk);
        let mut frames = Vec::new();
        loop {
            let from = self.scanned.saturating_sub(BOUNDARY_OVERLAP);
            let Some(offset) = find_block_end(&self.buf[from..]) else {
                self.scanned = self.buf.len();
                break;
            };
            let consumed = from + offset + 1;
            let block: Vec<u8> = self.buf.drain(..consumed).collect();
            self.scanned = self.scanned.saturating_sub(consumed);
            frames.extend(parse_block(&block));
        }
        if self.buf.len() > MAX_BUFFERED {
            return Err(format!(
                "sse frame exceeds {MAX_BUFFERED} bytes without a boundary"
            ));
        }
        Ok(frames)
    }

    /// The tail left when the body ends: a server that closes without the
    /// final blank line still gets its last frame read.
    pub fn finish(&mut self) -> Option<SseFrame> {
        let block = std::mem::take(&mut self.buf);
        self.scanned = 0;
        parse_block(&block)
    }
}

/// The index of a boundary's **last** byte (`\n\n` or `\r\n\r\n`).
fn find_block_end(buf: &[u8]) -> Option<usize> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 1);
    let crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 3);
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (found, None) | (None, found) => found,
    }
}

/// One block's `event:` and `data:` lines. Repeated `data:` lines join with a
/// newline, per the SSE grammar; a block with neither is a comment or a
/// keep-alive and yields nothing.
fn parse_block(block: &[u8]) -> Option<SseFrame> {
    let text = String::from_utf8_lossy(block).replace("\r\n", "\n");
    let mut event = None;
    let mut data = String::new();
    for line in text.split('\n') {
        if let Some(value) = line.strip_prefix("event:") {
            event = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            data.push_str(value.trim_start());
            data.push('\n');
        }
    }
    if event.is_none() && data.trim().is_empty() {
        return None;
    }
    Some(SseFrame {
        event: event.unwrap_or_default(),
        data: data.trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frames(chunks: &[&[u8]]) -> Vec<SseFrame> {
        let mut parser = SseParser::new();
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(parser.feed(chunk).expect("framed"));
        }
        out.extend(parser.finish());
        out
    }

    #[test]
    fn one_frame_is_an_event_and_its_data() {
        assert_eq!(
            frames(&[b"event: response.created\ndata: {}\n\n"]),
            vec![SseFrame {
                event: "response.created".into(),
                data: "{}".into()
            }]
        );
    }

    #[test]
    fn a_frame_split_across_chunks_is_reassembled() {
        let out = frames(&[
            b"event: response.out",
            b"put_text.delta\ndata: {\"delta\":",
            b"\"hi\"}\n\nevent: response.completed\ndata: {}\n\n",
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].event, "response.output_text.delta");
        assert_eq!(out[0].data, r#"{"delta":"hi"}"#);
        assert_eq!(out[1].event, "response.completed");
    }

    #[test]
    fn a_boundary_split_across_chunks_is_still_found() {
        let out = frames(&[
            b"event: response.in_progress\ndata: {}\r\n",
            b"\r\nevent: response.completed\ndata: {}\n\n",
        ]);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].event, "response.completed");
    }

    #[test]
    fn crlf_and_repeated_data_lines_join_with_a_newline() {
        let out = frames(&[b"event: x\r\ndata: line1\r\ndata: line2\r\n\r\n"]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, "line1\nline2");
    }

    #[test]
    fn a_body_that_ends_without_a_blank_line_still_yields_its_tail() {
        assert_eq!(
            frames(&[b"event: response.completed\ndata: {}"]),
            vec![SseFrame {
                event: "response.completed".into(),
                data: "{}".into()
            }]
        );
    }

    #[test]
    fn a_comment_only_block_yields_nothing() {
        assert!(frames(&[b": keep-alive\n\n"]).is_empty());
    }

    #[test]
    fn an_unframed_body_is_a_protocol_error_instead_of_unbounded_growth() {
        let mut parser = SseParser::new();
        let chunk = vec![b'x'; 1024 * 1024];
        let mut error = None;
        for _ in 0..=(MAX_BUFFERED / chunk.len()) {
            if let Err(e) = parser.feed(&chunk) {
                error = Some(e);
                break;
            }
        }
        assert!(
            error.is_some_and(|e| e.contains("without a boundary")),
            "the ceiling must report a protocol error"
        );
    }
}

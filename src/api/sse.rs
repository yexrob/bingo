/// Unframed-buffer ceiling: past this it is judged a protocol error, instead
/// of growing unboundedly until OOM.
const MAX_BUFFERED: usize = 8 * 1024 * 1024;

/// A frame boundary is at most 4 bytes (`\r\n\r\n`): the rescan rolls back
/// 3 bytes so a boundary split across chunks is not missed.
const BOUNDARY_OVERLAP: usize = 3;

/// Incremental SSE frame parser: byte stream → complete event blocks
/// (event/data pairs).
pub struct SseParser {
    buf: Vec<u8>,
    /// Length of the prefix confirmed to have no boundary (the rescan starts
    /// from here minus the overlap, avoiding an O(k·n) full rescan).
    scanned: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SseFrame {
    pub event: String,
    pub data: String,
}

impl SseParser {
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(1024), scanned: 0 }
    }

    /// Feed raw bytes; returns the complete frames accumulated this round.
    /// If the buffer exceeds MAX_BUFFERED without framing, that is a
    /// protocol error.
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
            if let Some(frame) = parse_block(&block) {
                frames.push(frame);
            }
        }
        if self.buf.len() > MAX_BUFFERED {
            return Err(format!(
                "sse frame exceeds {MAX_BUFFERED} bytes without a boundary"
            ));
        }
        Ok(frames)
    }
}

/// Locate a frame boundary (`\n\n` or `\r\n\r\n`), returning the index of the
/// boundary's **last** byte.
fn find_block_end(buf: &[u8]) -> Option<usize> {
    let lf_lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| i + 1);
    let crlf_crlf = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 3);
    match (lf_lf, crlf_crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

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
    if data.ends_with('\n') {
        data.pop();
    }
    if event.is_some() || !data.is_empty() {
        Some(SseFrame {
            event: event.unwrap_or_default(),
            data: data.trim().to_string(),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_frame() {
        let mut p = SseParser::new();
        let frames = p.feed(b"event: ping\ndata: {}\n\n").unwrap();
        assert_eq!(
            frames,
            vec![SseFrame { event: "ping".into(), data: "{}".into() }]
        );
    }

    #[test]
    fn parses_frames_across_chunk_boundaries() {
        let mut p = SseParser::new();
        assert!(p.feed(b"event: mess").unwrap().is_empty());
        assert!(p.feed(b"age_start\ndata: {\"id\":").unwrap().is_empty());
        let frames = p
            .feed(b"\"m_1\"}\n\nevent: message_stop\ndata: {}\n\n")
            .unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event, "message_start");
        assert_eq!(frames[0].data, "{\"id\":\"m_1\"}");
        assert_eq!(frames[1].event, "message_stop");
    }

    #[test]
    fn handles_crlf_and_multiline_data() {
        let mut p = SseParser::new();
        let frames = p.feed(b"event: x\r\ndata: line1\r\ndata: line2\r\n\r\n").unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "line1\nline2");
    }

    #[test]
    fn partial_tail_is_kept() {
        let mut p = SseParser::new();
        assert!(p.feed(b"event: ping\nda").unwrap().is_empty());
        let frames = p.feed(b"ta: {}\n\n").unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event, "ping");
    }

    /// LF-LF and CRLF-CRLF share the same index semantics: the boundary is
    /// consumed whole, leaving no residual newline in the buffer.
    #[test]
    fn lf_and_crlf_boundaries_consume_the_whole_separator() {
        for sep in ["\n\n", "\r\n\r\n"] {
            let mut p = SseParser::new();
            let frames = p.feed(format!("event: ping\ndata: {{}}{sep}").as_bytes()).unwrap();
            assert_eq!(frames.len(), 1, "{sep:?}");
            assert!(p.buf.is_empty(), "{sep:?} 残余: {:?}", p.buf);
            assert_eq!(p.scanned, 0, "{sep:?}");
        }
    }

    /// A boundary split across chunks, landing in the rescan overlap region,
    /// is still recognised.
    #[test]
    fn boundary_split_across_chunks_is_found() {
        let mut p = SseParser::new();
        assert!(p.feed(b"event: ping\ndata: {}\r\n").unwrap().is_empty());
        let frames = p.feed(b"\r\nevent: message_stop\ndata: {}\n\n").unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event, "ping");
        assert_eq!(frames[1].event, "message_stop");
    }

    /// A stream that never frames must not grow unboundedly: past the
    /// ceiling it is a protocol error.
    #[test]
    fn unbounded_buffer_is_a_protocol_error() {
        let mut p = SseParser::new();
        let chunk = vec![b'x'; 1024 * 1024];
        let mut err = None;
        for _ in 0..=(MAX_BUFFERED / chunk.len()) {
            if let Err(e) = p.feed(&chunk) {
                err = Some(e);
                break;
            }
        }
        assert!(err.is_some_and(|e| e.contains("without a boundary")), "应报协议错误");
    }
}

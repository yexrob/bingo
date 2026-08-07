/// 未成帧缓冲上限：越过即判定协议错误，而不是无界增长到 OOM。
const MAX_BUFFERED: usize = 8 * 1024 * 1024;

/// 帧边界最长 4 字节（`\r\n\r\n`）：续扫时回退 3 字节，避免漏掉跨 chunk 的边界。
const BOUNDARY_OVERLAP: usize = 3;

/// 增量 SSE 帧解析：字节流 → 完整事件块（event/data 对）。
pub struct SseParser {
    buf: Vec<u8>,
    /// 已确认无边界的前缀长度（下次从这里减去重叠续扫，避免 O(k·n) 全量重扫）。
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

    /// 喂入原始字节，返回本次累积出的完整帧。
    /// 缓冲超过 MAX_BUFFERED 仍未成帧即报协议错误。
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

/// 定位帧边界（`\n\n` 或 `\r\n\r\n`），返回边界**末字节**的索引。
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

    /// LF-LF 与 CRLF-CRLF 的下标语义一致：边界整体消费，缓冲不留残余换行。
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

    /// 边界跨 chunk 落在续扫重叠区内，仍能被认出来。
    #[test]
    fn boundary_split_across_chunks_is_found() {
        let mut p = SseParser::new();
        assert!(p.feed(b"event: ping\ndata: {}\r\n").unwrap().is_empty());
        let frames = p.feed(b"\r\nevent: message_stop\ndata: {}\n\n").unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event, "ping");
        assert_eq!(frames[1].event, "message_stop");
    }

    /// 永不成帧的流不得无界增长：越过上限报协议错误。
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

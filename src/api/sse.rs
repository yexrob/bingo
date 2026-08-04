/// 增量 SSE 帧解析：字节流 → 完整事件块（event/data 对）。
pub struct SseParser {
    buf: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SseFrame {
    pub event: String,
    pub data: String,
}

impl SseParser {
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(1024) }
    }

    /// 喂入原始字节，返回本次累积出的完整帧。
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        self.buf.extend_from_slice(chunk);
        let mut frames = Vec::new();
        while let Some(block_end) = find_block_end(&self.buf) {
            let block: Vec<u8> = self.buf.drain(..=block_end).collect();
            if let Some(frame) = parse_block(&block) {
                frames.push(frame);
            }
        }
        frames
    }
}

/// 定位帧边界（`\n\n` 或 `\r\n\r\n`），返回边界末字节的索引。
fn find_block_end(buf: &[u8]) -> Option<usize> {
    let lf_lf = buf.windows(2).position(|w| w == b"\n\n");
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
        let frames = p.feed(b"event: ping\ndata: {}\n\n");
        assert_eq!(
            frames,
            vec![SseFrame { event: "ping".into(), data: "{}".into() }]
        );
    }

    #[test]
    fn parses_frames_across_chunk_boundaries() {
        let mut p = SseParser::new();
        assert!(p.feed(b"event: mess").is_empty());
        assert!(p.feed(b"age_start\ndata: {\"id\":").is_empty());
        let frames = p.feed(b"\"m_1\"}\n\nevent: message_stop\ndata: {}\n\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].event, "message_start");
        assert_eq!(frames[0].data, "{\"id\":\"m_1\"}");
        assert_eq!(frames[1].event, "message_stop");
    }

    #[test]
    fn handles_crlf_and_multiline_data() {
        let mut p = SseParser::new();
        let frames = p.feed(b"event: x\r\ndata: line1\r\ndata: line2\r\n\r\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "line1\nline2");
    }

    #[test]
    fn partial_tail_is_kept() {
        let mut p = SseParser::new();
        assert!(p.feed(b"event: ping\nda").is_empty());
        let frames = p.feed(b"ta: {}\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event, "ping");
    }
}

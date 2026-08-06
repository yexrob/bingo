//! 显示层的样式化行模型：markdown 渲染与 activities 布局都产出
//! [`Line`]，再由 UI 层映射为 iocraft 元素。与显示库解耦。

use std::borrow::Cow;

use iocraft::prelude::Color;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

/// 一个带样式的文本段。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegStyle {
    /// 前景色。
    pub fg: Option<Color>,
    /// 背景色。
    pub bg: Option<Color>,
    /// 加粗。
    pub bold: bool,
    /// 斜体。
    pub italic: bool,
    /// 下划线。
    pub underline: bool,
    /// 删除线。iocraft 0.8.4 的 `TextDecoration` 只有 None/Underline，
    /// 显示层因此把它退化为 dim（[`crate::tui::theme::Theme::strikethrough`]），
    /// 语义仍保留在模型里：终端库支持后只需改显示层。
    pub strikethrough: bool,
}

impl SegStyle {
    pub const fn plain() -> Self {
        Self {
            fg: None,
            bg: None,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
        }
    }

    /// 叠加另一层样式：`other` 中出现的字段覆盖/启用自身（只加不减，
    /// 与 ratatui `Style::patch` 语义一致）。
    pub fn patch(self, other: SegStyle) -> SegStyle {
        SegStyle {
            fg: other.fg.or(self.fg),
            bg: other.bg.or(self.bg),
            bold: self.bold || other.bold,
            italic: self.italic || other.italic,
            underline: self.underline || other.underline,
            strikethrough: self.strikethrough || other.strikethrough,
        }
    }

    pub fn fg(color: Color) -> Self {
        Self { fg: Some(color), ..SegStyle::plain() }
    }

    /// 设置背景色（链式）。
    pub fn with_bg(mut self, color: Color) -> Self {
        self.bg = Some(color);
        self
    }

    pub fn bold(self) -> Self {
        Self { bold: true, ..self }
    }

    pub fn italic(self) -> Self {
        Self { italic: true, ..self }
    }

    pub fn underline(self) -> Self {
        Self { underline: true, ..self }
    }

    pub fn strikethrough(self) -> Self {
        Self { strikethrough: true, ..self }
    }
}

/// 一行样式化文本（多个段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// 按顺序排布的段。
    pub segs: Vec<Seg>,
    /// 图片块引用：块内每一行都携带（首行 + 后续空行），显示层按 url
    /// 识别块边界：块首行输出 kitty 序列，续行跳过。
    pub image: Option<ImageRef>,
}

/// 图片块引用（指向 [`crate::tui::gfx::ImageMeta`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub url: String,
    pub cols: usize,
    pub rows: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seg {
    pub text: String,
    pub style: SegStyle,
}

impl Line {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            segs: vec![Seg { text: text.into(), style: SegStyle::plain() }],
            image: None,
        }
    }

    pub fn styled(text: impl Into<String>, style: SegStyle) -> Self {
        Self {
            segs: vec![Seg { text: text.into(), style }],
            image: None,
        }
    }

    pub fn empty() -> Self {
        Self { segs: Vec::new(), image: None }
    }

    pub fn is_empty(&self) -> bool {
        self.segs.iter().all(|s| s.text.is_empty())
    }

    /// 整行应用一个样式（叠加，不覆盖已有颜色）。
    pub fn styled_all(mut self, style: SegStyle) -> Self {
        for seg in &mut self.segs {
            seg.style = seg.style.patch(style);
        }
        self
    }

    /// 行首插入一段（活动点前缀等）。
    pub fn prepend(&mut self, seg: Seg) {
        self.segs.insert(0, seg);
    }

    pub fn prepend_styled(&mut self, text: impl Into<String>, style: SegStyle) {
        self.prepend(Seg { text: text.into(), style });
    }

    pub fn push_styled(&mut self, text: impl Into<String>, style: SegStyle) {
        self.segs.push(Seg { text: text.into(), style });
    }

    /// 纯文本内容。
    pub fn plain_text(&self) -> String {
        self.segs.iter().map(|s| s.text.as_str()).collect()
    }

    /// Enforce the one-row invariant in place (see [`sanitize`]).
    pub fn sanitize(&mut self) {
        for seg in &mut self.segs {
            if let Cow::Owned(fixed) = sanitize(&seg.text) {
                seg.text = fixed;
            }
        }
    }
}

/// A [`Line`] must occupy exactly one terminal row. iocraft derives a text
/// node's height by counting `\n`, so an embedded newline silently makes the
/// canvas taller than the row model believes: the inline diff then moves the
/// cursor by the wrong amount and the canvas-height budget under-counts, which
/// is what makes iocraft fall back to `Clear(All)+Purge`.
///
/// Newlines and tabs fold to a single space (tabs also break column
/// accounting, since their display width is not their advance). Remaining C0
/// controls are dropped. ESC is kept so iocraft's own ANSI stripping still
/// sees complete escape sequences.
pub fn sanitize(text: &str) -> Cow<'_, str> {
    if !text.chars().any(|c| c.is_control() && c != '\x1b') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(
        text.chars()
            .filter_map(|c| match c {
                '\n' | '\r' | '\t' => Some(' '),
                '\x1b' => Some(c),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect(),
    )
}

/// Wrap `text` to `width` display columns, breaking at whitespace when
/// possible and hard-breaking words that don't fit on a line of their own
/// (CJK runs carry no spaces, so they always take that path). Embedded
/// newlines start a new output line; leading indentation is preserved,
/// whitespace at a break point is dropped. Always returns at least one line.
pub fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.trim_end_matches('\r');
        let mut line = String::new();
        let mut line_w = 0usize;
        let mut gap = String::new();
        let mut gap_w = 0usize;
        for (is_space, token) in tokens(raw) {
            if is_space {
                gap.push_str(token);
                gap_w += text_width(token);
                continue;
            }
            if line_w > 0 && line_w + gap_w + text_width(token) > width {
                out.push(std::mem::take(&mut line));
                line_w = 0;
            } else {
                line.push_str(&gap);
                line_w += gap_w;
            }
            gap.clear();
            gap_w = 0;
            for ch in token.chars() {
                let w = char_width(ch);
                if line_w > 0 && line_w + w > width {
                    out.push(std::mem::take(&mut line));
                    line_w = 0;
                }
                line.push(ch);
                line_w += w;
            }
        }
        out.push(line);
    }
    out
}

/// Split into alternating whitespace / non-whitespace runs.
fn tokens(s: &str) -> Vec<(bool, &str)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut current: Option<bool> = None;
    for (i, ch) in s.char_indices() {
        let is_space = ch.is_whitespace();
        match current {
            Some(prev) if prev == is_space => {}
            Some(prev) => {
                out.push((prev, &s[start..i]));
                start = i;
            }
            None => start = i,
        }
        current = Some(is_space);
    }
    if let Some(prev) = current {
        out.push((prev, &s[start..]));
    }
    out
}

/// CJK 感知的字符串显示宽度。
pub fn text_width(s: &str) -> usize {
    s.width()
}

/// 单个字符的显示宽度。
pub fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_folds_newlines_and_drops_controls() {
        assert_eq!(sanitize("plain"), "plain");
        assert!(matches!(sanitize("plain"), Cow::Borrowed(_)), "干净文本不分配");
        assert_eq!(sanitize("a\nb\r\nc"), "a b  c");
        assert_eq!(sanitize("a\tb"), "a b");
        assert_eq!(sanitize("a\u{7}b"), "ab");
        // ESC 保留：iocraft 的 strip_ansi 需要看到完整转义序列。
        assert_eq!(sanitize("a\x1b[0mb"), "a\x1b[0mb");
    }

    #[test]
    fn line_sanitize_enforces_single_row() {
        let mut line = Line::plain("multi\nline");
        line.push_styled("tail\nmore", SegStyle::plain());
        line.sanitize();
        assert!(!line.plain_text().contains('\n'));
    }

    #[test]
    fn wrap_words_breaks_on_whitespace_and_newlines() {
        assert_eq!(wrap_words("hello world", 20), vec!["hello world"]);
        assert_eq!(wrap_words("hello world", 7), vec!["hello", "world"]);
        // 显式换行开新行，空行保留。
        assert_eq!(wrap_words("a\n\nb", 10), vec!["a", "", "b"]);
        // 行首缩进保留。
        assert_eq!(wrap_words("  indented", 20), vec!["  indented"]);
        // 空输入仍有一行（调用方按行渲染）。
        assert_eq!(wrap_words("", 10), vec![""]);
    }

    #[test]
    fn wrap_words_hard_breaks_long_runs() {
        // 超长词（无空白可断）按宽度硬断。
        assert_eq!(wrap_words("aaaaaaaa", 3), vec!["aaa", "aaa", "aa"]);
        // CJK 按显示宽度（每字 2 列）断行。
        assert_eq!(wrap_words("中文换行测试", 4), vec!["中文", "换行", "测试"]);
    }

    #[test]
    fn wrap_words_never_exceeds_width() {
        let text = "混合 text with 中文 and a verylongtokenwithoutspaces 结尾";
        for width in 3..20 {
            for line in wrap_words(text, width) {
                assert!(text_width(&line) <= width, "width={width} line={line:?}");
            }
        }
    }
}

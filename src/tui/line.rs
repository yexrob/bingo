//! The styled line model of the display layer: both markdown rendering and
//! activities layout produce [`Line`]s, which [`crate::tui::view`] maps to
//! terminal rows. Decoupled from any display library.

use std::borrow::Cow;

use ratatui::style::Color;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

/// One styled text segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegStyle {
    /// Foreground colour.
    pub fg: Option<Color>,
    /// Background colour.
    pub bg: Option<Color>,
    /// Bold.
    pub bold: bool,
    /// Italic.
    pub italic: bool,
    /// Underline.
    pub underline: bool,
    /// Strikethrough (rendered as `Modifier::CROSSED_OUT`).
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

    /// Overlay another style layer: fields set in `other` override or enable
    /// ours (additive only, matching ratatui `Style::patch` semantics).
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
        Self {
            fg: Some(color),
            ..SegStyle::plain()
        }
    }

    /// Set the background colour (chainable).
    pub fn with_bg(mut self, color: Color) -> Self {
        self.bg = Some(color);
        self
    }

    pub fn bold(self) -> Self {
        Self { bold: true, ..self }
    }

    pub fn italic(self) -> Self {
        Self {
            italic: true,
            ..self
        }
    }

    pub fn underline(self) -> Self {
        Self {
            underline: true,
            ..self
        }
    }

    pub fn strikethrough(self) -> Self {
        Self {
            strikethrough: true,
            ..self
        }
    }
}

/// One line of styled text (several segments).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The segments, in order.
    pub segs: Vec<Seg>,
    /// Image block reference: every row of the block carries it, with `row`
    /// naming its position inside the block. The display layer renders each
    /// row as kitty Unicode placeholder cells; the transmit layer sends the
    /// image data once per image id.
    pub image: Option<ImageRef>,
}

/// Image block reference (points at [`crate::ui::ImageMeta`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRef {
    pub url: String,
    pub cols: usize,
    pub rows: usize,
    /// 0-based row of this line within the image block.
    pub row: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seg {
    pub text: String,
    pub style: SegStyle,
}

impl Line {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            segs: vec![Seg {
                text: text.into(),
                style: SegStyle::plain(),
            }],
            image: None,
        }
    }

    pub fn styled(text: impl Into<String>, style: SegStyle) -> Self {
        Self {
            segs: vec![Seg {
                text: text.into(),
                style,
            }],
            image: None,
        }
    }

    pub fn empty() -> Self {
        Self {
            segs: Vec::new(),
            image: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.segs.iter().all(|s| s.text.is_empty())
    }

    /// Apply one style to the whole line (patched on; existing colours are kept).
    pub fn styled_all(mut self, style: SegStyle) -> Self {
        for seg in &mut self.segs {
            seg.style = seg.style.patch(style);
        }
        self
    }

    /// Insert a segment at the start of the line (e.g. the activity-point prefix).
    pub fn prepend(&mut self, seg: Seg) {
        self.segs.insert(0, seg);
    }

    pub fn prepend_styled(&mut self, text: impl Into<String>, style: SegStyle) {
        self.prepend(Seg {
            text: text.into(),
            style,
        });
    }

    pub fn push_styled(&mut self, text: impl Into<String>, style: SegStyle) {
        self.segs.push(Seg {
            text: text.into(),
            style,
        });
    }

    /// The plain text content.
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

/// A [`Line`] must occupy exactly one terminal row: the viewport height is the
/// row count, and scrollback rows are written one per line, so an embedded
/// newline would desync both.
///
/// Newlines and tabs fold to a single space (tabs also break column
/// accounting, since their display width is not their advance). Remaining C0
/// controls are dropped. ESC is kept so a segment that already carries a
/// complete escape sequence stays intact.
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

/// CJK-aware display width of a string.
pub fn text_width(s: &str) -> usize {
    s.width()
}

/// Display width of a single character.
pub fn char_width(c: char) -> usize {
    c.width().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_folds_newlines_and_drops_controls() {
        assert_eq!(sanitize("plain"), "plain");
        assert!(
            matches!(sanitize("plain"), Cow::Borrowed(_)),
            "clean text is not allocated"
        );
        assert_eq!(sanitize("a\nb\r\nc"), "a b  c");
        assert_eq!(sanitize("a\tb"), "a b");
        assert_eq!(sanitize("a\u{7}b"), "ab");
        // ESC kept: a segment carrying a complete escape sequence stays intact.
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
        // Explicit newlines start new lines; blank lines are kept.
        assert_eq!(wrap_words("a\n\nb", 10), vec!["a", "", "b"]);
        // Leading indentation is kept.
        assert_eq!(wrap_words("  indented", 20), vec!["  indented"]);
        // Empty input still yields one line (the caller renders per line).
        assert_eq!(wrap_words("", 10), vec![""]);
    }

    #[test]
    fn wrap_words_hard_breaks_long_runs() {
        // Over-long words (no whitespace to break at) hard-break by width.
        assert_eq!(wrap_words("aaaaaaaa", 3), vec!["aaa", "aaa", "aa"]);
        // Wide glyphs break by display width (2 columns per glyph).
        assert_eq!(wrap_words("ＡＢＣＤＥＦ", 4), vec!["ＡＢ", "ＣＤ", "ＥＦ"]);
    }

    #[test]
    fn wrap_words_never_exceeds_width() {
        let text = "mixed text with ＡＢ and a verylongtokenwithoutspaces end";
        for width in 3..20 {
            for line in wrap_words(text, width) {
                assert!(text_width(&line) <= width, "width={width} line={line:?}");
            }
        }
    }
}

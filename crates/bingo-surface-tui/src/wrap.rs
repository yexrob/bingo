//! Word wrapping for styled lines. The transcript is measured, not predicted:
//! every view that needs to know how tall a block is wraps it first and counts.

use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Prose is read, not scanned: however wide the terminal is, a line of it
/// stops here (design §7).
pub const MEASURE: usize = 100;

/// The width prose is wrapped to inside a region `width` columns wide.
pub fn measure(width: usize) -> usize {
    width.min(MEASURE)
}

/// Wrap one styled line to `width` columns, keeping each span's style. An empty
/// line stays one empty line, so blank separators survive.
pub fn wrap(line: &Line<'static>, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![line.clone()];
    }
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for span in &line.spans {
        for token in tokens(&span.content) {
            place(token, span, width, &mut out, &mut current, &mut used);
        }
    }
    out.push(Line::from(current));
    out
}

/// Wrap every line of a block and flatten the result.
pub fn wrap_all(lines: &[Line<'static>], width: usize) -> Vec<Line<'static>> {
    lines.iter().flat_map(|l| wrap(l, width)).collect()
}

/// Put one token on the current line, breaking to the next when it will not fit.
fn place(
    token: &str,
    span: &Span<'static>,
    width: usize,
    out: &mut Vec<Line<'static>>,
    current: &mut Vec<Span<'static>>,
    used: &mut usize,
) {
    let token_width = token.width();
    if token.chars().all(char::is_whitespace) {
        // Whitespace is dropped where a break put it, but the line's own
        // leading indent — a result gutter, a list marker — is content.
        if *used == 0 && !out.is_empty() {
            return;
        }
        if *used + token_width <= width {
            push(current, token, span);
            *used += token_width;
        } else {
            break_line(out, current, used);
        }
        return;
    }
    if *used + token_width > width && *used > 0 {
        break_line(out, current, used);
    }
    if token_width <= width {
        push(current, token, span);
        *used += token_width;
        return;
    }
    for piece in split_wide(token, width) {
        if *used + piece.width() > width && *used > 0 {
            break_line(out, current, used);
        }
        push(current, &piece, span);
        *used += piece.width();
    }
}

fn push(current: &mut Vec<Span<'static>>, text: &str, span: &Span<'static>) {
    current.push(Span::styled(text.to_string(), span.style));
}

/// Break here, leaving no trailing whitespace at the end of the row.
fn break_line(out: &mut Vec<Line<'static>>, current: &mut Vec<Span<'static>>, used: &mut usize) {
    while current
        .last()
        .is_some_and(|s| s.content.chars().all(char::is_whitespace))
    {
        current.pop();
    }
    out.push(Line::from(std::mem::take(current)));
    *used = 0;
}

/// A word longer than the whole line is cut on grapheme boundaries.
fn split_wide(token: &str, width: usize) -> Vec<String> {
    let mut pieces = Vec::new();
    let mut piece = String::new();
    let mut used = 0usize;
    for grapheme in token.graphemes(true) {
        let w = grapheme.width();
        if used + w > width && !piece.is_empty() {
            pieces.push(std::mem::take(&mut piece));
            used = 0;
        }
        piece.push_str(grapheme);
        used += w;
    }
    if !piece.is_empty() {
        pieces.push(piece);
    }
    pieces
}

/// Split into alternating runs of whitespace and non-whitespace.
fn tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut space: Option<bool> = None;
    for (i, c) in text.char_indices() {
        let is_space = c.is_whitespace();
        match space {
            Some(previous) if previous != is_space => {
                out.push(&text[start..i]);
                start = i;
            }
            _ => {}
        }
        space = Some(is_space);
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    fn text(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|l| l.to_string()).collect()
    }

    #[test]
    fn words_break_at_spaces_and_drop_the_space_at_the_break() {
        let line = Line::from("the quick brown fox jumps");
        assert_eq!(
            text(&wrap(&line, 10)),
            vec!["the quick", "brown fox", "jumps"]
        );
    }

    #[test]
    fn a_word_wider_than_the_line_is_cut() {
        let line = Line::from("abcdefghijkl");
        assert_eq!(text(&wrap(&line, 5)), vec!["abcde", "fghij", "kl"]);
    }

    #[test]
    fn styles_survive_the_break() {
        let line = Line::from(vec![
            Span::styled("hello ".to_string(), theme::bold()),
            Span::styled("world".to_string(), theme::dim()),
        ]);
        let wrapped = wrap(&line, 6);
        assert_eq!(text(&wrapped), vec!["hello", "world"]);
        assert_eq!(wrapped[0].spans[0].style, theme::bold());
        assert_eq!(wrapped[1].spans[0].style, theme::dim());
    }

    #[test]
    fn an_empty_line_stays_one_line() {
        assert_eq!(wrap(&Line::from(""), 10).len(), 1);
    }

    #[test]
    fn wide_glyphs_count_two_columns() {
        let line = Line::from("你好世界");
        assert_eq!(text(&wrap(&line, 4)), vec!["你好", "世界"]);
    }
}

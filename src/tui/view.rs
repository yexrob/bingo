//! Pure conversion layer: the renderer-agnostic row model
//! ([`crate::tui::line::Line`] / [`Row`]) → ratatui text.
//!
//! Nothing here touches the terminal: the app hands the output either to
//! [`crate::tui::term::InlineTerm::insert_history`] (scrollback) or to a
//! [`Buffer`] (viewport). Keeping it pure is what makes the row model
//! testable without a terminal.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TextLine, Span};

use crate::tui::chat::Row;
use crate::tui::line::{Line, SegStyle};

/// Placeholder drawn in the viewport for an image block row. The real kitty
/// bytes only go out when the block is flushed into scrollback, so the live
/// tail shows the same text the old canvas did.
pub const IMAGE_PLACEHOLDER: &str = "#[image]";

/// [`SegStyle`] → ratatui [`Style`]. `default_fg` applies to segments that
/// carry no colour of their own (the document model leaves body text
/// uncoloured and lets the theme decide).
pub fn style_of(style: SegStyle, default_fg: Color) -> Style {
    let mut out = Style::default().fg(style.fg.unwrap_or(default_fg));
    if let Some(bg) = style.bg {
        out = out.bg(bg);
    }
    let mut modifier = Modifier::empty();
    if style.bold {
        modifier |= Modifier::BOLD;
    }
    if style.italic {
        modifier |= Modifier::ITALIC;
    }
    if style.underline {
        modifier |= Modifier::UNDERLINED;
    }
    if style.strikethrough {
        modifier |= Modifier::CROSSED_OUT;
    }
    if modifier.is_empty() {
        out
    } else {
        out.add_modifier(modifier)
    }
}

/// One document line → one ratatui line. Image rows render as the literal
/// [`IMAGE_PLACEHOLDER`].
pub fn to_line(line: &Line, default_fg: Color) -> TextLine<'static> {
    if line.image.is_some() {
        return TextLine::from(Span::styled(
            IMAGE_PLACEHOLDER,
            Style::default().fg(default_fg),
        ));
    }
    TextLine::from(
        line.segs
            .iter()
            .map(|seg| Span::styled(seg.text.clone(), style_of(seg.style, default_fg)))
            .collect::<Vec<_>>(),
    )
}

/// One document row → one scrollback line. Rows with a full-row background
/// (the user message bubble) are padded out to `width` so the bubble spans the
/// terminal instead of shrinking to its text once it lands in scrollback.
pub fn history_line(row: &Row, default_fg: Color, width: usize) -> TextLine<'static> {
    let mut line = to_line(&row.line, default_fg);
    let Some(bg) = row.bg else {
        return line;
    };
    let used: usize = line
        .spans
        .iter()
        .map(|s| crate::tui::line::text_width(&s.content))
        .sum();
    let pad = width.saturating_sub(used);
    if pad > 0 {
        line.spans
            .push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
    }
    line.style = line.style.bg(bg);
    line
}

/// Draw `rows` top-down into `area` (clipped to the buffer, so the caller may
/// pass the buffer's own area — the driver decides where the viewport sits).
/// Rows past the area's height are dropped: the caller sizes the frame, this
/// is only the safety net.
pub fn render_rows(rows: &[Row], default_fg: Color, buf: &mut Buffer, area: Rect) {
    let area = buf.area.intersection(area);
    for (i, row) in rows.iter().enumerate() {
        let Ok(offset) = u16::try_from(i) else {
            break;
        };
        if offset >= area.height {
            break;
        }
        let y = area.y + offset;
        if let Some(bg) = row.bg {
            buf.set_style(Rect::new(area.x, y, area.width, 1), Style::default().bg(bg));
        }
        let width = area
            .width
            .saturating_sub(u16::try_from(row.padding_right).unwrap_or(0));
        buf.set_line(area.x, y, &to_line(&row.line, default_fg), width);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::line::{ImageRef, Seg};

    fn styled(text: &str, style: SegStyle) -> Line {
        Line::styled(text, style)
    }

    #[test]
    fn style_maps_every_attribute() {
        let plain = style_of(SegStyle::plain(), Color::White);
        assert_eq!(plain.fg, Some(Color::White), "default colour fills in");
        assert_eq!(plain.bg, None);
        assert!(plain.add_modifier.is_empty());

        let full = SegStyle::fg(Color::Red)
            .with_bg(Color::Blue)
            .bold()
            .italic()
            .underline()
            .strikethrough();
        let style = style_of(full, Color::White);
        assert_eq!(style.fg, Some(Color::Red), "own colour wins");
        assert_eq!(style.bg, Some(Color::Blue));
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
        assert!(style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(
            style.add_modifier.contains(Modifier::CROSSED_OUT),
            "strikethrough is real now"
        );
    }

    #[test]
    fn line_keeps_segments_and_text() {
        let mut line = Line::styled("a", SegStyle::fg(Color::Red));
        line.push_styled("b", SegStyle::plain().bold());
        let out = to_line(&line, Color::White);
        assert_eq!(out.spans.len(), 2);
        assert_eq!(out.spans[0].content, "a");
        assert_eq!(out.spans[0].style.fg, Some(Color::Red));
        assert_eq!(out.spans[1].content, "b");
        assert_eq!(out.spans[1].style.fg, Some(Color::White));
        assert!(out.spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn empty_line_has_no_spans() {
        assert!(to_line(&Line::empty(), Color::White).spans.is_empty());
        // A segment that is empty stays an (empty) span — the row still
        // occupies exactly one terminal row either way.
        let line = Line {
            segs: vec![Seg {
                text: String::new(),
                style: SegStyle::plain(),
            }],
            image: None,
        };
        assert_eq!(to_line(&line, Color::White).spans.len(), 1);
    }

    #[test]
    fn image_rows_render_the_placeholder() {
        let line = Line {
            segs: vec![Seg {
                text: "ignored".into(),
                style: SegStyle::plain(),
            }],
            image: Some(ImageRef {
                url: "a.png".into(),
                cols: 10,
                rows: 3,
            }),
        };
        let out = to_line(&line, Color::White);
        assert_eq!(out.spans.len(), 1);
        assert_eq!(out.spans[0].content, IMAGE_PLACEHOLDER);
    }

    #[test]
    fn render_rows_writes_top_down_and_honours_wide_chars() {
        let rows = vec![
            Row::new(styled("ab", SegStyle::fg(Color::Red))),
            Row::new(Line::plain("中文")),
        ];
        let mut buf = Buffer::empty(Rect::new(0, 0, 6, 3));
        let area = buf.area;
        render_rows(&rows, Color::White, &mut buf, area);
        assert_eq!(buf[(0, 0)].symbol(), "a");
        assert_eq!(buf[(0, 0)].fg, Color::Red);
        assert_eq!(buf[(1, 0)].symbol(), "b");
        // A double-width grapheme occupies two cells; the second is blanked.
        assert_eq!(buf[(0, 1)].symbol(), "中");
        assert_eq!(buf[(2, 1)].symbol(), "文");
        // Third row untouched.
        assert_eq!(buf[(0, 2)].symbol(), " ");
    }

    #[test]
    fn render_rows_paints_full_row_background() {
        let theme_bg = Color::Rgb(55, 55, 55);
        let rows = vec![Row::bubble(Line::plain("❯ hi"), theme_bg)];
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        let area = buf.area;
        render_rows(&rows, Color::White, &mut buf, area);
        for x in 0..10u16 {
            assert_eq!(buf[(x, 0)].bg, theme_bg, "bubble spans the row at x={x}");
        }
    }

    #[test]
    fn render_rows_respects_a_non_zero_buffer_origin() {
        let rows = vec![Row::new(Line::plain("x"))];
        let mut buf = Buffer::empty(Rect::new(0, 7, 4, 2));
        let area = buf.area;
        render_rows(&rows, Color::White, &mut buf, area);
        assert_eq!(buf[(0, 7)].symbol(), "x");
    }

    #[test]
    fn history_line_pads_bubbles_to_width() {
        let bg = Color::Rgb(55, 55, 55);
        let row = Row::bubble(Line::plain("❯ hi"), bg);
        let line = history_line(&row, Color::White, 12);
        let width: usize = line
            .spans
            .iter()
            .map(|s| crate::tui::line::text_width(&s.content))
            .sum();
        assert_eq!(width, 12, "bubble spans the terminal width in scrollback");
        assert_eq!(line.spans.last().map(|s| s.style.bg), Some(Some(bg)));

        // Rows without a background are never padded: trailing spaces in
        // scrollback are exactly what the write-once design avoids.
        let plain = Row::new(Line::plain("plain"));
        let line = history_line(&plain, Color::White, 40);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "plain");
    }
}

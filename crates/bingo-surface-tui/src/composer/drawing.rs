//! Input ink and the caret belong to the dock, not the editing buffer.
use super::Layout;
use crate::{clock, theme, views::Drawing};
use bingo_sdk::component::ComposerProps;
use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub fn draw(
    layout: &Layout,
    props: &ComposerProps,
    empty: bool,
    rows: usize,
    active: bool,
    keyed_at: std::time::Instant,
    now: clock::Now,
) -> Drawing {
    let (now, rest) = rested(now, keyed_at);
    let overflow = usize::from(layout.lines.len() > 8 && rows >= 9);
    let count = rows.saturating_sub(overflow).min(8);
    let start = layout.lines.len().saturating_sub(count);
    let caret = if active {
        theme::glyphs::get("caret")
    } else {
        " "
    };
    let caret = Span::styled(
        caret,
        theme::caret_ink(now, "glow", "you").unwrap_or_default(),
    );
    let mut lines = Vec::new();
    if overflow > 0 {
        lines.push(Line::from(vec![
            Span::raw(" ".repeat(super::COLUMN)),
            Span::styled(
                format!("{} {start} lines above", theme::glyphs::get("ell")),
                ink("dim"),
            ),
        ]));
    }
    for (index, text) in layout.lines.iter().enumerate().skip(start).take(count) {
        let mut spans = vec![Span::raw(" ".repeat(super::COLUMN))];
        if !props.tag.is_empty() {
            spans.push(Span::styled(props.tag.clone(), theme::text()));
        }
        if empty {
            spans.extend([
                caret.clone(),
                Span::styled(props.placeholder.clone(), ink("dim")),
            ]);
        } else {
            spans.extend(input_with_images(
                text,
                &layout.images,
                layout.selection.get(index).and_then(Option::as_ref),
                (index == layout.cursor.0).then_some(layout.cursor.1),
                &caret,
            ));
        }
        lines.push(Line::from(spans));
    }
    Drawing {
        lines,
        // Without an input row there is no visible caret whose ink can change.
        next: (active && now.motion && layout.cursor.0 >= start && count > 0)
            .then_some((now.instant + clock::FRAME).min(rest)),
    }
}

fn rested(now: clock::Now, keyed_at: std::time::Instant) -> (clock::Now, std::time::Instant) {
    let rest = keyed_at + super::CARET_REST;
    (
        clock::Now {
            // The Windows console redraws a changing caret as a visible flash.
            // Keep it steady there while the rest of the surface may still move.
            motion: now.motion && now.instant < rest && !cfg!(windows),
            ..now
        },
        rest,
    )
}

#[cfg(test)]
fn input(text: &str, cursor: Option<usize>, caret: &Span<'static>) -> Vec<Span<'static>> {
    input_with_images(text, &[], None, cursor, caret)
}

fn input_with_images(
    text: &str,
    images: &[u32],
    selection: Option<&std::ops::Range<usize>>,
    cursor: Option<usize>,
    caret: &Span<'static>,
) -> Vec<Span<'static>> {
    let chips: Vec<_> = crate::pictures::spans(text)
        .into_iter()
        .filter(|(_, n)| images.contains(n))
        .collect();
    let command = text.starts_with(['/', '!']);
    let end = if command {
        text.find(char::is_whitespace).unwrap_or(text.len())
    } else {
        0
    };
    let mut mention = false;
    let mut column = 0;
    let mut spans = Vec::new();
    for (byte, ch) in text.grapheme_indices(true) {
        if cursor == Some(column) {
            spans.push(caret.clone());
        }
        let style = if chips.iter().any(|(range, _)| range.contains(&byte)) {
            ink("plan")
        } else if byte < end {
            if text.starts_with('!') {
                ink("code")
            } else if byte == 0 {
                ink("glow")
            } else {
                theme::text().patch(theme::bold())
            }
        } else {
            if ch == "@" {
                mention = text[byte + 1..].chars().next().is_some_and(mention_char);
            } else if !ch.chars().all(mention_char) {
                mention = false;
            }
            if mention { ink("plan") } else { theme::text() }
        };
        let selected =
            selection.is_some_and(|range| range.start < column + ch.width() && column < range.end);
        let style = if selected {
            style.patch(theme::raised().patch(theme::presence()))
        } else {
            style
        };
        spans.push(Span::styled(ch.to_owned(), style));
        column += ch.width();
    }
    if cursor == Some(column) {
        spans.push(caret.clone());
    }
    spans
}
fn mention_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '/' | '-')
}
fn ink(name: &str) -> ratatui::style::Style {
    theme::foreground(name).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{later, scene, still};

    #[test]
    fn input_inks_commands_shells_and_mentions_without_splitting_graphemes() {
        let spans = input("/fix @src/a.rs e\u{301}", Some(16), &Span::raw("▍"));
        assert_eq!(spans[0].style, ink("glow"));
        assert_eq!(spans[1].style, theme::text().patch(theme::bold()));
        assert_eq!(spans[5].style, ink("plan"));
        assert!(spans.iter().any(|span| span.content == "e\u{301}"));
        assert_eq!(input("!ls", None, &Span::raw(" "))[1].style, ink("code"));
    }

    #[test]
    fn input_marks_each_cell_of_a_selected_wide_grapheme() {
        let selection = 1..5;
        let spans = input_with_images("a你好b", &[], Some(&selection), None, &Span::raw(" "));
        let selected = theme::text().patch(theme::raised().patch(theme::presence()));
        assert_eq!(spans[0].style, theme::text());
        assert_eq!(spans[1].style, selected);
        assert_eq!(spans[2].style, selected);
        assert_eq!(spans[3].style, theme::text());
    }

    #[test]
    fn input_keeps_eight_tail_rows_and_only_a_visible_caret_schedules() {
        let now = scene().1;
        let mut editor = super::super::Composer::default();
        editor.set(
            &(0..12)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let layout = editor.layout(30);
        let props = ComposerProps::default();
        let drawn = draw(&layout, &props, false, 9, true, now.instant, now);
        assert_eq!(drawn.lines.len(), 9);
        assert!(drawn.lines[0].to_string().contains("4 lines above"));
        assert!(drawn.lines[1].to_string().contains("line 4"));
        assert!(drawn.lines[8].to_string().contains("line 11▍"));
        assert_eq!(drawn.next.is_some(), !cfg!(windows));
        assert!(
            draw(&layout, &props, false, 9, false, now.instant, now)
                .next
                .is_none()
        );
        assert_eq!(
            draw(&layout, &props, false, 9, true, now.instant, still(now)).lines,
            draw(
                &layout,
                &props,
                false,
                9,
                true,
                now.instant,
                still(later(now, 400))
            )
            .lines
        );
    }
}

//! The way back to the foot of the transcript, drawn only while the foot is
//! off the screen.
//!
//! A held transcript keeps its line while more arrives under it
//! ([`crate::scroll`]), and nothing on the screen said so: the person had
//! `end` and `pgdn`, and the mouse had nothing. The pill is the activity
//! band's first row — its air, which the frame holds whether or not anything
//! is going on (§3: the band is reserved) — so it costs no row and moves
//! nothing; a click on it follows the tail again, as `end` does, and the
//! moment the foot is on the screen the row is air again.
//!
//! What it counts is the lines under the frame's last row: the one fact the
//! scroll already has, read at draw time, never a counter of its own.

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::clock::Now;
use crate::theme;
use crate::ui::Ui;

/// The hint after the count: the key and the click that follow the foot.
const HINT: &str = "end or click to follow";

/// The pill as a band row draws it: the line, and the cells it covers.
pub struct Pill {
    pub line: Line<'static>,
    pub x: u16,
    pub width: u16,
}

/// The lines under the frame's last row while the transcript is held, and
/// nothing while it follows the tail.
pub fn below(ui: &Ui, now: Now) -> Option<usize> {
    if ui.scroll.following() {
        return None;
    }
    let (total, rows) = ui.transcript();
    let top = ui.scroll.top(total, rows, now.instant);
    Some(total.saturating_sub(top + rows))
}

/// The pill for a band `width` wide, centred on its bar: the count in text
/// and the hint in dim, both on the raised tint.
pub fn pill(below: usize, width: usize) -> Option<Pill> {
    let (lead, hint) = label(below, width)?;
    let text = format!(" {lead}");
    let hint = match hint {
        Some(hint) => format!(" · {hint} "),
        None => " ".to_string(),
    };
    let used = text.width() + hint.width();
    let x = width.saturating_sub(used) / 2;
    let bar = theme::raised();
    let line = Line::from(vec![
        Span::raw(" ".repeat(x)),
        Span::styled(text, theme::text().patch(bar)),
        Span::styled(hint, theme::dim().patch(bar)),
    ]);
    Some(Pill {
        line,
        x: u16::try_from(x).unwrap_or(u16::MAX),
        width: u16::try_from(used).unwrap_or(u16::MAX),
    })
}

/// Where this frame drew the pill, when it drew one: what a click is
/// answered against.
pub fn placed(ui: &Ui, area: Rect, now: Now) -> Option<Rect> {
    if area.height == 0 {
        return None;
    }
    let pill = pill(below(ui, now)?, usize::from(area.width))?;
    Some(Rect {
        x: area.x.saturating_add(pill.x),
        y: area.y,
        width: pill.width,
        height: 1,
    })
}

/// The words, as much of them as `width` holds with a cell of air each side:
/// the count and the hint, the count alone, the arrow alone — and nothing on
/// a row too narrow for even that. No count when there is nothing below: a
/// resize can hold a transcript whose foot is on the screen.
pub fn label(below: usize, width: usize) -> Option<(String, Option<&'static str>)> {
    let lead = match below {
        0 => "↓".to_string(),
        1 => "↓ 1 line below".to_string(),
        many => format!("↓ {many} lines below"),
    };
    let fits = |text: &str| text.width() + 2 <= width;
    if fits(&format!("{lead} · {HINT}")) {
        return Some((lead, Some(HINT)));
    }
    if fits(&lead) {
        return Some((lead, None));
    }
    fits("↓").then(|| ("↓".to_string(), None))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(below: usize, width: usize) -> Option<String> {
        label(below, width).map(|(lead, hint)| match hint {
            Some(hint) => format!("{lead} · {hint}"),
            None => lead,
        })
    }

    #[test]
    fn the_label_says_how_far_the_foot_is_and_how_to_reach_it() {
        assert_eq!(
            words(37, 80).as_deref(),
            Some("↓ 37 lines below · end or click to follow")
        );
        assert_eq!(
            words(1, 80).as_deref(),
            Some("↓ 1 line below · end or click to follow")
        );
        assert_eq!(
            words(0, 80).as_deref(),
            Some("↓ · end or click to follow"),
            "held with the foot on the screen counts nothing"
        );
    }

    #[test]
    fn a_narrow_row_keeps_the_count_and_then_the_arrow() {
        assert_eq!(words(37, 30).as_deref(), Some("↓ 37 lines below"));
        assert_eq!(words(37, 10).as_deref(), Some("↓"));
        assert_eq!(words(37, 2), None, "no room for the air round it");
    }

    #[test]
    fn the_pill_sits_in_the_middle_of_the_row() {
        let pill = pill(37, 80).expect("a pill");
        let left = usize::from(pill.x);
        let right = 80 - usize::from(pill.x + pill.width);
        assert!(left.abs_diff(right) <= 1, "{left} left, {right} right");
        let text: String = pill.line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            text,
            format!(
                "{}{}",
                " ".repeat(left),
                " ↓ 37 lines below · end or click to follow "
            )
        );
    }
}

//! `View::Badge`: `[ text ]` in the tone's colour. The tone is the one
//! styling hook a plugin has (ADR-0013 §1); which colour it becomes is the
//! surface's answer, and `attention` wears `presence` — the colour of
//! everything that wants a person (design §4).

use bingo_sdk::Tone;
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::theme;

pub fn lines(text: &str, tone: Tone) -> Vec<Line<'static>> {
    vec![Line::from(span(text, tone))]
}

/// A badge inside another row: a tree node's, a table cell's.
pub fn span(text: &str, tone: Tone) -> Span<'static> {
    Span::styled(format!("[ {text} ]"), style(tone))
}

fn style(tone: Tone) -> Style {
    match tone {
        Tone::Neutral => theme::dim(),
        Tone::Good => theme::good(),
        Tone::Bad => theme::bad(),
        Tone::Attention => theme::presence(),
    }
}

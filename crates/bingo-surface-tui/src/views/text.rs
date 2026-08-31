//! `View::Text`: what a plugin wrote, one row per line. An empty text keeps
//! its row, so a plugin can put air between two things with one.

use ratatui::text::{Line, Span};

use crate::theme;

pub fn lines(text: &str) -> Vec<Line<'static>> {
    if text.is_empty() {
        return vec![Line::default()];
    }
    text.lines()
        .map(|line| Line::from(Span::styled(line.to_string(), theme::text())))
        .collect()
}

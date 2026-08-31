//! `View::Diff`: through the one diff renderer, so a plugin's diff is
//! coloured by column exactly like the one a permission card previews.

use ratatui::text::Line;

pub fn lines(unified: &str) -> Vec<Line<'static>> {
    crate::preview::diff(unified)
}

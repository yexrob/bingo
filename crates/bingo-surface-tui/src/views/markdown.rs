//! `View::Markdown`: through the one markdown renderer, so a plugin's prose
//! reads exactly like the model's (design §5).

use ratatui::text::Line;

pub fn lines(text: &str, width: usize) -> Vec<Line<'static>> {
    crate::markdown::render(text, width)
}

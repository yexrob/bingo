//! `View::List`: a point opens every item (design §5's markdown lists, which
//! is what a person already reads a list as here).

use ratatui::text::{Line, Span};

use crate::theme;

pub fn lines(items: &[String]) -> Vec<Line<'static>> {
    items.iter().map(|item| row(item)).collect()
}

fn row(item: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{} ", theme::point()), theme::dim()),
        Span::styled(item.to_string(), theme::text()),
    ])
}

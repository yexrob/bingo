//! `View::Actions`: `[ 1 Approve ] [ 2 Next ]`. The key is the plugin's hint
//! or, when it named none, where the item sits — a plugin says what a button
//! is called and, at most, one letter; which key fires it is the surface's
//! (ADR-0013 §4).
//!
//! An item a person has fired wears the ellipsis until the answer comes back,
//! so a key that has landed is never mistaken for one that has not.

use bingo_sdk::ActionItem;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme;
use crate::views::Marks;

pub fn lines(items: &[ActionItem], width: usize, marks: &Marks) -> Vec<Line<'static>> {
    let mut rows: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut used = 0usize;
    for (at, item) in items.iter().enumerate() {
        let button = button(item, at, marks);
        let cells: usize = button.iter().map(|span| span.content.width()).sum();
        if used > 0 && used + 1 + cells > width {
            rows.push(Vec::new());
            used = 0;
        }
        if used > 0 {
            push(&mut rows, Span::raw(" "));
            used += 1;
        }
        for span in button {
            push(&mut rows, span);
        }
        used += cells;
    }
    rows.into_iter().map(Line::from).collect()
}

fn push(rows: &mut [Vec<Span<'static>>], span: Span<'static>) {
    if let Some(row) = rows.last_mut() {
        row.push(span);
    }
}

/// One button: its brackets dim, its key `presence`, its label plain.
fn button(item: &ActionItem, at: usize, marks: &Marks) -> Vec<Span<'static>> {
    let waiting = marks.pending.as_ref() == Some(&item.action);
    let label = match waiting {
        true => format!("{}{}", item.label, theme::ellipsis()),
        false => item.label.clone(),
    };
    vec![
        Span::styled("[ ".to_string(), theme::dim()),
        Span::styled(format!("{} ", key_of(item, at)), theme::presence()),
        Span::styled(label, theme::text()),
        Span::styled(" ]".to_string(), theme::dim()),
    ]
}

/// The key that fires an item: the plugin's hint, else where it sits.
pub fn key_of(item: &ActionItem, at: usize) -> char {
    item.key
        .unwrap_or_else(|| char::from_digit(at as u32 + 1, 10).unwrap_or('?'))
}

/// The item a key press fires, when a card that offers one has the focus.
pub fn fired<'a>(items: &[&'a ActionItem], key: char) -> Option<&'a ActionItem> {
    items
        .iter()
        .enumerate()
        .find(|(at, item)| key_of(item, *at) == key)
        .map(|(_, item)| *item)
}

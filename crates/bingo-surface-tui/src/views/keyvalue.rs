//! `View::KeyValue`: a table of two columns with nothing to head them, so it
//! wears no rule — the key column, dim, is the edge the eye runs down. Values
//! that are all numbers hug their right edge, as a table's do (design §5).

use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme;
use crate::views::{MISSING, clip, numeric};

const GUTTER: usize = 2;

pub fn lines(rows: &[(String, String)], width: usize) -> Vec<Line<'static>> {
    let keys = rows.iter().map(|(key, _)| key.width()).max().unwrap_or(0);
    let numbers = rows.iter().all(|(_, value)| numeric(&shown(value)));
    let values = rows
        .iter()
        .map(|(_, value)| shown(value).width())
        .max()
        .unwrap_or(0);
    rows.iter()
        .map(|(key, value)| row(key, &shown(value), keys, numbers.then_some(values), width))
        .collect()
}

/// What a row shows for a value it does not have.
fn shown(value: &str) -> String {
    match value.is_empty() {
        true => MISSING.to_string(),
        false => value.to_string(),
    }
}

fn row(key: &str, value: &str, keys: usize, right: Option<usize>, width: usize) -> Line<'static> {
    let lead = format!("{key:<keys$}{}", " ".repeat(GUTTER));
    let value = match right {
        Some(column) => format!(
            "{}{value}",
            " ".repeat(column.saturating_sub(value.width()))
        ),
        None => value.to_string(),
    };
    Line::from(vec![
        Span::styled(lead.clone(), theme::dim()),
        Span::styled(
            clip(&value, width.saturating_sub(lead.width())),
            theme::text(),
        ),
    ])
}

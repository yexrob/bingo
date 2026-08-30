//! A `View` as styled lines. It is the one renderer for what a plugin hands a
//! surface to show: a command's block and the `ctrl+t` panel both draw through
//! it, so a table looks the same wherever it came from.

use bingo_sdk::View;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme;

pub fn lines(view: &View) -> Vec<Line<'static>> {
    match view {
        View::Text { text } => text.lines().map(plain).collect(),
        View::List { items } => items.iter().map(|i| plain(&format!("• {i}"))).collect(),
        View::Table { headers, rows } => table(headers, rows),
        // The rest draw as their fold until M11d gives each its renderer.
        other => other.fold().lines().map(plain).collect(),
    }
}

fn table(headers: &[String], rows: &[Vec<String>]) -> Vec<Line<'static>> {
    let widths: Vec<usize> = headers
        .iter()
        .enumerate()
        .map(|(i, header)| {
            rows.iter()
                .filter_map(|row| row.get(i))
                .map(|cell| cell.width())
                .chain(std::iter::once(header.width()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = vec![Line::from(Span::styled(
        row(headers, &widths),
        theme::bold(),
    ))];
    out.extend(rows.iter().map(|cells| plain(&row(cells, &widths))));
    out
}

fn row(cells: &[String], widths: &[usize]) -> String {
    cells
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            format!(
                "{cell:<width$}",
                width = widths.get(i).copied().unwrap_or(0)
            )
        })
        .collect::<Vec<_>>()
        .join("  ")
        .trim_end()
        .to_string()
}

fn plain(text: &str) -> Line<'static> {
    Line::from(Span::raw(text.to_string()))
}

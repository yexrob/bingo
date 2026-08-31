//! `View::Table`: hairline rules, right-aligned numbers, `–` for a missing
//! cell (design §5). A table never wraps — it folds to the width and opens in
//! a sheet — so a row wider than the frame is cut, not folded onto the next.

use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use crate::theme;
use crate::views::{MISSING, clip, numeric};

/// What separates two columns.
const GUTTER: &str = "  ";

/// One column, as the whole table measures it.
struct Column {
    width: usize,
    /// Every value in it is a number, so the column reads down its right edge.
    numeric: bool,
}

pub fn lines(headers: &[String], rows: &[Vec<String>], width: usize) -> Vec<Line<'static>> {
    let columns = columns(headers, rows);
    let mut out = vec![Line::from(Span::styled(
        clip(&laid(headers, &columns), width),
        theme::text().patch(theme::bold()),
    ))];
    out.push(rule(across(&columns).min(width)));
    out.extend(rows.iter().map(|row| {
        Line::from(Span::styled(
            clip(&laid(row, &columns), width),
            theme::text(),
        ))
    }));
    out
}

/// The hairline under the headers: the one rule that says these rows are one
/// table and not a list of sentences. It runs the width of the widest row a
/// column could hold, not of the headers — a header narrower than its column
/// would cut the rule short of the table it heads.
fn rule(width: usize) -> Line<'static> {
    Line::from(Span::styled(theme::rule().repeat(width), theme::dim()))
}

/// How wide the table is, gutters included.
fn across(columns: &[Column]) -> usize {
    let cells: usize = columns.iter().map(|column| column.width).sum();
    cells + GUTTER.width() * columns.len().saturating_sub(1)
}

fn columns(headers: &[String], rows: &[Vec<String>]) -> Vec<Column> {
    headers
        .iter()
        .enumerate()
        .map(|(at, header)| Column {
            width: rows
                .iter()
                .map(|row| cell(row, at).width())
                .chain(std::iter::once(header.width()))
                .max()
                .unwrap_or(0),
            // A cell a row has not does not stop a column being numbers.
            numeric: rows
                .iter()
                .map(|row| cell(row, at))
                .all(|cell| cell == MISSING || numeric(&cell)),
        })
        .collect()
}

/// What a row carries in a column: its cell, or the mark for one it has not.
fn cell(row: &[String], at: usize) -> String {
    match row.get(at) {
        Some(cell) if !cell.is_empty() => cell.clone(),
        _ => MISSING.to_string(),
    }
}

/// One row laid out in the table's columns; numbers hug their right edge.
fn laid(cells: &[String], columns: &[Column]) -> String {
    columns
        .iter()
        .enumerate()
        .map(|(at, column)| pad(&cell(cells, at), column))
        .collect::<Vec<_>>()
        .join(GUTTER)
        .trim_end()
        .to_string()
}

fn pad(cell: &str, column: &Column) -> String {
    let room = column.width.saturating_sub(cell.width());
    let space = " ".repeat(room);
    match column.numeric {
        true => format!("{space}{cell}"),
        false => format!("{cell}{space}"),
    }
}

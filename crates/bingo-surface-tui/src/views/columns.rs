//! `View::Columns`: the width split evenly between the children. Below
//! [`NARROW`] cells there is no width to split — two columns of thirty cells
//! read worse than one of sixty — so they stack instead.

use bingo_sdk::View;
use ratatui::text::{Line, Span};

use crate::views::{Marks, fit, marked, stack};

/// The width from which columns are worth having.
pub const NARROW: usize = 60;
/// What separates two columns.
const GUTTER: usize = 2;

pub fn lines(children: &[View], width: usize, marks: &Marks) -> Vec<Line<'static>> {
    if children.len() < 2 || width < NARROW {
        return stack::lines(children, width, marks);
    }
    let column = column(width, children.len());
    let drawn: Vec<Vec<Line<'static>>> = children
        .iter()
        .map(|child| marked(child, column, marks))
        .collect();
    let rows = drawn.iter().map(Vec::len).max().unwrap_or(0);
    (0..rows).map(|row| beside(&drawn, row, column)).collect()
}

/// How wide one of `n` columns is once the gutters are taken out.
fn column(width: usize, n: usize) -> usize {
    width
        .saturating_sub(GUTTER * (n - 1))
        .checked_div(n)
        .unwrap_or(width)
        .max(1)
}

/// One row across every column: each column's line of that row, or its blank.
fn beside(drawn: &[Vec<Line<'static>>], row: usize, column: usize) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (at, lines) in drawn.iter().enumerate() {
        if at > 0 {
            spans.push(Span::raw(" ".repeat(GUTTER)));
        }
        let line = lines.get(row).cloned().unwrap_or_default();
        spans.extend(fit(line, column).spans);
    }
    Line::from(spans)
}

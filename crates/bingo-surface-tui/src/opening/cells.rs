//! One drawn row as the cells a terminal draws it in, and back again.
//!
//! The piece rewrites the welcome box cell by cell — a border glyph the light
//! has not reached is blank, a word the beam has not reached is blank — and a
//! `Line` is runs of text rather than cells, so it is taken apart and put back
//! together. Whatever shares a style comes back as one span, which is what
//! makes the frame the piece lands on the box itself and not a wordier drawing
//! of it.

use ratatui::style::Style;
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

/// One cell of a row: what stands in it, and what it is drawn in.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub glyph: String,
    pub style: Style,
}

impl Cell {
    /// How many columns it takes. A wide glyph takes two, so a row of them
    /// still measures what the terminal will measure.
    pub fn width(&self) -> usize {
        UnicodeWidthStr::width(self.glyph.as_str())
    }

    /// The same columns with nothing in them.
    pub fn blank(&self) -> Self {
        Cell {
            glyph: " ".repeat(self.width()),
            style: Style::new(),
        }
    }

    /// Whether anything stands in it at all.
    pub fn blank_already(&self) -> bool {
        self.glyph.trim().is_empty()
    }
}

/// One row, cell by cell.
pub fn of(line: &Line<'_>) -> Vec<Cell> {
    line.spans
        .iter()
        .flat_map(|span| {
            span.content.chars().map(|glyph| Cell {
                glyph: glyph.to_string(),
                style: span.style,
            })
        })
        .collect()
}

/// The row those cells are, with everything that shares a style in one span.
pub fn line(cells: Vec<Cell>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for cell in cells {
        match spans.last_mut() {
            Some(last) if last.style == cell.style => last.content.to_mut().push_str(&cell.glyph),
            _ => spans.push(Span::styled(cell.glyph, cell.style)),
        }
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    #[test]
    fn a_row_comes_apart_into_cells_and_back_into_the_runs_it_was() {
        let row = Line::from(vec![
            Span::styled("✻ ", theme::presence()),
            Span::styled("hello", theme::text()),
        ]);
        let cells = of(&row);
        assert_eq!(cells.len(), 7, "one cell a glyph");
        assert_eq!(cells[0].glyph, "✻");
        assert_eq!(cells[1].style, theme::presence(), "the space is the mark's");
        assert_eq!(line(cells), row, "and the runs come back as they were");
    }

    #[test]
    fn a_wide_glyph_takes_the_two_columns_the_terminal_gives_it() {
        let row = Line::from(Span::raw("汉a"));
        let cells = of(&row);
        assert_eq!(cells.iter().map(Cell::width).sum::<usize>(), 3);
        assert_eq!(cells[0].blank().glyph, "  ", "and blanks to both of them");
        assert_eq!(cells[1].blank().glyph, " ");
    }

    #[test]
    fn a_cell_with_nothing_in_it_says_so() {
        let cells = of(&Line::from(Span::styled(" x", theme::dim())));
        assert!(cells[0].blank_already());
        assert!(!cells[1].blank_already());
    }
}

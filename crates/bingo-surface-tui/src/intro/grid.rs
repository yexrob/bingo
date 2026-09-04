//! The canvas the opening is drawn on.
//!
//! Every shot writes into one of these — the marched world, the embers over
//! it, the welcome box the last shot flattens into — and only at the end is
//! it turned into the rows a terminal draws. One canvas rather than a stack
//! of layers to merge: a cell holds what is in front of it, and the order
//! things are written in is the order they stand in.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

/// One cell: what is drawn there, and in what light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub glyph: char,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            glyph: ' ',
            style: Style::new(),
        }
    }
}

/// A frame of the opening, cell by cell.
#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    width: u16,
    height: u16,
    cells: Vec<Cell>,
}

impl Grid {
    pub fn new(width: u16, height: u16) -> Self {
        Grid {
            width,
            height,
            cells: vec![Cell::default(); usize::from(width) * usize::from(height)],
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn cell(&self, x: u16, y: u16) -> Cell {
        self.at(x, y)
            .and_then(|index| self.cells.get(index).copied())
            .unwrap_or_default()
    }

    /// Put one cell down. A write outside the canvas is dropped, so a shot
    /// may aim at where a thing *is* without first asking whether it is on
    /// screen.
    pub fn set(&mut self, x: u16, y: u16, cell: Cell) {
        if let Some(index) = self.at(x, y)
            && let Some(slot) = self.cells.get_mut(index)
        {
            *slot = cell;
        }
    }

    /// Put a word down, one cell a character, from `x` rightwards.
    pub fn write(&mut self, x: u16, y: u16, text: &str, style: Style) {
        for (offset, glyph) in text.chars().enumerate() {
            let Ok(offset) = u16::try_from(offset) else {
                return;
            };
            self.set(x.saturating_add(offset), y, Cell { glyph, style });
        }
    }

    /// The rows a terminal draws, runs of one style to a span.
    pub fn lines(&self) -> Vec<Line<'static>> {
        (0..self.height).map(|y| self.line(y)).collect()
    }

    fn line(&self, y: u16) -> Line<'static> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for x in 0..self.width {
            let cell = self.cell(x, y);
            match spans.last_mut() {
                Some(last) if last.style == cell.style => last.content.to_mut().push(cell.glyph),
                _ => spans.push(Span::styled(cell.glyph.to_string(), cell.style)),
            }
        }
        Line::from(spans)
    }

    fn at(&self, x: u16, y: u16) -> Option<usize> {
        (x < self.width && y < self.height)
            .then(|| usize::from(y) * usize::from(self.width) + usize::from(x))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme;

    #[test]
    fn a_fresh_canvas_is_blank_to_its_edges() {
        let grid = Grid::new(4, 2);
        assert_eq!(grid.lines().len(), 2);
        for line in grid.lines() {
            assert_eq!(line.to_string(), "    ");
        }
    }

    #[test]
    fn a_write_outside_the_canvas_is_dropped_rather_than_wrapped() {
        let mut grid = Grid::new(4, 2);
        grid.write(2, 0, "abcdef", Style::new());
        grid.write(0, 9, "gone", Style::new());
        assert_eq!(grid.lines()[0].to_string(), "  ab");
        assert_eq!(grid.lines()[1].to_string(), "    ");
    }

    #[test]
    fn a_run_of_one_style_is_one_span() {
        let mut grid = Grid::new(6, 1);
        grid.write(0, 0, "abc", theme::presence());
        grid.write(3, 0, "def", theme::presence());
        assert_eq!(grid.lines()[0].spans.len(), 1, "{:?}", grid.lines()[0]);
        assert_eq!(grid.lines()[0].to_string(), "abcdef");
    }

    #[test]
    fn a_change_of_style_starts_a_new_span() {
        let mut grid = Grid::new(4, 1);
        grid.write(0, 0, "ab", theme::presence());
        grid.write(2, 0, "cd", theme::dim());
        let spans = &grid.lines()[0].spans;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].content, "ab");
        assert_eq!(spans[1].content, "cd");
    }

    #[test]
    fn what_is_written_last_is_what_stands_in_front() {
        let mut grid = Grid::new(3, 1);
        grid.write(0, 0, "...", Style::new());
        grid.write(1, 0, "@", theme::presence());
        assert_eq!(grid.lines()[0].to_string(), ".@.");
    }
}

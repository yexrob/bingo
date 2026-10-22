//! The visible draft rows and their mapping back from terminal cells.

use ratatui::layout::{Position, Rect};

use super::{CARET_COLUMNS, COLUMN, Layout, ROWS};

pub(super) struct Viewport {
    pub lines: std::ops::Range<usize>,
    pub overflow: usize,
}

/// The text cells and caret of the last rendered draft, even between gestures.
#[derive(Clone, Copy, Debug)]
pub struct Hitbox {
    pub area: Rect,
    pub cursor: Option<Position>,
    start: usize,
}

impl Layout {
    pub(super) fn viewport(&self, rows: usize) -> Viewport {
        let overflow = usize::from(self.lines.len() > ROWS && rows > ROWS);
        let count = rows.saturating_sub(overflow).min(ROWS);
        let start = self.lines.len().saturating_sub(count);
        Viewport {
            lines: start..self.lines.len(),
            overflow,
        }
    }

    pub fn hitbox(&self, area: Rect, tag_columns: usize) -> Hitbox {
        let viewport = self.viewport(usize::from(area.height));
        let prefix = COLUMN.saturating_add(tag_columns);
        let x = area
            .x
            .saturating_add(u16::try_from(prefix).unwrap_or(u16::MAX));
        let text = Rect {
            x,
            y: area.y.saturating_add(viewport.overflow as u16),
            width: area.right().saturating_sub(x),
            height: viewport.lines.len() as u16,
        };
        Hitbox {
            area: text,
            cursor: viewport.cursor(text, self.cursor),
            start: viewport.lines.start,
        }
    }
}

impl Viewport {
    fn cursor(&self, area: Rect, (row, column): (usize, usize)) -> Option<Position> {
        if !self.lines.contains(&row) {
            return None;
        }
        let x = area.x.checked_add(u16::try_from(column).ok()?)?;
        let y = area
            .y
            .checked_add(u16::try_from(row - self.lines.start).ok()?)?;
        let at = Position::new(x, y);
        area.contains(at).then_some(at)
    }
}

impl Hitbox {
    pub fn cell(&self, at: Position) -> Option<(usize, usize)> {
        if !self.area.contains(at) {
            return None;
        }
        let row = self.start + usize::from(at.y - self.area.y);
        let mut column = usize::from(at.x - self.area.x);
        // The painted caret inserts a cell; it does not replace draft text.
        if self
            .cursor
            .is_some_and(|cursor| cursor.y == at.y && cursor.x < at.x)
        {
            column = column.saturating_sub(CARET_COLUMNS);
        }
        Some((row, column))
    }
}

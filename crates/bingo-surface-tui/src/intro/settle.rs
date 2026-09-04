//! The box arriving over the world.
//!
//! Everything the last shot draws that is not the world: the block coming down
//! out of the air to be the box's own mark, the border walking out of that
//! corner along both edges, and the rows lighting up behind it in the order a
//! person reads them.
//!
//! The box itself is never composed here — it is [`crate::welcome`]'s, drawn
//! whole and handed in. What this decides is only *how much of it is there
//! yet*, cell by cell, so the last frame of the piece is the box and not a
//! second drawing of one.

use ratatui::text::Line;

use super::grid::{Cell, Grid};
use crate::welcome;

/// When the block leaves the air and when it lands on the mark.
const DESCENT: (f32, f32) = (0.06, 0.40);
/// When the border starts walking out of that corner and when it closes.
const BORDER: (f32, f32) = (0.30, 0.70);
/// When the first row inside the box lights and when the last one does.
const WORDS: (f32, f32) = (0.50, 0.86);

/// The box, `p` of the way through the last shot, drawn into `grid` with its
/// first row at `top`.
pub fn draw(grid: &mut Grid, boxed: &[Line<'static>], top: u16, p: f32) {
    let p = match p.is_nan() {
        true => 0.0,
        false => p.clamp(0.0, 1.0),
    };
    let height = u16::try_from(boxed.len()).unwrap_or(u16::MAX);
    for (row, line) in boxed.iter().enumerate() {
        let Ok(row) = u16::try_from(row) else { return };
        arriving(grid, line, (top, row, height), p);
    }
}

/// One row of the box: its border cells as far as the walk has come, and its
/// words once the row has lit.
fn arriving(grid: &mut Grid, line: &Line<'static>, (top, row, height): (u16, u16, u16), p: f32) {
    let width = grid.width();
    let reach = walked(width, height, p);
    let lit = row_lights(row, height, p);
    for (x, cell) in cells_of(line).into_iter().enumerate() {
        let Ok(x) = u16::try_from(x) else { return };
        let shown = match edge(x, row, (width, height)) {
            Some(along) => along < reach,
            None => lit,
        };
        if shown {
            grid.set(x, top + row, cell);
        }
    }
}

/// How far the border has walked out of the top-left corner, in cells. Two
/// walkers leave it at once — one rightwards along the top, one down the left
/// — and the border closes where they meet, at the far corner.
fn walked(width: u16, height: u16, p: f32) -> u32 {
    let along = ((p - BORDER.0) / (BORDER.1 - BORDER.0)).clamp(0.0, 1.0);
    let whole = u32::from(width) + u32::from(height);
    (crate::clock::ease_out(along) * whole as f32).round() as u32
}

/// How far round the border a cell stands from that corner, or `None` for a
/// cell that is not on the border at all.
fn edge(x: u16, row: u16, (width, height): (u16, u16)) -> Option<u32> {
    let last = (width.saturating_sub(1), height.saturating_sub(1));
    match (x, row) {
        (_, 0) => Some(u32::from(x)),
        (0, _) => Some(u32::from(row)),
        (x, _) if x == last.0 => Some(u32::from(last.0) + u32::from(row)),
        (_, row) if row == last.1 => Some(u32::from(last.1) + u32::from(x)),
        _ => None,
    }
}

/// Whether a row inside the box has lit yet. They light in order, spread over
/// [`WORDS`], so the greeting arrives, then the help line, then the cwd.
fn row_lights(row: u16, height: u16, p: f32) -> bool {
    let inside = height.saturating_sub(2).max(1);
    let which = f32::from(row.saturating_sub(1)) / f32::from(inside);
    p >= WORDS.0 + (WORDS.1 - WORDS.0) * which
}

/// One drawn row, cell by cell: what a `Line` says, spread back over the
/// columns it covers. Every glyph the box draws is one cell wide.
fn cells_of(line: &Line<'static>) -> Vec<Cell> {
    line.spans
        .iter()
        .flat_map(|span| {
            span.content.chars().map(|glyph| Cell {
                glyph,
                style: span.style,
            })
        })
        .collect()
}

/// The block on its way down to be the box's own mark.
///
/// It is drawn from the moment it leaves the air until the row the mark is on
/// lights up: before that the block a person is looking at is this one, and
/// after it the box's own `✻` has taken over. One mark on the screen at every
/// instant, and never two.
pub fn descending(grid: &mut Grid, from: (u16, u16), top: u16, p: f32) {
    if p >= WORDS.0 {
        return;
    }
    let walked =
        crate::clock::ease_in_out(((p - DESCENT.0) / (DESCENT.1 - DESCENT.0)).clamp(0.0, 1.0));
    let to = (welcome::MARK.0, top + welcome::MARK.1);
    grid.write(
        stepped(from.0, to.0, walked),
        stepped(from.1, to.1, walked),
        crate::theme::caret(),
        crate::theme::lit(1.0, 1.0),
    );
}

fn stepped(from: u16, to: u16, t: f32) -> u16 {
    let walked = f32::from(from) + (f32::from(to) - f32::from(from)) * t;
    walked.round().clamp(0.0, f32::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painted::truecolor;
    use crate::test_support::state;

    fn boxed(width: u16) -> Vec<Line<'static>> {
        welcome::lines(&state(), usize::from(width), None)
    }

    fn drawn(width: u16, p: f32) -> Vec<String> {
        crate::theme::with(truecolor(), || {
            let boxed = boxed(width);
            let height = u16::try_from(boxed.len()).expect("a short box");
            let mut grid = Grid::new(width, height);
            draw(&mut grid, &boxed, 0, p);
            grid.lines().iter().map(ToString::to_string).collect()
        })
    }

    #[test]
    fn nothing_of_the_box_is_there_before_the_border_starts() {
        let rows = drawn(60, 0.0);
        assert!(
            rows.iter().all(|row| row.trim().is_empty()),
            "the world is all there is: {rows:#?}"
        );
    }

    #[test]
    fn the_border_walks_out_of_the_corner_along_both_edges() {
        let half = drawn(60, 0.45);
        let top = &half[0];
        assert!(top.starts_with('╭'), "it starts at the corner: {top:?}");
        assert!(
            top.trim_end().chars().count() < 60,
            "and has not reached the other one: {top:?}"
        );
        assert!(
            half.iter().skip(1).any(|row| row.starts_with('│')),
            "and it is coming down the left as well: {half:#?}"
        );
        assert!(
            !half.iter().any(|row| row.contains("Welcome")),
            "with nothing said yet: {half:#?}"
        );
    }

    #[test]
    fn the_rows_light_in_the_order_they_are_read() {
        let said = |p| drawn(60, p).join("\n");
        assert!(said(0.55).contains("Welcome to bingo!"), "{}", said(0.55));
        assert!(!said(0.55).contains("cwd:"), "{}", said(0.55));
        assert!(said(0.95).contains("cwd:"), "{}", said(0.95));
    }

    #[test]
    fn the_whole_box_is_there_at_the_end_and_it_is_the_box_itself() {
        let whole = drawn(60, 1.0);
        let itself: Vec<String> = crate::theme::with(truecolor(), || {
            boxed(60).iter().map(ToString::to_string).collect()
        });
        assert_eq!(whole, itself);
    }

    /// The block and the box's own mark are never both on the screen, and
    /// never neither.
    #[test]
    fn there_is_one_mark_at_every_instant_of_the_hand_off() {
        let caret = crate::theme::caret();
        for step in 0..=40 {
            let p = step as f32 / 40.0;
            let screen = crate::theme::with(truecolor(), || {
                let boxed = boxed(60);
                let height = u16::try_from(boxed.len()).expect("a short box");
                let mut grid = Grid::new(60, height);
                draw(&mut grid, &boxed, 0, p);
                descending(&mut grid, (30, 0), 0, p);
                grid.lines()
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            let block = screen.contains(caret);
            let mark = screen.contains(crate::theme::spark());
            assert!(block != mark, "at p={p}: block {block}, mark {mark}");
        }
    }

    #[test]
    fn the_block_walks_from_where_it_was_to_the_boxs_own_mark() {
        let at = |p| {
            crate::theme::with(truecolor(), || {
                let mut grid = Grid::new(60, 6);
                descending(&mut grid, (30, 4), 0, p);
                grid.lines().iter().enumerate().find_map(|(row, line)| {
                    line.to_string()
                        .find(crate::theme::caret())
                        .map(|column| (column, row))
                })
            })
        };
        assert_eq!(at(DESCENT.0), Some((30, 4)), "it starts where the block is");
        assert_eq!(
            at(DESCENT.1),
            Some((usize::from(welcome::MARK.0), usize::from(welcome::MARK.1))),
            "and lands on the mark"
        );
        assert_eq!(at(0.99), None, "and the box's own has taken over by then");
    }
}

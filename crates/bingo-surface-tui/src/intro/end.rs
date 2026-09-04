//! Where the opening lands: the welcome box, with her in it.
//!
//! The last frame of the piece is this and nothing else, still. She sits on
//! the left, drawn out of the same characters the world was drawn out of; the
//! greeting, the help line and the cwd sit beside her; and one row under the
//! box is the caret the block became, which is the whole point of the five
//! seconds before it.
//!
//! Nothing here is wired into [`crate::welcome`] yet — M70 does that, and
//! takes this box's caret row with it, because by then the real composer is
//! standing there.

use super::grid::{Cell, Grid};
use super::mascot;
use crate::{paths, theme};

/// What the box says, which is what the box on a fresh session says today.
const GREETING: &str = "Welcome to bingo!";
const HELP: &str = "/help for help · /login codex to use a subscription";

/// How many cells she is given. Her crop is taller than it is wide and a cell
/// is twice as tall as it is wide, so fourteen rows of twenty columns is her
/// own shape — `20 × `[`mascot::SHAPE`]` / 2`.
///
/// Fewer than this and she stops being a face: the ears go first, then the
/// profile, and what is left is a warm smudge. A welcome box that is her
/// height is a large box, and it is the box a fresh session opens with; it
/// scrolls away like anything else the moment there is something above it.
const HER: (u16, u16) = (20, 13);
/// The gap between her and what the box says.
const GUTTER: u16 = 3;
/// How far the box's foot sits above the bottom of the screen, and the caret
/// row between them.
const FOOT: u16 = 3;

/// How tall the whole box is, borders included.
pub fn height() -> u16 {
    HER.1 + 2
}

/// Where the caret the block becomes ends up — the cell the block walks down
/// to in the last shot, and the one it stands in for ever after.
pub fn caret_at(size: (u16, u16)) -> (u16, u16) {
    (4, size.1.saturating_sub(FOOT).saturating_add(1))
}

/// Where the box's own middle is — what the border grows out from, and where
/// the block is when the last shot starts.
fn middle(size: (u16, u16)) -> (u16, u16) {
    let top = size.1.saturating_sub(FOOT + height());
    (size.0 / 2, top + height() / 2)
}

/// The box, drawn into `grid`. `reveal` runs from nothing at 0 to the whole
/// of it at 1: the border opens outward from the middle first, and what the
/// box says arrives once it has somewhere to stand.
pub fn draw(grid: &mut Grid, cwd: &str, reveal: f32) {
    let size = (grid.width(), grid.height());
    let reveal = match reveal.is_nan() {
        true => 0.0,
        false => reveal.clamp(0.0, 1.0),
    };
    let top = size.1.saturating_sub(FOOT + height());
    let (middle_x, _) = middle(size);
    let opened = (f32::from(middle_x) * reveal.min(1.0) * 1.35) as u16;
    border(grid, top, size.0, (middle_x, opened));
    if reveal >= SAID {
        her(grid, top);
        words(grid, top, size.0, cwd);
        caret(grid, size);
    }
}

/// How far the border has to be open before the box says anything.
const SAID: f32 = 0.7;

/// The box itself, opening from `middle` outward by `opened` columns.
fn border(grid: &mut Grid, top: u16, width: u16, (middle, opened): (u16, u16)) {
    if opened == 0 {
        return;
    }
    let set = theme::border();
    let left = middle.saturating_sub(opened);
    let right = (middle + opened).min(width.saturating_sub(1));
    let bottom = top + height() - 1;
    for x in left..=right {
        let corner = x == left && left == 0;
        let end = x == right && right == width.saturating_sub(1);
        edge(
            grid,
            x,
            top,
            rule(set.top_left, set.horizontal_top, set.top_right, corner, end),
        );
        edge(
            grid,
            x,
            bottom,
            rule(
                set.bottom_left,
                set.horizontal_bottom,
                set.bottom_right,
                corner,
                end,
            ),
        );
    }
    for y in top + 1..bottom {
        edge(grid, left, y, set.vertical_left);
        edge(grid, right, y, set.vertical_right);
    }
}

/// Which of a border's three horizontal parts stands at one column.
fn rule(
    start: &'static str,
    along: &'static str,
    finish: &'static str,
    at_start: bool,
    at_finish: bool,
) -> &'static str {
    match (at_start, at_finish) {
        (true, _) => start,
        (_, true) => finish,
        _ => along,
    }
}

fn edge(grid: &mut Grid, x: u16, y: u16, glyph: &'static str) {
    if let Some(glyph) = glyph.chars().next() {
        grid.set(
            x,
            y,
            Cell {
                glyph,
                style: theme::dim(),
            },
        );
    }
}

fn her(grid: &mut Grid, top: u16) {
    mascot::draw(grid, (2, top + 1), HER);
}

/// The greeting, the help line and the cwd, beside her.
fn words(grid: &mut Grid, top: u16, width: u16, cwd: &str) {
    let x = 2 + HER.0 + GUTTER;
    let room = usize::from(width.saturating_sub(x + 2));
    grid.write(x, top + 2, theme::spark(), theme::presence());
    grid.write(x + 2, top + 2, &cut(GREETING, room - 2), theme::text());
    grid.write(x + 2, top + 4, &cut(HELP, room - 2), theme::dim());
    let cwd = format!("cwd: {}", paths::short(cwd, "", paths::home()));
    grid.write(x + 2, top + 5, &cut(&cwd, room - 2), theme::dim());
}

/// One line of it, cut where the box ends.
fn cut(text: &str, room: usize) -> String {
    match text.chars().count() > room {
        true => {
            text.chars()
                .take(room.saturating_sub(1))
                .collect::<String>()
                + theme::ellipsis()
        }
        false => text.to_string(),
    }
}

/// The composer's own row: the caret the block came down to be. M70 replaces
/// this with the real input box, which is where the caret actually lives.
fn caret(grid: &mut Grid, size: (u16, u16)) {
    let (x, y) = caret_at(size);
    grid.write(x.saturating_sub(2), y, theme::user(), theme::dim());
    grid.write(x, y, theme::caret(), theme::presence());
}

/// The whole end state on a screen of its own, which is what a snapshot of it
/// reads and what the last frame of the opening is.
pub fn lines(width: u16, height: u16, cwd: &str) -> Vec<ratatui::text::Line<'static>> {
    let mut grid = Grid::new(width, height);
    draw(&mut grid, cwd, 1.0);
    grid.lines()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painted::{ascii, daylight, in_look, truecolor};

    fn drawn(width: u16, height: u16) -> String {
        lines(width, height, "/tmp/project")
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_end_state_is_a_box_with_her_in_it_at_both_sizes() {
        insta::assert_snapshot!("end_80x24", in_look(truecolor(), || drawn(80, 24)));
        insta::assert_snapshot!("end_120x40", in_look(truecolor(), || drawn(120, 40)));
    }

    #[test]
    fn the_light_palette_draws_the_same_box_in_its_own_ink() {
        insta::assert_snapshot!("end_daylight", in_look(daylight(), || drawn(80, 24)));
    }

    #[test]
    fn a_terminal_with_only_ascii_gets_the_silhouette_and_the_ascii_border() {
        let drawn = in_look(ascii(), || drawn(80, 24));
        assert!(drawn.contains("+---"), "the ASCII border:\n{drawn}");
        insta::assert_snapshot!("end_ascii", drawn);
    }

    #[test]
    fn the_box_opens_from_its_middle_and_says_nothing_until_it_has_room() {
        let shut = in_look(truecolor(), || {
            let mut grid = Grid::new(80, 24);
            draw(&mut grid, "/tmp/project", 0.0);
            grid.lines()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("")
        });
        assert!(shut.trim().is_empty(), "nothing at all at the start");

        let half = in_look(truecolor(), || {
            let mut grid = Grid::new(80, 24);
            draw(&mut grid, "/tmp/project", 0.45);
            grid.lines()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        });
        assert!(!half.contains(GREETING), "and no words yet:\n{half}");
        assert!(half.contains("──"), "but a border on its way:\n{half}");
    }

    #[test]
    fn the_caret_stands_where_the_block_came_down() {
        let (x, y) = caret_at((80, 24));
        let drawn = in_look(truecolor(), || drawn(80, 24));
        let row = drawn.lines().nth(usize::from(y)).expect("the caret's row");
        assert_eq!(row.chars().nth(usize::from(x)), Some('▌'), "{row:?}");
        assert!(row.trim_start().starts_with('>'), "{row:?}");
    }

    #[test]
    fn every_row_of_it_is_exactly_as_wide_as_the_screen() {
        for (width, height) in [(80u16, 24u16), (120, 40), (60, 20)] {
            let drawn = in_look(truecolor(), || drawn(width, height));
            for row in drawn.lines() {
                assert_eq!(row.chars().count(), usize::from(width), "{row:?}");
            }
        }
    }
}

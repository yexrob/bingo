//! The four beats composed over the box they land on.
//!
//! Nothing here draws a picture of the welcome box: it takes the box the
//! transcript would have drawn anyway and says, cell by cell, what the light
//! has done to that cell yet. So the resting frame is not a frame at all — it
//! is the box, and the piece cannot drift from it.

use ratatui::text::Line;

use super::beat::{self, Beat, END};
use super::cells::{self, Cell};
use super::lap::{self, Edge};
use crate::{clock, theme, welcome};

/// The frame `t` seconds in, over the welcome box as [`crate::welcome`] draws
/// it for the session in view.
///
/// Once the piece is over the box *is* the answer: there is no last frame that
/// merely looks like it.
pub fn frame(t: f32, boxed: &[Line<'static>]) -> Vec<Line<'static>> {
    if t >= END {
        return boxed.to_vec();
    }
    let rows: Vec<Vec<Cell>> = boxed.iter().map(cells::of).collect();
    let piece = Piece::at(t, &rows);
    rows.iter()
        .enumerate()
        .map(|(y, row)| cells::line(piece.row(y, row)))
        .collect()
}

/// One frame's worth of where the light is.
struct Piece {
    beat: Beat,
    /// How wide the box is, which is how the border grid is indexed.
    width: usize,
    /// The border as the line has drawn it so far, one entry per cell.
    edges: Vec<Edge>,
    /// The beam each row arrives under, and nothing for a row that has nothing
    /// to reveal.
    beams: Vec<Option<Beam>>,
}

/// One row arriving: how far its own beam has come, and how many columns it has
/// to cross.
///
/// To the row's last glyph, and no further: the beam crosses the words it
/// lights, as the one across bingo's working word does (`crate::view`), so a
/// short row arrives over the whole of its beat instead of in the first quarter
/// of it.
#[derive(Clone, Copy, Debug)]
struct Beam {
    come: f32,
    run: usize,
}

impl Piece {
    fn at(t: f32, rows: &[Vec<Cell>]) -> Self {
        let width = rows.iter().map(|row| measure(row)).max().unwrap_or(0);
        let beat = beat::beat(t);
        let edges = lap::lap(beat.line, width, rows.len());
        Piece {
            beams: beams(t, rows, &edges, width),
            beat,
            width,
            edges,
        }
    }

    /// One row of the frame, cell by cell along its own columns.
    fn row(&self, y: usize, cells: &[Cell]) -> Vec<Cell> {
        columns(cells)
            .map(|(x, cell)| self.cell((x, y), cell))
            .collect()
    }

    fn cell(&self, at: (usize, usize), cell: &Cell) -> Cell {
        match self.edge(at) {
            Edge::Dark => cell.blank(),
            Edge::Lit(age) => Cell {
                glyph: cell.glyph.clone(),
                style: theme::hairline(self.warmth(age)),
            },
            Edge::Interior => self.word(at, cell),
        }
    }

    fn edge(&self, (x, y): (usize, usize)) -> Edge {
        self.edges
            .get(y * self.width + x)
            .copied()
            .unwrap_or(Edge::Interior)
    }

    /// How much light stands on one cell of the border: what the line left
    /// behind as it passed, and the breath the whole border takes at the end.
    /// Both fall to nothing, which is the hairline it rests as.
    fn warmth(&self, age: f32) -> f32 {
        (1.0 - age).max(clock::swell(self.beat.breath) / 2.0)
    }

    /// One cell inside the border: the mark, a glyph the beam has reached, or
    /// nothing yet.
    fn word(&self, at: (usize, usize), cell: &Cell) -> Cell {
        // A cell nothing stands in has nothing to reveal: the padding a row is
        // squared off with, and the blank line under the greeting, are drawn as
        // they rest at every instant of the piece.
        if cell.blank_already() {
            return cell.clone();
        }
        if let Some(ignition) = self.beat.mark.filter(|_| at == mark()) {
            return self.ignited(ignition);
        }
        match self.beams.get(at.1).copied().flatten() {
            Some(beam) => beamed(at.0, beam, cell),
            None => cell.blank(),
        }
    }

    /// The mark igniting where the light came home: the sparkle's own four
    /// frames, and the light the head spent on it cooling to bingo's colour.
    fn ignited(&self, ignition: f32) -> Cell {
        Cell {
            glyph: theme::sparkle(beat::sparkling(ignition)).to_string(),
            style: theme::pulse(1.0 - ignition),
        }
    }
}

/// One glyph under the beam that reveals its row: blank ahead of the light, its
/// glow under it, and the row's own weight where it has passed — the recipe
/// `crate::view` lights bingo's working word with.
fn beamed(x: usize, beam: Beam, cell: &Cell) -> Cell {
    let column = x.saturating_sub(1);
    if !clock::swept(beam.come, column, beam.run) {
        return cell.blank();
    }
    Cell {
        glyph: cell.glyph.clone(),
        style: match clock::sweep(beam.come, column, beam.run) {
            lit if lit > 0.0 => theme::comet(1.0 - lit),
            _ => cell.style,
        },
    }
}

/// Where the box's own mark stands, as [`crate::welcome`] holds that fact.
fn mark() -> (usize, usize) {
    (usize::from(welcome::MARK.0), usize::from(welcome::MARK.1))
}

/// The beam each row arrives under. The rows that say something arrive in the
/// order they are read; a row that says nothing — a border, the blank line under
/// the greeting — waits for no beam and takes none of the beat.
fn beams(t: f32, rows: &[Vec<Cell>], edges: &[Edge], width: usize) -> Vec<Option<Beam>> {
    let runs: Vec<Option<usize>> = rows
        .iter()
        .enumerate()
        .map(|(y, cells)| reach(cells, y, edges, width))
        .collect();
    let saying: Vec<usize> = runs
        .iter()
        .enumerate()
        .filter_map(|(y, run)| run.map(|_| y))
        .collect();
    runs.iter()
        .enumerate()
        .map(|(y, run)| {
            Some(Beam {
                come: beat::row(t, saying.iter().position(|at| *at == y)?, saying.len()),
                run: (*run)?,
            })
        })
        .collect()
}

/// How far along its own inside a row's beam has to run: to the column its last
/// glyph stands in, and nothing at all for a row with no glyph inside the
/// border to reveal.
fn reach(cells: &[Cell], y: usize, edges: &[Edge], width: usize) -> Option<usize> {
    columns(cells)
        .filter(|(x, cell)| {
            !cell.blank_already() && matches!(edges.get(y * width + x), Some(Edge::Interior))
        })
        .map(|(x, _)| x)
        .last()
}

/// The columns a row's cells stand in, which a wide glyph takes two of.
fn columns(cells: &[Cell]) -> impl Iterator<Item = (usize, &Cell)> {
    cells.iter().scan(0usize, |x, cell| {
        let at = *x;
        *x += cell.width();
        Some((at, cell))
    })
}

fn measure(cells: &[Cell]) -> usize {
    cells.iter().map(Cell::width).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painted::{daylight, truecolor};
    use crate::test_support::state;

    const WIDE: u16 = 80;

    fn boxed(width: u16) -> Vec<Line<'static>> {
        welcome::lines(&state(), usize::from(width), None)
    }

    /// The frame as a person would read it off the screen: the glyphs alone.
    fn text(t: f32, width: u16) -> String {
        theme::with(truecolor(), || {
            frame(t, &boxed(width))
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    /// Two frames are the same drawing when every cell of them is: the same
    /// glyph in the same style. Stronger than comparing spans, which two runs
    /// of one style may be split into differently.
    fn same(left: &[Line<'static>], right: &[Line<'static>]) -> bool {
        let cells = |rows: &[Line<'static>]| rows.iter().map(cells::of).collect::<Vec<_>>();
        cells(left) == cells(right)
    }

    #[test]
    fn every_frame_is_the_boxs_own_shape_at_every_instant() {
        for width in [80u16, 120] {
            let boxed = boxed(width);
            for step in 0..=30 {
                let t = step as f32 / 10.0;
                let drawn = text(t, width);
                assert_eq!(drawn.lines().count(), boxed.len(), "at {t}s, {width} wide");
                for row in drawn.lines() {
                    assert_eq!(row.chars().count(), usize::from(width), "at {t}s: {row:?}");
                }
            }
        }
    }

    #[test]
    fn the_same_second_draws_the_same_frame() {
        for t in [0.0, 0.3, 0.9, 1.1, 1.5, 2.0, 2.4] {
            assert_eq!(text(t, WIDE), text(t, WIDE), "{t}");
        }
    }

    /// The exit criterion: what the piece lands on is the box itself, in both
    /// palettes and in both glyph tables — not a second drawing that looks
    /// like it.
    #[test]
    fn the_last_frame_is_the_welcome_box_and_nothing_else() {
        for look in [truecolor(), daylight(), crate::painted::ascii()] {
            theme::with(look, || {
                let boxed = boxed(WIDE);
                assert_eq!(frame(END, &boxed), boxed);
                assert_eq!(frame(9.0, &boxed), boxed, "and after it");
            });
        }
    }

    /// And the frame before it already *is* the box, so the piece does not
    /// finish on a jump: the breath is back at the hairline and every row has
    /// settled into its own weight.
    #[test]
    fn the_frame_before_the_last_one_is_already_the_box() {
        for look in [truecolor(), daylight()] {
            theme::with(look, || {
                let boxed = boxed(WIDE);
                let almost = frame(END - clock::FRAME.as_secs_f32(), &boxed);
                assert!(same(&almost, &boxed), "{almost:#?}");
            });
        }
    }

    /// The first beat: the border is drawn by one light and the inside of the
    /// box is empty until the mark ignites.
    #[test]
    fn the_border_arrives_before_anything_is_said_in_the_box() {
        let opening = text(0.0, WIDE);
        assert!(
            opening.lines().next().is_some_and(|row| row.trim() == "╭"),
            "the light starts on the corner alone:\n{opening}"
        );
        let running = text(0.45, WIDE);
        assert!(
            !running.contains("Welcome"),
            "nothing is said yet:\n{running}"
        );
        assert!(
            running.lines().next().is_some_and(|row| row.contains("──")),
            "and the top edge is being drawn:\n{running}"
        );
        let home = text(0.9, WIDE);
        assert_eq!(
            home.lines().last(),
            text(END, WIDE).lines().last(),
            "the whole border stands when the light is home"
        );
        assert!(!home.contains("Welcome"), "{home}");
    }

    /// The second: the mark alone inside a finished border.
    #[test]
    fn the_mark_ignites_where_the_light_came_home() {
        let lit = text(1.0, WIDE);
        assert!(lit.contains('✻'), "the mark is there:\n{lit}");
        assert!(!lit.contains("Welcome"), "and nothing else is:\n{lit}");
        for t in [0.9, 1.0, 1.2, 1.4] {
            let row = theme::with(truecolor(), || cells::of(&frame(t, &boxed(WIDE))[1]));
            let at = mark().0;
            assert!(
                theme::UNICODE.sparkles.contains(&row[at].glyph.as_str()),
                "at {t}s the mark wears a frame of the sparkle: {:?}",
                row[at].glyph
            );
        }
    }

    /// The third: the rows arrive one after another, each of them whole by the
    /// time the next has finished.
    #[test]
    fn the_rows_arrive_in_the_order_they_are_read() {
        let said = |t: f32| {
            text(t, WIDE)
                .lines()
                .map(|row| row.contains("Welcome") || row.contains("/help") || row.contains("cwd:"))
                .filter(|said| *said)
                .count()
        };
        assert_eq!(said(1.0), 0, "before the beam, none of them");
        assert!(said(1.4) >= 1, "the greeting is in first");
        assert!(said(1.4) <= 2, "and the cwd has not arrived yet");
        assert_eq!(said(2.0), 3, "by the end of the beat, all three");
    }

    /// The last: the border warms and comes back to the hairline it rests as,
    /// and no glyph moves while it does.
    #[test]
    fn the_border_breathes_once_and_nothing_else_moves() {
        assert_eq!(text(2.0, WIDE), text(2.2, WIDE), "the words are settled");
        theme::with(truecolor(), || {
            let boxed = boxed(WIDE);
            let corner = |t: f32| cells::of(&frame(t, &boxed)[0])[0].style;
            assert_eq!(corner(2.0), theme::dim(), "it starts at the hairline");
            assert_ne!(corner(2.175), theme::dim(), "and warms halfway through");
            assert_eq!(corner(2.35), theme::dim(), "then comes back to it");
            assert_eq!(corner(2.39), theme::dim(), "and rests there");
        });
    }

    /// A box with a release to announce has one more row, and it arrives last
    /// the same way — inside the same beat, which is what keeps the piece two
    /// and four tenths of a second long whatever the box has to say.
    #[test]
    fn a_newer_release_arrives_last_and_the_piece_is_no_longer_for_it() {
        let told = welcome::lines(&state(), usize::from(WIDE), Some("0.5.0"));
        let drawn = |t: f32| {
            theme::with(truecolor(), || {
                frame(t, &told)
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        };
        assert!(!drawn(1.4).contains("v0.5.0"), "not before the others");
        assert!(drawn(2.0).contains("v0.5.0"), "and there by the end");
        assert_eq!(frame(END, &told), told);
    }

    /// A box too narrow to have an inside still plays: there is a border to
    /// draw and nothing else, and every frame is the box's own shape.
    #[test]
    fn a_box_with_no_room_in_it_at_all_still_draws() {
        for width in [0u16, 1, 3] {
            let boxed = welcome::lines(&state(), usize::from(width), None);
            for t in [0.0, 0.5, 1.2, 2.0] {
                let drawn = theme::with(truecolor(), || frame(t, &boxed));
                assert_eq!(drawn.len(), boxed.len(), "{width} wide at {t}s");
            }
            assert_eq!(theme::with(truecolor(), || frame(END, &boxed)), boxed);
        }
    }

    /// A session this surface did not open has no box, and so no piece.
    #[test]
    fn a_session_with_no_welcome_box_has_nothing_to_play() {
        assert!(frame(0.5, &[]).is_empty());
    }
}

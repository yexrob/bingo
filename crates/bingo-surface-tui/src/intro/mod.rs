//! The opening shot: four seconds, three cuts, one block — played inside the
//! welcome box.
//!
//! `docs/design/tui.md` §11 is the storyboard in words and this is it in code.
//! A ray-marcher walks a world of signed distances ([`sdf`], [`march`]), the
//! light that lands is spent on a field of square pixels and packed two rows to
//! a cell of `▀` ([`shade`]), and the three shots are a table of worlds
//! ([`scenes`]). She is in the second of them ([`mascot`]), and the third hands
//! the screen to the box the block becomes the mark of ([`settle`]).
//!
//! The whole of it is a pure function of one number: the second of the piece a
//! frame is for. Nothing counts frames, so a frame that arrives late is
//! *skipped* rather than played slowly, and the same second is the same picture
//! on a fast machine and a slow one.
//!
//! What is not here: when it plays and who asks for the frames. That is
//! [`crate::run::opening`]'s, and the frames are rendered off the draw thread.

mod embers;
mod grid;
mod march;
mod mascot;
mod scenes;
mod sdf;
mod settle;
mod shade;

use std::time::Instant;

use ratatui::text::Line;

use crate::clock::Now;
use grid::Grid;
use scenes::Stage;

/// How many rows the box takes while the piece plays. Resolution is rows — a
/// row is two pixels — and twelve is the most a composer and a status line can
/// give up on a screen of sixteen.
pub const ROWS: u16 = 12;

/// When in the last shot the box starts settling to the height it rests at:
/// where the world has finished going out, so what shrinks away is empty.
const SETTLES: f32 = 0.62;

/// The frame `t` seconds in, over a box `width` wide, landing on `boxed` — the
/// welcome box as [`crate::welcome`] draws it for the session in view.
///
/// Once the piece is over the box *is* the answer: there is no last frame that
/// merely looks like it.
pub fn frame(t: f32, width: u16, boxed: &[Line<'static>]) -> Vec<Line<'static>> {
    let resting = u16::try_from(boxed.len()).unwrap_or(u16::MAX);
    if t >= scenes::END {
        return boxed.to_vec();
    }
    let staged = scenes::staged(t);
    let (mut field, _) = shade::pixels(&staged.scene, &staged.camera, width, tall(t, resting) * 2);
    embers::draw(&mut field, &staged.camera, &staged.scene);
    let mut grid = shade::halves(&field);
    let (shot, p) = scenes::shot(t);
    if shot.stage == Stage::HandOff {
        handing_over(&mut grid, &staged.camera, boxed, p);
    }
    grid.lines()
}

/// How tall the box is at `t`: [`ROWS`] while the world is on the screen, and
/// down to the height it rests at as the last shot closes.
pub fn tall(t: f32, resting: u16) -> u16 {
    let playing = ROWS.max(resting);
    let (shot, p) = scenes::shot(t);
    if shot.stage != Stage::HandOff {
        return playing;
    }
    let settling = crate::clock::ease_in_out(((p - SETTLES) / (1.0 - SETTLES)).clamp(0.0, 1.0));
    let shrunk = f32::from(playing) - (f32::from(playing) - f32::from(resting)) * settling;
    (shrunk.round() as u16).max(resting)
}

/// The world handing the screen over: the box arrives at the bottom of the
/// rows the piece is playing in, so what shrinks away is the world above it,
/// and the block walks down out of the air to be the box's own mark.
fn handing_over(grid: &mut Grid, camera: &march::Camera, boxed: &[Line<'static>], p: f32) {
    let resting = u16::try_from(boxed.len()).unwrap_or(u16::MAX);
    let top = grid.height().saturating_sub(resting);
    if let Some(from) = on_screen(grid, camera) {
        settle::descending(grid, from, top, p);
    }
    settle::draw(grid, boxed, top, p);
}

/// The cell the block stands in, or nothing at all when it is off the screen.
fn on_screen(grid: &Grid, camera: &march::Camera) -> Option<(u16, u16)> {
    let (u, v, _) = camera.project(scenes::handed_over())?;
    let (x, y) = shade::pixel_at(u, v, grid.width(), grid.height() * 2)?;
    Some((x, y / 2))
}

/// The opening as the surface holds it: when it started, and the newest frame
/// rendered for it.
///
/// The frames are rendered off the draw thread and the draw takes whichever it
/// has — so this is a memo of work already done and never a queue. A frame for
/// a width the screen no longer has is not a frame at all, which is why the
/// width it was rendered at is kept beside it.
#[derive(Debug)]
pub struct Playing {
    started: Instant,
    held: Option<Held>,
    /// A frame is being rendered. One at a time, however slow the machine: a
    /// second request would only race the first to be thrown away.
    rendering: bool,
}

#[derive(Debug)]
struct Held {
    width: u16,
    rows: Vec<Line<'static>>,
}

impl Playing {
    pub fn from(now: Instant) -> Self {
        Playing {
            started: now,
            held: None,
            rendering: false,
        }
    }

    /// Which second of the piece this frame is for.
    pub fn seconds(&self, now: Now) -> f32 {
        now.since(self.started).as_secs_f32()
    }

    /// Whether the piece has played out.
    pub fn over(&self, now: Now) -> bool {
        self.seconds(now) >= scenes::END
    }

    /// The newest frame, if it was rendered for the width the box has now.
    /// Nothing at all until the first one lands, which is the black the piece
    /// opens on anyway.
    pub fn rows(&self, width: u16) -> Option<&[Line<'static>]> {
        self.held
            .as_ref()
            .filter(|held| held.width == width)
            .map(|held| held.rows.as_slice())
    }

    /// Whether a frame should be asked for: none is on its way.
    pub fn wants(&self) -> bool {
        !self.rendering
    }

    /// One has been asked for.
    pub fn asked(&mut self) {
        self.rendering = true;
    }

    /// One has come back.
    pub fn landed(&mut self, width: u16, rows: Vec<Line<'static>>) {
        self.rendering = false;
        self.held = Some(Held { width, rows });
    }
}

/// The frames the shots are reviewed from.
#[cfg(test)]
mod storyboard;

/// One frame of the opening, named — the storyboard's own snapshots land under
/// this module rather than under the test that writes them, because they are
/// the record of the shots and not of the test.
#[cfg(test)]
fn snapshot(name: &str, drawn: String) {
    insta::assert_snapshot!(name, drawn);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painted::{in_look, truecolor};
    use crate::test_support::{later, scene, state};

    const WIDE: u16 = 100;

    fn boxed(width: u16) -> Vec<Line<'static>> {
        crate::welcome::lines(&state(), usize::from(width), None)
    }

    fn text(t: f32) -> String {
        in_look(truecolor(), || {
            frame(t, WIDE, &boxed(WIDE))
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    #[test]
    fn every_frame_is_as_wide_as_the_box_and_as_tall_as_the_piece_asks() {
        let resting = u16::try_from(boxed(WIDE).len()).expect("a short box");
        for step in 0..=40 {
            let t = step as f32 / 10.0;
            let drawn = text(t);
            assert_eq!(
                drawn.lines().count(),
                usize::from(tall(t, resting)),
                "at {t}s"
            );
            for row in drawn.lines() {
                assert_eq!(row.chars().count(), usize::from(WIDE), "at {t}s: {row:?}");
            }
        }
    }

    #[test]
    fn the_box_is_twelve_rows_while_it_plays_and_its_own_height_at_the_end() {
        let resting = u16::try_from(boxed(WIDE).len()).expect("a short box");
        assert_eq!(tall(0.0, resting), ROWS);
        assert_eq!(tall(2.0, resting), ROWS);
        assert_eq!(tall(3.0, resting), ROWS, "it plays out at its full height");
        assert!(tall(3.7, resting) < ROWS, "then it settles");
        assert_eq!(tall(scenes::END, resting), resting);
        assert_eq!(tall(9.0, resting), resting, "and stays there");
    }

    #[test]
    fn the_same_second_draws_the_same_frame() {
        for t in [0.0, 0.7, 1.4, 2.5, 3.6, 4.0] {
            assert_eq!(text(t), text(t), "{t}");
        }
    }

    /// The exit criterion: what the piece lands on is the box itself, both
    /// palettes — not a second drawing that looks like it.
    #[test]
    fn the_last_frame_is_the_welcome_box_and_nothing_else() {
        for look in [truecolor(), crate::painted::daylight()] {
            crate::theme::with(look, || {
                let boxed = boxed(WIDE);
                assert_eq!(frame(scenes::END, WIDE, &boxed), boxed);
                assert_eq!(frame(9.0, WIDE, &boxed), boxed, "and after it");
            });
        }
    }

    /// One frame before the end the world is already gone and the box is
    /// whole, so the piece does not finish on a jump.
    #[test]
    fn the_frame_before_the_last_one_already_reads_as_the_box() {
        let almost = text(scenes::END - crate::clock::FRAME.as_secs_f32());
        let boxed = in_look(truecolor(), || {
            boxed(WIDE)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        });
        assert_eq!(almost, boxed);
    }

    #[test]
    fn a_box_with_no_room_in_it_at_all_still_draws() {
        for width in [0u16, 1, 3] {
            let boxed = crate::welcome::lines(&state(), usize::from(width), None);
            let resting = u16::try_from(boxed.len()).expect("a short box");
            let drawn = crate::theme::with(truecolor(), || frame(1.0, width, &boxed));
            assert_eq!(drawn.len(), usize::from(tall(1.0, resting)));
        }
    }

    /// What the surface holds: a frame lands, is drawn, and is dropped the
    /// moment the screen is a different width.
    #[test]
    fn a_held_frame_is_drawn_only_at_the_width_it_was_rendered_for() {
        let (_, now) = scene();
        let mut playing = Playing::from(now.instant);
        assert_eq!(playing.rows(80), None, "nothing has landed yet");
        assert!(playing.wants(), "so one is wanted");
        playing.asked();
        assert!(!playing.wants(), "and only one at a time");
        playing.landed(80, vec![Line::from("a frame")]);
        assert!(playing.wants(), "the next one may go");
        assert_eq!(playing.rows(80).map(<[Line]>::len), Some(1));
        assert_eq!(playing.rows(120), None, "not at another width");
    }

    #[test]
    fn the_piece_is_over_when_its_seconds_have_run_out() {
        let (_, now) = scene();
        let playing = Playing::from(now.instant);
        assert!(!playing.over(now));
        assert!(!playing.over(later(now, 3_900)));
        assert!(playing.over(later(now, 4_000)));
    }
}

//! The opening shot: five seconds, five cuts, one block.
//!
//! `docs/design/tui.md` §11 is the storyboard in words and this is it in
//! code. A ray-marcher walks a world of signed distances ([`sdf`], [`march`]),
//! the light that lands is spent on a luminance ramp and the theme's own
//! tokens ([`shade`]), and the five shots are a table of worlds ([`scenes`]).
//! It ends on her ([`mascot`]) and on the welcome box the block becomes the
//! cursor of ([`end`]).
//!
//! The whole of it is a pure function of one number: the second of the piece
//! a frame is for. Nothing counts frames, so a frame that arrives late is
//! *skipped* rather than played slowly, and the same second is the same
//! picture on a fast machine and a slow one.
//!
//! This milestone is the brick and the storyboard. Nothing draws it yet:
//! M70 wires it into the welcome box, with the skip, the short form and the
//! settings.

mod embers;
mod end;
mod grid;
mod march;
mod mascot;
mod scenes;
mod sdf;
mod shade;

use ratatui::text::Line;

use scenes::Stage;
use shade::Rendered;

/// One frame of the opening, `t` seconds in.
pub fn frame(t: f32, size: (u16, u16), cwd: &str) -> Rendered {
    let staged = scenes::staged(t);
    let mut rendered = shade::render(&staged.scene, &staged.camera, size.0, size.1);
    embers::draw(&mut rendered, &staged.camera, &staged.scene);
    if scenes::shot(t).0.stage == Stage::HandOff {
        handing_over(&mut rendered, &staged.camera, scenes::shot(t).1, cwd);
    }
    rendered
}

/// The same, as the rows a terminal draws.
pub fn at(t: f32, size: (u16, u16), cwd: &str) -> Vec<Line<'static>> {
    frame(t, size, cwd).grid.lines()
}

/// The world flattening into the box: the block walks down out of the air to
/// where the composer's cursor will be, and the box opens behind it.
fn handing_over(rendered: &mut Rendered, camera: &march::Camera, p: f32, cwd: &str) {
    let size = (rendered.grid.width(), rendered.grid.height());
    end::draw(&mut rendered.grid, cwd, (p - 0.2) / 0.8);
    descending(rendered, camera, p, size);
}

/// The block on its way down. It is drawn only between leaving the air and
/// arriving: before [`DESCENT`] opens, the block a person is looking at is
/// the one in the world; after it closes, it is the box's own caret. One
/// mark on the screen at every instant, and never two.
fn descending(rendered: &mut Rendered, camera: &march::Camera, p: f32, size: (u16, u16)) {
    if !(DESCENT.0..DESCENT.1).contains(&p) {
        return;
    }
    let Some((u, v, _)) = camera.project(scenes::handed_over()) else {
        return;
    };
    let Some(from) = shade::cell_at(u, v, size.0, size.1) else {
        return;
    };
    let walked = crate::clock::ease_in_out((p - DESCENT.0) / (DESCENT.1 - DESCENT.0));
    let to = end::caret_at(size);
    rendered.grid.write(
        stepped(from.0, to.0, walked),
        stepped(from.1, to.1, walked),
        crate::theme::caret(),
        crate::theme::lit(1.0, 1.0),
    );
}

/// When in the last shot the block leaves the air and when it arrives.
const DESCENT: (f32, f32) = (0.25, 0.85);

fn stepped(from: u16, to: u16, t: f32) -> u16 {
    let walked = f32::from(from) + (f32::from(to) - f32::from(from)) * t;
    walked.round().clamp(0.0, f32::from(u16::MAX)) as u16
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::painted::{in_look, truecolor};

    fn text(t: f32) -> String {
        in_look(truecolor(), || {
            at(t, (60, 18), "/tmp/project")
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        })
    }

    #[test]
    fn every_frame_is_the_size_it_was_asked_for() {
        for step in 0..=50 {
            let drawn = text(step as f32 / 10.0);
            assert_eq!(drawn.lines().count(), 18, "{step}");
            for row in drawn.lines() {
                assert_eq!(row.chars().count(), 60, "{step}: {row:?}");
            }
        }
    }

    #[test]
    fn the_same_second_draws_the_same_frame() {
        for t in [0.0, 0.7, 1.4, 2.5, 3.6, 4.8, 5.0] {
            assert_eq!(text(t), text(t), "{t}");
        }
    }

    #[test]
    fn the_last_frame_is_the_welcome_box_and_nothing_else() {
        let last = text(scenes::END);
        let box_alone = in_look(truecolor(), || {
            end::lines(60, 18, "/tmp/project")
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        });
        assert_eq!(last, box_alone);
    }

    #[test]
    fn a_screen_with_no_room_in_it_at_all_still_draws() {
        for size in [(1u16, 1u16), (0, 0), (200, 1)] {
            let drawn = crate::theme::with(truecolor(), || at(2.5, size, "/tmp"));
            assert_eq!(drawn.len(), usize::from(size.1));
        }
    }
}

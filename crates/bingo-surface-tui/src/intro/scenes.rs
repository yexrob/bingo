//! The five shots, and where each one is cut.
//!
//! `docs/design/tui.md` §11 is this table in words. Every cut is a hard one:
//! there is no dissolve anywhere in the opening, because a five-second piece
//! has no time to spend on one and because a cut is what says *elsewhere* in
//! the language a person already reads films in.
//!
//! Nothing here draws. A shot is a world and a place to look at it from, at
//! one instant, and it is a pure function of that instant — so a frame that
//! arrives late is skipped rather than slowing the piece down, and the same
//! second of the piece is the same picture on every machine.

use std::f32::consts::PI;

use super::embers::{self, Rising};
use super::march::{Camera, Lamp, Scene, Solid};
use super::{mascot, sdf};
use sdf::{Shape, Vec3, at};

/// One of the five.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shot {
    /// The second the cut to it happens on.
    pub at_second: f32,
    /// What it is called in the design doc and in the storyboard's file names.
    pub name: &'static str,
    pub stage: Stage,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Dark,
    Lattice,
    Find,
    Her,
    HandOff,
}

/// The cuts. Five shots over five seconds, and the piece is over.
pub const SHOTS: [Shot; 5] = [
    Shot {
        at_second: 0.0,
        name: "dark",
        stage: Stage::Dark,
    },
    Shot {
        at_second: 1.0,
        name: "lattice",
        stage: Stage::Lattice,
    },
    Shot {
        at_second: 2.2,
        name: "find",
        stage: Stage::Find,
    },
    Shot {
        at_second: 3.2,
        name: "her",
        stage: Stage::Her,
    },
    Shot {
        at_second: 4.5,
        name: "handoff",
        stage: Stage::HandOff,
    },
];

/// When the last frame is. Nothing is drawn after it; the welcome box is.
pub const END: f32 = 5.0;

/// Which shot `t` seconds falls in, and how far through it — 0 at the cut to
/// it, 1 at the cut away.
pub fn shot(t: f32) -> (&'static Shot, f32) {
    let t = held(t);
    let which = SHOTS
        .iter()
        .rposition(|shot| t >= shot.at_second)
        .unwrap_or(0);
    let shot = &SHOTS[which];
    let until = SHOTS.get(which + 1).map_or(END, |next| next.at_second);
    let span = (until - shot.at_second).max(f32::EPSILON);
    (shot, ((t - shot.at_second) / span).clamp(0.0, 1.0))
}

/// A second of the piece, held inside it — before the start is the first
/// frame and after the end is the last, so no caller has to clamp its clock.
fn held(t: f32) -> f32 {
    match t.is_nan() {
        true => 0.0,
        false => t.clamp(0.0, END),
    }
}

/// A world and where it is seen from.
#[derive(Clone, Debug, PartialEq)]
pub struct Staged {
    pub scene: Scene,
    pub camera: Camera,
}

/// The world at `t` seconds.
pub fn staged(t: f32) -> Staged {
    let (shot, p) = shot(t);
    match shot.stage {
        Stage::Dark => dark(t, p),
        Stage::Lattice => lattice(p),
        Stage::Find => find(p),
        Stage::Her => her(t, p),
        Stage::HandOff => hand_off(t, p),
    }
}

// ---- the block ----------------------------------------------------------

/// The one character: a tall thin slab, the shape of a composer's caret.
/// Six times as tall as it is wide, which is what a `▌` is.
fn block(centre: Vec3, height: f32, spin: f32) -> Solid {
    Solid::of(Shape::Block {
        at: centre,
        half: at(height / 6.0, height, height / 6.0),
        round: 0.02,
        spin,
    })
    .lit()
}

/// The light that block is.
fn lamp(centre: Vec3, reach: f32, strength: f32) -> Lamp {
    Lamp {
        at: centre,
        reach,
        strength,
    }
}

/// Where the world's own light comes from: up, and over the left shoulder.
const SUN: Vec3 = at(-0.45, 0.78, -0.44);

// ---- 0.0–1.0 · dark -----------------------------------------------------

/// A black world, one block turning in it, six embers rising. Everything the
/// piece is about is on screen and nothing else is.
fn dark(t: f32, p: f32) -> Staged {
    let centre = at(0.0, 0.0, 0.0);
    let rising = Rising {
        from: at(0.0, -1.25, -0.35),
        spread: 0.42,
        span: 2.4,
        rate: 0.34,
    };
    Staged {
        scene: Scene {
            solids: vec![
                Solid::of(Shape::Ground { y: -1.15 }),
                block(centre, 0.46, 0.55 + p * 0.95),
            ],
            lamp: lamp(centre, 1.7, 1.0),
            sun: SUN,
            sky: 0.04,
            fog: 0.42,
            embers: embers::at_time(rising, t),
            ..Scene::default()
        },
        camera: Camera {
            eye: at(0.0, 0.26, -2.5 + p * 0.2),
            at: at(0.0, -0.05, 0.0),
            lens: 0.62,
        },
    }
}

// ---- 1.0–2.2 · the lattice ----------------------------------------------

/// How far apart the frames of the lattice stand.
const CELL: f32 = 3.0;

/// The codebase: square frames without end, one at every point of a grid —
/// one lattice of blocks with a second, longer one taken out of it, so every
/// block is a window and the run of them is a corridor. One shape and one
/// cut, however far it goes.
///
/// The hole is wider than the lens sees at a frame's distance; a tighter one
/// puts a wall across each side of the screen and the shot reads as a room
/// rather than as something without end.
fn frames() -> Solid {
    let period = at(CELL, CELL, CELL);
    Solid::of(Shape::Lattice {
        period,
        half: at(1.22, 1.22, 0.30),
        round: 0.05,
    })
    .less(Shape::Lattice {
        period,
        half: at(0.97, 0.97, 8.0),
        round: 0.03,
    })
}

/// Inside it, dollying forward fast. The cursor block is not here yet: the
/// only light is the world's own, and what a person reads is the edges of the
/// frames coming past and the far end going into fog.
fn lattice(p: f32) -> Staged {
    let eye = at(
        0.16 * (p * 3.3).sin(),
        0.11 * (p * 2.6 + 1.0).cos(),
        -7.5 + p * 17.0,
    );
    Staged {
        scene: Scene {
            solids: vec![frames()],
            lamp: lamp(at(0.0, 0.0, 0.0), 1.0, 0.0),
            sun: SUN,
            sky: 1.0,
            fog: 0.105,
            ..Scene::default()
        },
        camera: Camera {
            eye,
            at: eye.plus(at(0.05 * (p * 2.1).sin(), 0.04 * (p * 1.7).cos(), 1.0)),
            lens: 0.66,
        },
    }
}

// ---- 2.2–3.2 · the find -------------------------------------------------

/// Where the block is found, four frames down the corridor.
const FOUND: Vec3 = at(0.0, 0.0, 6.0);

/// The dolly stops dead. One block in the lattice lights from inside and
/// everything around it goes into the fog. Nothing moves but the light and
/// the fog: half a second of stillness is what makes the cut to her land.
fn find(p: f32) -> Staged {
    let coming = (p / 0.28).clamp(0.0, 1.0);
    Staged {
        scene: Scene {
            solids: vec![frames(), block(FOUND, 0.42, 0.35)],
            lamp: lamp(FOUND, 2.6, crate::clock::ease_out(coming)),
            sun: SUN,
            sky: 1.0 - 0.62 * p,
            fog: 0.105 + 0.30 * p,
            ..Scene::default()
        },
        camera: Camera {
            eye: at(0.42, 0.26, -0.2),
            at: FOUND,
            lens: 0.66,
        },
    }
}

// ---- 3.2–4.5 · her ------------------------------------------------------

/// How wide she stands in the world, and where.
const HER_WIDTH: f32 = 2.3;
const HER: Vec3 = at(1.05, 0.15, 3.6);
/// The block, hanging before her face — much nearer the camera than she is,
/// which is what gives the drift something to be parallax *of*.
const BEFORE_HER: Vec3 = at(-1.05, 0.02, 1.15);

/// Her, on a billboard in the world, with the block before her face and the
/// embers rising between them; the camera drifts slowly across.
fn her(t: f32, p: f32) -> Staged {
    let rising = Rising {
        from: at(-0.2, -1.6, 2.0),
        spread: 0.8,
        span: 3.4,
        rate: 0.30,
    };
    // One breath over the whole shot: in, and out again by the cut.
    let breath = 1.0 + 0.15 * (p * PI).sin();
    Staged {
        scene: Scene {
            solids: vec![
                Solid::of(Shape::Block {
                    at: HER,
                    half: at(HER_WIDTH, HER_WIDTH * mascot::SHAPE, 0.03),
                    round: 0.0,
                    spin: 0.0,
                })
                .pictured(),
                block(BEFORE_HER, 0.42 * breath, 0.30 + p * 0.22),
            ],
            lamp: lamp(BEFORE_HER, 2.7, 0.85 + 0.15 * (p * PI).sin()),
            sun: SUN,
            sky: 0.06,
            fog: 0.055,
            embers: embers::at_time(rising, t),
            ..Scene::default()
        },
        camera: drifting(p),
    }
}

/// The slow cross-drift of the fourth shot, which the fifth settles out of.
fn drifting(p: f32) -> Camera {
    Camera {
        eye: at(
            -0.08 + 0.42 * (p * 1.9 - 0.7).sin(),
            0.10 + 0.15 * (p * 1.3).sin(),
            -2.05 + 0.28 * p,
        ),
        at: at(0.20, 0.05, 1.8),
        lens: 0.66,
    }
}

// ---- 4.5–5.0 · the hand-off ---------------------------------------------

/// Where the camera comes to rest: square on, on the block.
fn frontal() -> Camera {
    Camera {
        eye: at(0.0, 0.08, -1.9),
        at: at(0.0, 0.05, 1.8),
        lens: 0.66,
    }
}

/// The camera settles frontal and the world goes out, leaving the box that
/// was underneath it all along. What is drawn *over* this is
/// [`super::end`]'s; the world's part of the shot is only to stop moving and
/// to fade.
fn hand_off(t: f32, p: f32) -> Staged {
    let settling = crate::clock::ease_in_out(p.clamp(0.0, 1.0));
    let staged = her(t, 1.0);
    let from = drifting(1.0);
    let to = frontal();
    Staged {
        scene: Scene {
            // The world goes out over the first two thirds, so the last third
            // is the box alone and the final frame is exactly it.
            exposure: 1.0 - (p / 0.66).clamp(0.0, 1.0),
            ..staged.scene
        },
        camera: Camera {
            eye: between(from.eye, to.eye, settling),
            at: between(from.at, to.at, settling),
            lens: from.lens,
        },
    }
}

fn between(from: Vec3, to: Vec3, t: f32) -> Vec3 {
    from.plus(to.minus(from).times(t))
}

/// Where the block stands in the world as the last shot opens — what
/// [`super::end`] takes over, and walks down to the caret.
pub fn handed_over() -> Vec3 {
    BEFORE_HER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cuts_are_where_the_design_says_they_are() {
        let cuts: Vec<f32> = SHOTS.iter().map(|shot| shot.at_second).collect();
        assert_eq!(cuts, vec![0.0, 1.0, 2.2, 3.2, 4.5]);
        assert_eq!(END, 5.0);
    }

    #[test]
    fn every_second_of_the_piece_falls_in_exactly_one_shot() {
        let mut seen = Vec::new();
        for step in 0..=500 {
            let (shot, p) = shot(step as f32 / 100.0);
            assert!((0.0..=1.0).contains(&p), "{step}: {p}");
            if seen.last() != Some(&shot.name) {
                seen.push(shot.name);
            }
        }
        assert_eq!(seen, vec!["dark", "lattice", "find", "her", "handoff"]);
    }

    #[test]
    fn a_cut_is_the_first_frame_of_the_shot_it_cuts_to() {
        for shot_at in SHOTS.iter().skip(1) {
            let (landed, p) = shot(shot_at.at_second);
            assert_eq!(landed.name, shot_at.name);
            assert_eq!(p, 0.0, "{}", shot_at.name);
        }
    }

    #[test]
    fn a_clock_outside_the_piece_is_held_at_its_ends() {
        assert_eq!(shot(-4.0).0.name, "dark");
        assert_eq!(shot(-4.0).1, 0.0);
        assert_eq!(shot(9.0).0.name, "handoff");
        assert_eq!(shot(9.0).1, 1.0);
        assert_eq!(shot(f32::NAN).0.name, "dark", "and a clock that broke");
    }

    #[test]
    fn the_same_second_is_the_same_world_however_often_it_is_asked() {
        for step in 0..50 {
            let t = step as f32 / 10.0;
            assert_eq!(staged(t), staged(t), "{t}");
        }
    }

    #[test]
    fn the_block_is_lit_wherever_it_is_and_the_lattice_never_is() {
        use crate::intro::march::Material;
        let emissive = |t: f32| {
            staged(t)
                .scene
                .solids
                .iter()
                .filter(|solid| solid.material == Material::Emissive)
                .count()
        };
        assert_eq!(emissive(0.5), 1, "the block establishes the piece");
        assert_eq!(emissive(1.6), 0, "and is not in the lattice yet");
        assert_eq!(emissive(2.8), 1, "until it is found");
        assert_eq!(emissive(3.9), 1);
    }

    #[test]
    fn the_lattice_shot_has_no_lamp_to_light_it() {
        assert_eq!(staged(1.6).scene.lamp.strength, 0.0);
        assert!(
            staged(2.4).scene.lamp.strength > 0.0,
            "the find turns it on"
        );
    }

    #[test]
    fn the_dolly_runs_forward_through_the_lattice_and_then_stops_dead() {
        let eye = |t: f32| staged(t).camera.eye.z;
        assert!(eye(1.05) < eye(1.6) && eye(1.6) < eye(2.15), "it dollies");
        assert!((eye(2.3) - eye(3.1)).abs() < 1e-5, "and then holds still");
    }

    #[test]
    fn the_last_frame_of_the_piece_has_no_world_left_in_it() {
        assert_eq!(staged(END).scene.exposure, 0.0);
        assert!(
            staged(4.55).scene.exposure > 0.8,
            "and it was there a moment before"
        );
    }

    #[test]
    fn the_embers_rise_only_where_there_is_someone_to_see_them() {
        assert_eq!(staged(0.5).scene.embers.len(), embers::COUNT);
        assert!(staged(1.6).scene.embers.is_empty(), "not in the corridor");
        assert!(staged(2.8).scene.embers.is_empty(), "not on the find");
        assert_eq!(staged(3.9).scene.embers.len(), embers::COUNT);
    }
}

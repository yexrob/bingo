//! The three shots, and where each one is cut.
//!
//! `docs/design/tui.md` §11 is this table in words. Every cut is a hard one:
//! there is no dissolve anywhere in the opening, because a four-second piece
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

/// One of the three.
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
    Floor,
    Field,
    HandOff,
}

/// The cuts. Three shots over four seconds, and the piece is over.
pub const SHOTS: [Shot; 3] = [
    Shot {
        at_second: 0.0,
        name: "floor",
        stage: Stage::Floor,
    },
    Shot {
        at_second: 1.4,
        name: "field",
        stage: Stage::Field,
    },
    Shot {
        at_second: 2.8,
        name: "handoff",
        stage: Stage::HandOff,
    },
];

/// When the last frame is. Nothing is drawn after it; the welcome box is.
pub const END: f32 = 4.0;

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
        Stage::Floor => floor(t, p),
        Stage::Field => field(p),
        Stage::HandOff => hand_off(p),
    }
}

// ---- the pieces every shot is built out of ------------------------------

/// The one character: a tall thin slab, the shape of a composer's caret — six
/// times as tall as it is broad, measured in the square pixels the world is
/// drawn in.
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

/// Where the world's own light comes from: up, over the left shoulder, and
/// from *behind* what it lights — so the shadow it throws falls towards the
/// camera, where a person can see it, instead of hiding behind the thing that
/// cast it. A unit vector, pointing at the sun.
const SUN: Vec3 = at(-0.38, 0.72, 0.58);

/// How wide the lens is. One number for the whole piece: a cut that also
/// changed the focal length would read as two cuts.
const LENS: f32 = 0.6;

/// The camera the block is looked at from, at an angle about it and a height
/// above it.
fn about(pivot: Vec3, angle: f32, high: f32, away: f32) -> Camera {
    Camera {
        eye: pivot.plus(at(away * angle.sin(), high, -away * angle.cos())),
        at: pivot,
        lens: LENS,
    }
}

// ---- 0.0–1.4 · the floor ------------------------------------------------

/// How far below the block the ground lies, and where the block hangs.
const GROUND: f32 = -1.55;
const STANDING: Vec3 = at(0.0, -0.30, 0.0);
/// How tall the block stands, from its middle. Its foot hangs just clear of
/// the floor, which is what its shadow is a shadow of.
const TALL: f32 = 1.2;

/// The dust that rises through the block's light.
const DUST: Rising = Rising {
    from: at(0.0, -1.4, -0.4),
    spread: 0.8,
    span: 2.4,
    rate: 0.3,
};

/// A ruled floor running away to a vanishing point, the block hanging over it
/// with its shadow on it, and the camera dollying in low. The rules and the
/// shadow are the whole of what says *there is a room here* — fog on its own
/// is not depth.
fn floor(t: f32, p: f32) -> Staged {
    let dolly = crate::clock::ease_out(p);
    Staged {
        scene: Scene {
            solids: vec![
                Solid::of(Shape::Ground { y: GROUND }).ruled(),
                block(STANDING, TALL, 0.35 + p * 1.5),
            ],
            lamp: lamp(STANDING, 1.5, 1.0),
            sun: SUN,
            sky: 0.5,
            fog: 0.30,
            embers: embers::at_time(DUST, t),
            ..Scene::default()
        },
        // High enough over the floor that its rules converge on a vanishing
        // point rather than closing into one band, and coming down as it
        // dollies in.
        camera: Camera {
            eye: at(0.0, 0.75 - dolly * 0.25, -4.2 + dolly * 1.2),
            at: at(0.0, -0.45, 0.5),
            lens: LENS,
        },
    }
}

// ---- 1.4–2.8 · the field ------------------------------------------------

/// How far the camera stands off the block while it orbits it, and the height
/// it comes down from and to.
const AWAY: f32 = 2.1;
const HIGH: (f32, f32) = (0.55, 0.12);
/// The half turn, in radians about the block: from behind it to nearly square
/// on, which is what the last shot settles out of.
const TURN: (f32, f32) = (-PI, 0.34);

/// The field of dark blocks, at two depths: a near grid the orbit sweeps past
/// and a coarser, larger one behind it that barely moves. Both stand off half
/// a period, so the air the camera turns through is clear and the emissive
/// block has the middle of the world to itself.
fn floating() -> [Solid; 2] {
    [
        lattice(at(5.5, 4.4, 5.5), 0.42),
        lattice(at(9.0, 7.0, 9.0), 1.10),
    ]
}

fn lattice(period: Vec3, half: f32) -> Solid {
    Solid::of(Shape::Lattice {
        at: period.times(0.5),
        period,
        half: at(half, half, half),
        round: 0.05,
    })
}

/// Where she stands: behind the block from the camera's own last position, and
/// off to the right of it, so the block hangs before her face.
const HER: Vec3 = at(1.5, 0.06, 2.5);
/// How wide the billboard she is on is, from its middle. Her crop is taller
/// than it is wide ([`mascot::SHAPE`]) and the billboard is built to that, so
/// she is never stretched — and this is the width at which her head fills the
/// box's own height at the distance the last shot leaves her.
const HER_WIDTH: f32 = 2.05;

/// The camera orbits the block half a turn while a field of dark blocks sweeps
/// past it at two depths — the near ones fast, the far ones barely at all —
/// and she comes into view as the turn ends. The block is the only light in
/// this world: what a person reads is which faces caught it.
fn field(p: f32) -> Staged {
    let turned = crate::clock::ease_in_out(p);
    let mut solids = vec![her(), block(at(0.0, 0.0, 0.0), 0.75, 0.2 + p * 1.1)];
    solids.extend(floating());
    Staged {
        scene: Scene {
            solids,
            lamp: lamp(at(0.0, 0.0, 0.0), 2.2, 1.0),
            sun: SUN,
            // No sky at all: the block is the only light, so a face that is
            // turned away from it is not there, and every shadow walk the sun
            // would have cost is never taken.
            sky: 0.0,
            fog: 0.10,
            ..Scene::default()
        },
        camera: orbiting(turned),
    }
}

/// Her billboard: a plane in the world, its normal down the world's `z`, so
/// what a ray finds on it is read straight off the picture.
fn her() -> Solid {
    Solid::of(Shape::Block {
        at: HER,
        half: at(HER_WIDTH, HER_WIDTH * mascot::SHAPE, 0.03),
        round: 0.0,
        spin: 0.0,
    })
    .pictured()
}

/// The orbit, `turned` of the way through it.
fn orbiting(turned: f32) -> Camera {
    about(
        at(0.0, 0.0, 0.0),
        TURN.0 + (TURN.1 - TURN.0) * turned,
        HIGH.0 + (HIGH.1 - HIGH.0) * turned,
        AWAY,
    )
}

// ---- 2.8–4.0 · the hand-off ---------------------------------------------

/// Where the camera comes to rest: square on, on the block, with her behind
/// it. The last frame of the world before the box takes the screen.
fn frontal() -> Camera {
    about(at(0.0, 0.0, 0.0), 0.0, HIGH.1, AWAY)
}

/// How much of the shot the world has to fade over. The rest of it is the box
/// alone, which is what makes the last frame exactly the box.
const FADES_BY: f32 = 0.62;

/// The camera settles frontal and the world goes out, leaving the box that
/// was underneath it all along. What is drawn *over* this is
/// [`super::settle`]'s; the world's part of the shot is only to stop turning
/// and to fade.
fn hand_off(p: f32) -> Staged {
    let settling = crate::clock::ease_in_out(p.clamp(0.0, 1.0));
    let staged = field(1.0);
    let from = orbiting(1.0);
    let to = frontal();
    Staged {
        scene: Scene {
            exposure: 1.0 - (p / FADES_BY).clamp(0.0, 1.0),
            ..staged.scene
        },
        camera: Camera {
            eye: between(from.eye, to.eye, settling),
            at: between(from.at, to.at, settling),
            lens: LENS,
        },
    }
}

fn between(from: Vec3, to: Vec3, t: f32) -> Vec3 {
    from.plus(to.minus(from).times(t))
}

/// Where the block stands in the world as the last shot opens — what
/// [`super::settle`] takes over, and walks down to the box's own mark.
pub fn handed_over() -> Vec3 {
    at(0.0, 0.0, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intro::march::Material;

    #[test]
    fn the_cuts_are_where_the_design_says_they_are() {
        let cuts: Vec<f32> = SHOTS.iter().map(|shot| shot.at_second).collect();
        assert_eq!(cuts, vec![0.0, 1.4, 2.8]);
        assert_eq!(END, 4.0);
    }

    #[test]
    fn every_second_of_the_piece_falls_in_exactly_one_shot() {
        let mut seen = Vec::new();
        for step in 0..=400 {
            let (shot, p) = shot(step as f32 / 100.0);
            assert!((0.0..=1.0).contains(&p), "{step}: {p}");
            if seen.last() != Some(&shot.name) {
                seen.push(shot.name);
            }
        }
        assert_eq!(seen, vec!["floor", "field", "handoff"]);
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
        assert_eq!(shot(-4.0).0.name, "floor");
        assert_eq!(shot(-4.0).1, 0.0);
        assert_eq!(shot(9.0).0.name, "handoff");
        assert_eq!(shot(9.0).1, 1.0);
        assert_eq!(shot(f32::NAN).0.name, "floor", "and a clock that broke");
    }

    #[test]
    fn the_same_second_is_the_same_world_however_often_it_is_asked() {
        for step in 0..40 {
            let t = step as f32 / 10.0;
            assert_eq!(staged(t), staged(t), "{t}");
        }
    }

    #[test]
    fn the_block_is_the_one_light_and_it_is_in_every_shot() {
        let emissive = |t: f32| {
            staged(t)
                .scene
                .solids
                .iter()
                .filter(|solid| solid.material == Material::Emissive)
                .count()
        };
        for t in [0.0, 0.7, 1.4, 2.1, 2.8, 3.4, 4.0] {
            assert_eq!(emissive(t), 1, "at {t}s");
        }
    }

    #[test]
    fn the_first_shot_has_a_floor_and_the_others_do_not() {
        let ground = |t: f32| {
            staged(t)
                .scene
                .solids
                .iter()
                .any(|solid| matches!(solid.shape, Shape::Ground { .. }))
        };
        assert!(ground(0.7), "the floor shot stands on one");
        assert!(!ground(2.1), "and the field floats");
    }

    /// The camera dollies in over the first shot, orbits half a turn over the
    /// second, and comes to rest square on by the end of the third.
    #[test]
    fn the_camera_dollies_then_orbits_then_settles() {
        let eye = |t: f32| staged(t).camera.eye;
        assert!(
            eye(0.1).z < eye(0.7).z && eye(0.7).z < eye(1.3).z,
            "it dollies"
        );
        assert!(eye(1.5).z > 0.0, "the orbit starts behind the block");
        assert!(eye(2.7).z < 0.0, "and comes round in front of it");
        let last = eye(END);
        assert!(last.x.abs() < 1e-3, "square on: {last:?}");
        assert!((last.z + AWAY).abs() < 1e-3, "{last:?}");
    }

    #[test]
    fn she_stands_in_the_field_and_the_block_hangs_before_her() {
        let pictured = |t: f32| {
            staged(t)
                .scene
                .solids
                .iter()
                .any(|solid| solid.material == Material::Pictured)
        };
        assert!(!pictured(0.7), "not in the floor shot");
        assert!(pictured(2.1), "and in the field");
        // She stands beyond the block and to one side of it, so the camera
        // that has come round frontal has the block between it and her face.
        let her = her().shape;
        let Shape::Block { at: standing, .. } = her else {
            panic!("she is a billboard: {her:?}");
        };
        assert!(standing.z > 0.0 && standing.x > 0.0, "{standing:?}");
    }

    #[test]
    fn the_field_is_dark_but_for_the_block_and_the_floor_shot_is_not() {
        assert!(
            staged(0.7).scene.sky > 0.2,
            "the floor is lit enough to rule"
        );
        assert_eq!(staged(2.1).scene.sky, 0.0, "and the field is the block's");
    }

    #[test]
    fn the_last_frame_of_the_piece_has_no_world_left_in_it() {
        assert_eq!(staged(END).scene.exposure, 0.0);
        assert!(
            staged(2.85).scene.exposure > 0.8,
            "and it was there a moment before"
        );
    }

    #[test]
    fn the_dust_rises_over_the_floor_and_nowhere_else() {
        assert_eq!(staged(0.7).scene.embers.len(), embers::COUNT);
        assert!(staged(2.1).scene.embers.is_empty(), "not in the field");
        assert!(staged(3.4).scene.embers.is_empty(), "nor on the hand-off");
    }
}

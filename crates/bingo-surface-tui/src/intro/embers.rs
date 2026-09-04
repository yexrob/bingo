//! The six points that rise.
//!
//! The only particles in the opening, and they are points: a pixel each, no
//! shape, no shadow. They are not solids, so they cost the marcher nothing —
//! each is projected onto the field once and drawn where it lands, if what is
//! already there stands further off. That depth test is what makes one pass
//! *behind* the block rather than over it, which is most of what a mote of
//! dust has to say about where it is.
//!
//! Each rises on its own clock and fades in at the bottom and out at the top,
//! so none of them ever appears or vanishes on a cut.

use std::f32::consts::PI;

use super::march::{Camera, Scene};
use super::sdf::{Vec3, at};
use super::shade::{Lit, Pixels};

/// How many rise at once.
pub const COUNT: usize = 6;

/// One of them: where it is, and how far through its own climb — which is
/// the only thing its brightness is ever made of, so nothing has to be told
/// twice what the clock already said.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Ember {
    pub at: Vec3,
    pub level: f32,
}

/// A column of air the embers rise through.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rising {
    /// The bottom of the column, in the middle of it.
    pub from: Vec3,
    /// How far each one wanders either side of that.
    pub spread: f32,
    /// How far it climbs before it starts again at the bottom.
    pub span: f32,
    /// How many times a second one of them makes that climb.
    pub rate: f32,
}

/// Where the embers stand at `t` seconds.
pub fn at_time(rising: Rising, t: f32) -> Vec<Ember> {
    (0..COUNT).map(|which| one(rising, which, t)).collect()
}

/// One of them. Everything about it comes off its index, so the six are
/// spread through the column and up it without a number generator anywhere.
fn one(rising: Rising, which: usize, t: f32) -> Ember {
    let seed = which as f32;
    let climb = cycled(t * rising.rate + seed * 0.37);
    let sway = (seed * 2.4 + climb * 3.1).sin();
    Ember {
        at: at(
            rising.from.x + sway * rising.spread,
            rising.from.y + climb * rising.span,
            rising.from.z + (seed * 1.7).sin() * rising.spread,
        ),
        // Full in the middle of the climb and nothing at either end, so none
        // of them ever appears or vanishes where a person can see it happen.
        level: (climb * PI).sin().max(0.0),
    }
}

/// The fractional part, which is one ember's place in its own climb.
fn cycled(turns: f32) -> f32 {
    match turns.is_finite() {
        true => turns - turns.floor(),
        false => 0.0,
    }
}

/// How far off one has to be before it is no longer worth a cell.
const REACH: f32 = 9.0;
/// Below this it is not drawn at all, rather than flickering at the edge of
/// what a pixel can say for a moment at each end of its climb.
const SEEN: f32 = 0.10;

/// The embers over the world, where the world is not already in front of them.
pub fn draw(field: &mut Pixels, camera: &Camera, scene: &Scene) {
    let (width, height) = (field.width(), field.height());
    for ember in &scene.embers {
        let Some((u, v, depth)) = camera.project(ember.at) else {
            continue;
        };
        let Some((x, y)) = super::shade::pixel_at(u, v, width, height) else {
            continue;
        };
        if !field.in_front(x, y, depth) {
            continue;
        }
        let level = ember.level * (1.0 - depth / REACH).clamp(0.0, 1.0) * scene.exposure;
        if level > SEEN {
            field.set(x, y, Lit { level, warm: 1.0 });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const COLUMN: Rising = Rising {
        from: at(0.0, -1.0, 0.0),
        spread: 0.5,
        span: 2.0,
        rate: 0.3,
    };

    #[test]
    fn six_rise_at_once_and_none_of_them_stands_where_another_does() {
        let embers = at_time(COLUMN, 0.4);
        assert_eq!(embers.len(), COUNT);
        for (index, ember) in embers.iter().enumerate() {
            for other in &embers[index + 1..] {
                assert!(
                    ember.at.minus(other.at).length() > 0.05,
                    "{ember:?} {other:?}"
                );
            }
        }
    }

    #[test]
    fn one_climbs_its_column_and_starts_again_at_the_bottom() {
        let height = |t| one(COLUMN, 0, t).at.y;
        assert!(height(0.0) < height(1.0), "it rises");
        let full = 1.0 / COLUMN.rate;
        assert!((height(0.0) - height(full)).abs() < 1e-3, "and comes round");
    }

    #[test]
    fn it_is_dark_at_both_ends_of_its_climb_and_bright_in_the_middle() {
        let ends = one(COLUMN, 0, 0.0).level;
        let middle = one(COLUMN, 0, 0.5 / COLUMN.rate).level;
        assert!(ends < 0.02, "{ends}");
        assert!(middle > 0.98, "{middle}");
    }

    #[test]
    fn a_time_that_is_not_a_number_leaves_them_where_they_started() {
        let ember = one(COLUMN, 3, f32::NAN).at;
        assert!(ember.x.is_finite() && ember.y.is_finite() && ember.z.is_finite());
    }
}

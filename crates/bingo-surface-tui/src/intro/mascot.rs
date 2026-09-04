//! Her, out of the picture's own pixels.
//!
//! A face in characters cannot be modelled — a mesh at twenty-four cells wide
//! is a smear — so she stands in the world as a picture on a billboard, and
//! what the ray finds there is read off the pixels. The picture is decoded
//! once, through [`bingo_pictures`] like every other picture this surface
//! shows, and box-filtered down to a field of light small enough that a cell
//! is one lookup and not a thousand.
//!
//! Two numbers come out of each pixel and no more: how bright it is, and how
//! warm — which is exactly what [`super::shade::Lit`] spends. The picture is
//! a rim-lit profile on black, so the bright part *is* the drawing, and the
//! warm part is the block's light on her. The theme's own two inks draw her
//! from those, and a terminal with no colour still has the rim.

use std::sync::OnceLock;

use super::shade::Lit;

/// The picture, as the site draws it: her in profile, the block before her.
const MASCOT: &[u8] = include_bytes!("../../assets/mascot.png");

/// The part of the picture she is in: her head from the ears to the chin,
/// with the hood behind it. The picture's own block and the embers before it
/// are left out on purpose — the opening has a block of its own, in the
/// world, and one fact is drawn once.
const CROP: (f32, f32, f32, f32) = (0.30, 0.02, 0.762, 0.62);

/// How tall the crop is for its width. The billboard in the world is built
/// to this, so she is never stretched.
pub const SHAPE: f32 = (CROP.3 - CROP.1) / (CROP.2 - CROP.0);

/// How finely the crop is filtered down: about the size the billboard is
/// drawn at, in pixels, and not much more.
///
/// One reduction and not two. She is a *line* drawing — the profile is a rim a
/// pixel or two wide in the source — and every resampling between the file and
/// the screen is a chance to average that rim away. Reducing once, straight to
/// the size she is seen at, and keeping some of the brightest sample in each
/// ([`PEAK`]) is what leaves her a face rather than a smudge.
const ACROSS: usize = 40;
const DOWN: usize = 52;

/// How much of a sample is the brightest pixel under it rather than the mean
/// of them all. A plain average buries a one-pixel rim under the dark beside
/// it; the rim is the whole of what makes a face a face.
const PEAK: f32 = 0.55;

/// The window of the picture's own light the ramp is spent on.
///
/// Measured, not guessed (`intro::mascot::probe::histogram`): seven tenths of
/// the crop sits within a hundredth of 0.05 — the hood and the dark she stands
/// in — and everything that is *drawing* is in the last seventh, up to 0.81.
/// Spending the ramp on 0 to 1 would put nine cells in ten on the same step
/// and leave her a rim floating in a wash. The floor is just under that dark
/// band so it comes out as air; the ceiling is where the lit profile begins,
/// so the rim reaches the top of the ramp; and the curve lifts what is between
/// them, which is the hood — the faint mass that makes the rim a head.
const FLOOR: f32 = 0.048;
const CEIL: f32 = 0.45;
const CURVE: f32 = 0.8;

/// Her light, sampled: [`ACROSS`] by [`DOWN`] of it.
struct Field {
    lit: Vec<Lit>,
}

impl Field {
    /// The sample at a place in the field, the two it falls between mixed —
    /// so the billboard has no stair-steps in it when the camera drifts.
    fn at(&self, across: f32, down: f32) -> Lit {
        let (x, u) = (across.floor(), across.fract());
        let (y, v) = (down.floor(), down.fract());
        let (x, y) = (x as isize, y as isize);
        let row = |y: isize| {
            let left = self.one(x, y);
            let right = self.one(x + 1, y);
            (left.level + (right.level - left.level) * u, left.warm)
        };
        let (top, warm) = row(y);
        let (bottom, _) = row(y + 1);
        Lit {
            level: top + (bottom - top) * v,
            warm,
        }
    }

    fn one(&self, x: isize, y: isize) -> Lit {
        let inside = (0..ACROSS as isize).contains(&x) && (0..DOWN as isize).contains(&y);
        match inside {
            true => self
                .lit
                .get(y as usize * ACROSS + x as usize)
                .copied()
                .unwrap_or_default(),
            false => Lit::default(),
        }
    }
}

/// The field, filtered once and kept. A picture that will not decode leaves
/// an empty field rather than taking the surface down: the opening is a
/// flourish, and a flourish that panics is worse than one that is dark.
fn field() -> &'static Field {
    static FIELD: OnceLock<Field> = OnceLock::new();
    FIELD.get_or_init(|| Field {
        lit: bingo_pictures::pixels(MASCOT).map_or_else(|_| Vec::new(), filtered),
    })
}

/// The crop, boxed down to the field's size: every pixel of a sample's own
/// rectangle averaged, which is what keeps her free of the shimmer a single
/// sample per cell would give a drifting camera.
fn filtered(picture: bingo_pictures::Pixels) -> Vec<Lit> {
    let across = picture.width as f32;
    let down = picture.height as f32;
    let left = CROP.0 * across;
    let top = CROP.1 * down;
    let step = ((CROP.2 - CROP.0) * across / ACROSS as f32).max(1.0);
    let drop = ((CROP.3 - CROP.1) * down / DOWN as f32).max(1.0);
    let mut lit = Vec::with_capacity(ACROSS * DOWN);
    for y in 0..DOWN {
        for x in 0..ACROSS {
            let from = (left + x as f32 * step, top + y as f32 * drop);
            lit.push(boxed(&picture, from, (step, drop)));
        }
    }
    lit
}

/// One sample: the pixels under it, mostly as their brightest and partly as
/// their mean ([`PEAK`]).
fn boxed(picture: &bingo_pictures::Pixels, from: (f32, f32), size: (f32, f32)) -> Lit {
    let mut light = 0.0f32;
    let mut warm = 0.0f32;
    let mut peak = 0.0f32;
    let mut taken = 0.0f32;
    for down in 0..size.1.ceil() as u32 {
        for across in 0..size.0.ceil() as u32 {
            let pixel = picture.at(from.0 as u32 + across, from.1 as u32 + down);
            let (level, hue) = of(pixel);
            light += level;
            warm += hue * level;
            peak = peak.max(level);
            taken += 1.0;
        }
    }
    if taken <= 0.0 {
        return Lit::default();
    }
    let mean = light / taken;
    graded(
        mean * (1.0 - PEAK) + peak * PEAK,
        warm / light.max(f32::EPSILON),
    )
}

/// One pixel, as brightness and how far towards the warm end of the spectrum
/// it sits. Transparent pixels are nothing at all, which is what makes the
/// picture's rounded corners disappear rather than square her off.
fn of(pixel: [u8; 4]) -> (f32, f32) {
    let [r, g, b, a] = pixel.map(|part| f32::from(part) / 255.0);
    let level = (0.2126 * r + 0.7152 * g + 0.0722 * b) * a;
    let warm = match r + b > 1e-3 {
        true => ((r - b) / (r + b)).clamp(0.0, 1.0),
        false => 0.0,
    };
    (level, warm)
}

/// A sample pushed onto the ramp, through the window the picture actually
/// uses.
fn graded(level: f32, warm: f32) -> Lit {
    Lit {
        level: ((level - FLOOR) / (CEIL - FLOOR))
            .clamp(0.0, 1.0)
            .powf(CURVE),
        warm: warm.clamp(0.0, 1.0),
    }
}

/// Her light at a point of the billboard: `across` and `down` each from -1 at
/// one edge to 1 at the other. Outside it there is nothing — which is how the
/// billboard's own corners come out as air rather than as a black rectangle.
pub fn light(across: f32, down: f32) -> Lit {
    let inside = (-1.0..=1.0).contains(&across) && (-1.0..=1.0).contains(&down);
    match inside {
        true => field().at(
            (across + 1.0) * 0.5 * (ACROSS - 1) as f32,
            (down + 1.0) * 0.5 * (DOWN - 1) as f32,
        ),
        false => Lit::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picture_decodes_and_the_field_is_the_size_it_says() {
        assert_eq!(field().lit.len(), ACROSS * DOWN, "the picture decoded");
    }

    #[test]
    fn outside_the_billboard_there_is_nothing_at_all() {
        assert_eq!(light(-1.4, 0.0), Lit::default());
        assert_eq!(light(0.0, 9.0), Lit::default());
        assert_eq!(light(f32::NAN, 0.0), Lit::default());
    }

    #[test]
    fn every_sample_of_her_is_a_number_between_nothing_and_everything() {
        for step in 0..40i32 {
            let place = -1.0 + step as f32 / 20.0;
            let lit = light(place, place * 0.7);
            assert!((0.0..=1.0).contains(&lit.level), "{place}: {lit:?}");
            assert!((0.0..=1.0).contains(&lit.warm), "{place}: {lit:?}");
        }
    }

    #[test]
    fn she_is_a_lit_rim_on_a_dark_ground_and_the_ramp_is_spent_that_way() {
        let brightest = field()
            .lit
            .iter()
            .map(|lit| lit.level)
            .fold(0.0f32, f32::max);
        assert!(
            brightest > 0.9,
            "the lit profile reaches the top: {brightest}"
        );

        let mut levels: Vec<f32> = field().lit.iter().map(|lit| lit.level).collect();
        levels.sort_by(|a, b| a.partial_cmp(b).expect("every sample is a number"));
        let median = levels[levels.len() / 2];
        assert!(
            median < 0.05,
            "and most of her is the dark she stands in: {median}"
        );

        assert!(light(0.9, -0.9).level < 0.1, "the far corner of the hood");
        let warm = field()
            .lit
            .iter()
            .filter(|lit| lit.level > 0.5)
            .map(|lit| lit.warm)
            .fold(0.0f32, f32::max);
        assert!(warm > 0.3, "and what light there is on her is warm: {warm}");
    }

    /// The reduction keeps the rim, and keeps it *continuous*: her lit profile
    /// is a line a pixel or two wide in the source, and a plain mean of each
    /// sample's own pixels would leave it a dotted one. A dotted rim is what a
    /// smudge is.
    #[test]
    fn the_lit_profile_survives_the_reduction_as_a_line() {
        let brightest = |from: f32| {
            (0..DOWN)
                .map(|y| light(from, y as f32 / DOWN as f32 * 2.0 - 1.0).level)
                .fold(0.0f32, f32::max)
        };
        let across: Vec<f32> = (0..20).map(|step| -0.6 + step as f32 * 0.06).collect();
        let lit = across.iter().filter(|x| brightest(**x) > 0.5).count();
        assert!(
            lit >= 14,
            "the profile runs down her face: {lit} of {} columns, {:?}",
            across.len(),
            across.iter().map(|x| brightest(*x)).collect::<Vec<_>>()
        );
    }
}

#[cfg(test)]
mod probe {
    use super::*;

    #[test]
    #[ignore = "prints the crop's own histogram, for grading it"]
    fn histogram() {
        let picture = bingo_pictures::pixels(MASCOT).expect("the picture");
        let mut raw: Vec<f32> = Vec::new();
        let across = picture.width as f32;
        let down = picture.height as f32;
        for y in 0..DOWN {
            for x in 0..ACROSS {
                let px = (CROP.0 * across + x as f32 * (CROP.2 - CROP.0) * across / ACROSS as f32)
                    as u32;
                let py = (CROP.1 * down + y as f32 * (CROP.3 - CROP.1) * down / DOWN as f32) as u32;
                raw.push(of(picture.at(px, py)).0);
            }
        }
        raw.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
        for share in [0.0, 0.25, 0.5, 0.7, 0.85, 0.93, 0.97, 0.99, 1.0] {
            let at = ((raw.len() - 1) as f32 * share) as usize;
            println!("p{:>3.0}: {:.4}", share * 100.0, raw[at]);
        }
    }
}

//! What light does to a cell.
//!
//! A ray comes back from [`super::march`] with a point and a surface; this
//! says how much light stands on it and where that light came from, and then
//! spends the answer on the two things a terminal has: a glyph from a
//! luminance ramp, and one of the theme's own tokens.
//!
//! Two lights and no more, which is what the design's one warm colour allows:
//! a directional one that models the world, and the block's own — the point
//! light that is also the thing the whole opening is about. What the block
//! touches comes out warm, what only the sun touches comes out in the neutral
//! inks, and the ramp carries the brightness in both — so `NO_COLOR` loses
//! the *source* of the light and never the shape of the world.

use super::grid::{Cell, Grid};
use super::march::{Camera, Hit, Marcher, Material, Ray, Scene};
use super::sdf::{Shape, Vec3};
use crate::theme;

/// One cell's light: how much of it there is, and how much of it came from
/// the block. Both are 0 to 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Lit {
    pub level: f32,
    pub warm: f32,
}

/// A terminal cell is twice as tall as it is wide, so a world drawn one
/// sample to a cell would come out squashed. The view is stretched across by
/// this much to put it back.
const CELL: f32 = 2.0;

/// How much light stands everywhere, which is what keeps an unlit face from
/// being a hole rather than a surface.
const AMBIENT: f32 = 0.05;
/// The directional light's share. Most of the ramp is spent here: a ten-step
/// ramp with half its range on ambient has no contrast left for shape.
const SUN: f32 = 0.9;
/// How hard a surface turned away from the eye catches the light. This is the
/// term that draws an edge, and an edge is what a person reads depth from.
const RIM: f32 = 0.5;
const RIM_FALLOFF: i32 = 3;
/// How far a surface is lifted off itself before a shadow is walked from it,
/// so a surface does not shadow the point the ray landed on.
const LIFT: f32 = 0.03;
/// How wide the block's own halo hangs in the air around it, across and up.
/// Wider up than across, because the block is a caret and a round glow around
/// an upright bar makes the bar read as a ball.
const HALO: f32 = 0.30;
const HALO_TALL: f32 = 2.8;
/// How bright that halo is where the ray passes straight through the block.
const HALO_LIGHT: f32 = 0.85;
/// How much of an emissive surface's light is flat, and how much follows the
/// face it is on. All of it flat and a turning block would show nothing turn:
/// an emissive solid has no shading to read its rotation from, so the little
/// it has must come from which way the face is pointed.
const EMISSIVE: (f32, f32) = (0.76, 0.24);

/// The ramp a cell's brightness is drawn on. Ten steps of ink, from nothing
/// to solid — the one the ray-marched-ASCII genre is written in, and the one
/// that draws an *edge* rather than a wash, because its glyphs grow by how
/// much of the cell they fill.
pub const RAMP: &[char] = &[' ', '.', ':', '-', '=', '+', '*', '#', '%', '@'];

/// The same, where the terminal draws more than ASCII: the top of the ramp
/// becomes the shade blocks, which fill a cell evenly instead of leaving the
/// holes a `#` and a `%` do. The bottom stays punctuation, because a shade
/// block is already too solid to say *faint*.
pub const SHADED: &[char] = &[' ', '.', ':', '-', '=', '+', '░', '▒', '▓', '█'];

/// Which of the two this terminal is drawing in.
pub fn ramp() -> &'static [char] {
    match theme::glyphs() == &theme::ASCII {
        true => RAMP,
        false => SHADED,
    }
}

/// The glyph a brightness is drawn with.
pub fn glyph(level: f32) -> char {
    let ramp = ramp();
    let last = ramp.len().saturating_sub(1);
    let step = (level.clamp(0.0, 1.0) * last as f32).round() as usize;
    ramp.get(step).copied().unwrap_or(' ')
}

/// One lit cell, ready for the canvas.
pub fn cell(lit: Lit) -> Cell {
    Cell {
        glyph: glyph(lit.level),
        style: theme::lit(lit.level, lit.warm),
    }
}

/// One frame of the world: the cells it was drawn into, how far away what
/// each cell shows stands, and what the marching cost.
///
/// The depths are here because the embers are drawn after the world and have
/// to know what is already in front of them; nothing else asks.
pub struct Rendered {
    pub grid: Grid,
    depth: Vec<f32>,
    pub steps: u64,
}

impl Rendered {
    /// Whether something `depth` away would be seen at this cell, or is
    /// standing behind what already is.
    pub fn in_front(&self, x: u16, y: u16, depth: f32) -> bool {
        let index = usize::from(y) * usize::from(self.grid.width()) + usize::from(x);
        self.depth
            .get(index)
            .is_none_or(|standing| depth < *standing)
    }
}

/// The world, inked. Every step the marching spent comes back with it, which
/// is what the frame budget is held to.
pub fn render(scene: &Scene, camera: &Camera, width: u16, height: u16) -> Rendered {
    let mut marcher = Marcher::new(scene);
    let mut grid = Grid::new(width, height);
    let mut depth = Vec::with_capacity(usize::from(width) * usize::from(height));
    for y in 0..height {
        for x in 0..width {
            let (u, v) = view(x, y, width, height);
            let ray = camera.ray(u, v);
            let (lit, standing) = along(&mut marcher, scene, ray);
            grid.set(x, y, cell(lit));
            depth.push(standing);
        }
    }
    Rendered {
        grid,
        depth,
        steps: marcher.steps(),
    }
}

/// Where the centre of a cell falls in the view: -1 to 1 up the screen, and
/// the same across widened by [`CELL`] so nothing round comes out oval.
fn view(x: u16, y: u16, width: u16, height: u16) -> (f32, f32) {
    let across = f32::from(width).max(1.0);
    let down = f32::from(height).max(1.0);
    let aspect = across / (down * CELL);
    let u = ((f32::from(x) + 0.5) / across * 2.0 - 1.0) * aspect;
    let v = 1.0 - (f32::from(y) + 0.5) / down * 2.0;
    (u, v)
}

/// The cell a point of the view falls in — [`view`] read the other way, for
/// the things that are projected onto the screen rather than marched into it.
/// `None` for a point outside the screen.
pub fn cell_at(u: f32, v: f32, width: u16, height: u16) -> Option<(u16, u16)> {
    let across = f32::from(width).max(1.0);
    let down = f32::from(height).max(1.0);
    let aspect = across / (down * CELL);
    let x = (u / aspect + 1.0) * 0.5 * across - 0.5;
    let y = (1.0 - v) * 0.5 * down - 0.5;
    let (x, y) = (x.round(), y.round());
    let on_screen = (0.0..across).contains(&x) && (0.0..down).contains(&y);
    on_screen.then_some((x as u16, y as u16))
}

/// What one ray comes back with: the surface it found, dimmed by the distance
/// it crossed, the block's halo in whatever air it passed through, and how far
/// away the whole of that stands.
fn along(marcher: &mut Marcher, scene: &Scene, ray: Ray) -> (Lit, f32) {
    let horizon = marcher.horizon();
    let hit = marcher.cast(ray);
    let reach = hit.map_or(horizon, |hit| hit.travelled);
    let surface = hit.map_or(Lit::default(), |hit| {
        fogged(surface(marcher, scene, ray, hit), scene.fog, hit.travelled)
    });
    let halo = halo(scene, ray, reach);
    let lit = Lit {
        level: ((surface.level + halo) * scene.exposure).clamp(0.0, 1.0),
        warm: share(surface.level * surface.warm + halo, surface.level + halo),
    };
    (lit, reach)
}

/// The light standing on one surface — or, where the surface wears one, what
/// the picture says stands there.
fn surface(marcher: &mut Marcher, scene: &Scene, ray: Ray, hit: Hit) -> Lit {
    let Some(solid) = scene.solids.get(hit.solid) else {
        return Lit::default();
    };
    if solid.material == Material::Pictured {
        return pictured(solid.shape, hit.at);
    }
    let normal = marcher.normal(hit.at);
    if solid.material == Material::Emissive {
        let facing = normal.dot(ray.towards.times(-1.0)).clamp(0.0, 1.0);
        return Lit {
            level: EMISSIVE.0 + EMISSIVE.1 * facing,
            warm: 1.0,
        };
    }
    let from = hit.at.plus(normal.times(LIFT));
    let key = normal.dot(scene.sun).max(0.0) * marcher.shadow(from, scene.sun, super::march::FAR);
    let (lamp, nearness) = lamp(marcher, scene, from, normal);
    let rim = (1.0 - normal.dot(ray.towards.times(-1.0)).abs())
        .clamp(0.0, 1.0)
        .powi(RIM_FALLOFF)
        * RIM;
    let warm = lamp + rim * nearness;
    let cool = (AMBIENT + key * SUN) * scene.sky + rim * (1.0 - nearness);
    Lit {
        level: (warm + cool).clamp(0.0, 1.0),
        warm: share(warm, warm + cool),
    }
}

/// A picture, where the ray landed on it: the point on the billboard turned
/// back into a place in the picture, and the picture asked what is there.
fn pictured(shape: Shape, point: Vec3) -> Lit {
    let Shape::Block { at, half, .. } = shape else {
        return Lit::default();
    };
    let offset = point.minus(at);
    super::mascot::light(
        share_of(offset.x, half.x),
        // A picture's rows run down and the world's `y` runs up.
        -share_of(offset.y, half.y),
    )
}

/// Where a point falls across a half-extent, from -1 at one edge to 1 at the
/// other.
fn share_of(offset: f32, half: f32) -> f32 {
    match half.abs() > f32::EPSILON {
        true => offset / half,
        false => 0.0,
    }
}

/// The directional light on one surface. A face turned away from it, or a
/// world with no sky to speak of, is not worth walking a shadow for — and the
/// walk is a dozen steps of the frame's budget every time it is taken.
fn sun(marcher: &mut Marcher, scene: &Scene, from: Vec3, normal: Vec3) -> f32 {
    let facing = normal.dot(scene.sun);
    match facing * scene.sky > FAINT {
        true => facing * marcher.shadow(from, scene.sun, marcher.horizon()),
        false => 0.0,
    }
}

/// The block's light on one surface, and how near that surface is to it —
/// which is also how warm its rim should read.
fn lamp(marcher: &mut Marcher, scene: &Scene, from: Vec3, normal: Vec3) -> (f32, f32) {
    let offset = scene.lamp.at.minus(from);
    let distance = offset.length();
    let reach = scene.lamp.reach.max(f32::EPSILON);
    let nearness = 1.0 / (1.0 + (distance / reach) * (distance / reach));
    let facing = normal.dot(offset.unit()).max(0.0);
    let standing = facing * nearness * scene.lamp.strength;
    match standing > FAINT {
        true => (
            standing * marcher.shadow(from, offset.unit(), distance),
            nearness,
        ),
        false => (0.0, nearness),
    }
}

/// Below this a light would not move the cell off the step of the ramp it is
/// already on, so what stands in its way does not matter.
const FAINT: f32 = 0.01;

/// Distance eating light. Nothing far is bright, which is the whole of how a
/// corridor reads as long.
fn fogged(lit: Lit, fog: f32, travelled: f32) -> Lit {
    Lit {
        level: lit.level * (-fog * travelled).exp(),
        warm: lit.warm,
    }
}

/// The block seen through the air in front of it: how close the ray passed to
/// the lamp, as light. It is what makes a light look like a light rather than
/// a bright shape — and it is the only thing in the opening that is not a
/// surface.
fn halo(scene: &Scene, ray: Ray, upto: f32) -> f32 {
    let offset = scene.lamp.at.minus(ray.from);
    let along = offset.dot(ray.towards).clamp(0.0, upto);
    if along <= 0.0 {
        return 0.0;
    }
    let miss = offset.minus(ray.towards.times(along));
    let closest = super::sdf::at(miss.x, miss.y / HALO_TALL, miss.z).length() / HALO;
    (-closest * closest).exp() * HALO_LIGHT * scene.lamp.strength
}

/// One part of a whole, and nothing at all where there is no whole to be part
/// of — the guard that keeps a black cell's warmth out of the arithmetic.
fn share(part: f32, whole: f32) -> f32 {
    match whole > 1e-4 {
        true => (part / whole).clamp(0.0, 1.0),
        false => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intro::march::{Camera, Lamp, Solid};
    use crate::intro::sdf::{Shape, at};
    use crate::painted::{ascii, in_look, no_colour, truecolor};

    const BOARD: (u16, u16) = (60, 20);

    fn lone(shape: Shape) -> Scene {
        Scene {
            solids: vec![Solid::of(shape)],
            sun: at(-0.4, 0.8, -0.45).unit(),
            ..Scene::default()
        }
    }

    fn frame(scene: &Scene) -> Rendered {
        render(scene, &Camera::default(), BOARD.0, BOARD.1)
    }

    fn drawn(scene: &Scene) -> String {
        frame(scene)
            .grid
            .lines()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A cell's brightness read back off the canvas, as a place on the ramp.
    fn lit_at(rendered: &Rendered, x: u16, y: u16) -> usize {
        let glyph = rendered.grid.cell(x, y).glyph;
        ramp().iter().position(|step| *step == glyph).unwrap_or(0)
    }

    #[test]
    fn the_ramp_runs_from_nothing_to_solid() {
        in_look(ascii(), || {
            assert_eq!(glyph(0.0), ' ');
            assert_eq!(glyph(1.0), '@');
            assert_eq!(glyph(-9.0), ' ', "and clamps rather than panicking");
            assert_eq!(glyph(f32::NAN), ' ', "a level that is not a number is dark");
            String::new()
        });
    }

    #[test]
    fn a_terminal_that_draws_only_ascii_gets_the_ascii_ramp() {
        in_look(ascii(), || {
            assert_eq!(ramp(), RAMP);
            String::new()
        });
        in_look(truecolor(), || {
            assert_eq!(ramp(), SHADED);
            String::new()
        });
    }

    /// The three shapes of the plan's first exit criterion, side by side.
    #[test]
    fn a_sphere_a_box_and_a_torus_read_as_themselves() {
        let board = in_look(ascii(), || {
            [
                Shape::Sphere {
                    at: at(0.0, 0.0, 0.0),
                    radius: 1.6,
                },
                Shape::Block {
                    at: at(0.0, 0.0, 0.0),
                    half: at(1.2, 1.2, 1.2),
                    round: 0.06,
                    spin: 0.6,
                },
                Shape::Torus {
                    at: at(0.0, 0.0, 0.0),
                    major: 1.4,
                    minor: 0.45,
                },
            ]
            .map(|shape| drawn(&lone(shape)))
            .join("\n\n")
        });
        insta::assert_snapshot!("primitives", board);
    }

    #[test]
    fn a_lit_face_is_brighter_than_one_turned_away() {
        let scene = lone(Shape::Sphere {
            at: at(0.0, 0.0, 0.0),
            radius: 1.6,
        });
        let rendered = frame(&scene);
        // The sun stands up and to the left, so the top-left of the ball is
        // its key side and the bottom-right is its shadow side.
        let key = lit_at(&rendered, 22, 6);
        let shadow = lit_at(&rendered, 38, 14);
        assert!(key > shadow, "key {key} should beat shadow {shadow}");
    }

    #[test]
    fn distance_eats_light() {
        let corridor = |fog| Scene {
            fog,
            ..lone(Shape::Ground { y: -1.0 })
        };
        let clear = lit_at(&frame(&corridor(0.0)), 30, 19);
        let foggy = lit_at(&frame(&corridor(0.25)), 30, 19);
        assert!(clear > foggy, "clear {clear} should beat foggy {foggy}");
    }

    #[test]
    fn the_block_lights_what_it_stands_beside_and_the_air_around_it() {
        let scene = Scene {
            solids: vec![
                Solid::of(Shape::Ground { y: -1.2 }),
                Solid::of(Shape::Block {
                    at: at(0.0, 0.0, 0.0),
                    half: at(0.12, 0.42, 0.12),
                    round: 0.02,
                    spin: 0.0,
                })
                .lit(),
            ],
            sun: at(-0.4, 0.8, -0.45).unit(),
            lamp: Lamp {
                at: at(0.0, 0.0, 0.0),
                reach: 2.0,
                strength: 1.0,
            },
            fog: 0.05,
            ..Scene::default()
        };
        let rendered = frame(&scene);
        assert!(
            lit_at(&rendered, 30, 9) >= ramp().len() - 2,
            "the block itself is at the top of the ramp: {}",
            lit_at(&rendered, 30, 9)
        );
        assert!(
            lit_at(&rendered, 28, 9) > 0,
            "the air beside it carries a halo"
        );
    }

    #[test]
    fn the_halo_hangs_about_the_lamp_and_nowhere_else() {
        let scene = Scene {
            lamp: Lamp {
                at: at(0.0, 0.0, 0.0),
                reach: 2.0,
                strength: 1.0,
            },
            ..Scene::default()
        };
        let through = |from: Vec3| {
            halo(
                &scene,
                Ray {
                    from,
                    towards: at(0.0, 0.0, 1.0),
                },
                super::super::march::FAR,
            )
        };
        assert!(through(at(0.0, 0.0, -4.0)) > 0.5, "straight through it");
        assert!(through(at(0.4, 0.0, -4.0)) < 0.4, "and gone a step aside");
        assert_eq!(through(at(0.0, 0.0, 4.0)), 0.0, "and nothing behind it");
        assert!(
            through(at(0.0, 0.6, -4.0)) > through(at(0.6, 0.0, -4.0)),
            "it hangs taller than it is wide, as the block does"
        );
    }

    #[test]
    fn a_terminal_with_no_colour_still_has_the_whole_shape() {
        let scene = lone(Shape::Sphere {
            at: at(0.0, 0.0, 0.0),
            radius: 1.6,
        });
        let plain = in_look(no_colour(), || drawn(&scene));
        let painted = in_look(truecolor(), || drawn(&scene));
        assert_eq!(plain, painted, "the ramp carries the world, not the colour");
    }

    #[test]
    fn a_cell_off_the_screen_has_nothing_projected_into_it() {
        assert_eq!(cell_at(9.0, 0.0, 60, 20), None);
        assert_eq!(cell_at(0.0, -9.0, 60, 20), None);
        assert_eq!(cell_at(f32::NAN, 0.0, 60, 20), None);
    }

    #[test]
    fn the_middle_of_the_view_is_the_middle_of_the_screen() {
        let (x, y) = cell_at(0.0, 0.0, 60, 20).expect("the middle is on screen");
        assert_eq!((x, y), (30, 10));
        let (u, v) = view(30, 10, 60, 20);
        assert!(u.abs() < 0.05 && v.abs() < 0.1, "{u} {v}");
    }
}

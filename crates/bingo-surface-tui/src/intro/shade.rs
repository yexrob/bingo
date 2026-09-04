//! What light does to a pixel, and how two pixels become one cell.
//!
//! A ray comes back from [`super::march`] with a point and a surface; this
//! says how much light stands on it and where that light came from, and then
//! spends the answer on the finest thing a terminal has without a graphics
//! protocol: a **half block**. A cell is `▀` with its foreground the pixel in
//! the top half and its background the one in the bottom, so a box of `w × h`
//! cells is a picture of `w × 2h` truecolor samples — and a pixel is *square*,
//! where a cell is twice as tall as it is wide.
//!
//! Two lights and no more, which is what the design's one warm colour allows:
//! a directional one that models the world, and the block's own — the point
//! light that is also the thing the whole opening is about. What the block
//! touches comes out warm, what only the sun touches comes out in the neutral
//! inks, and a pixel with no light in it at all is left to the terminal's own
//! ground, which is what the design paints everywhere else.

use super::grid::{Cell, Grid};
use super::march::{Camera, Casting, Hit, Marcher, Material, Ray, Scene};
use super::sdf::{Shape, Vec3};
use crate::theme;

/// One pixel's light: how much of it there is, and how much of it came from
/// the block. Both are 0 to 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Lit {
    pub level: f32,
    pub warm: f32,
}

/// Below this a pixel's ink is the ground it stands on, so the cell is left
/// unpainted and the terminal's own background shows through.
const DARK: f32 = 0.02;

/// The top half of a cell, and the bottom.
const UPPER: char = '▀';
const LOWER: char = '▄';

/// How much light stands everywhere, which is what keeps an unlit face from
/// being a hole rather than a surface.
const AMBIENT: f32 = 0.05;
/// The directional light's share.
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
const HALO: f32 = 0.10;
const HALO_TALL: f32 = 2.8;
/// How bright that halo is where the ray passes straight through the block.
const HALO_LIGHT: f32 = 0.85;
/// How much of an emissive surface's light is flat, and how much follows the
/// face it is on. All of it flat and a turning block would show nothing turn:
/// an emissive solid has no shading to read its rotation from, so the little
/// it has must come from which way the face is pointed.
const EMISSIVE: (f32, f32) = (0.76, 0.24);

/// How far apart the floor's rules stand.
const RULE: f32 = 2.5;
/// How wide one is drawn, as a share of how far away it stands. A rule of a
/// fixed width in the world projects thinner the further off it is, and a line
/// thinner than a pixel is a stipple; widening it with the distance keeps it
/// the same width on the screen wherever it lies, which is what a *line* is.
const RULE_WIDTH: f32 = 0.07;
/// How much of a ruled floor's light stands between its rules. Almost none:
/// the floor is the grid, and the dark between its lines is the terminal's own
/// ground — which is what makes the lines read as lines and the vanishing
/// point read as distance.
const BETWEEN: f32 = 0.10;

/// One pixel, ready for the canvas — or nothing at all where there is no
/// light in it.
fn ink(lit: Lit) -> Option<Lit> {
    (lit.level > DARK).then_some(lit)
}

/// One cell: the two pixels stacked in it. Which glyph it wears is which of
/// them carries light — the dark half is always the terminal's own ground, so
/// a black world is black and not a grey wash.
pub fn stacked(upper: Lit, lower: Lit) -> Cell {
    match (ink(upper), ink(lower)) {
        (None, None) => Cell::default(),
        (Some(up), None) => Cell {
            glyph: UPPER,
            style: theme::lit(up.level, up.warm),
        },
        (None, Some(down)) => Cell {
            glyph: LOWER,
            style: theme::lit(down.level, down.warm),
        },
        (Some(up), Some(down)) => Cell {
            glyph: UPPER,
            style: theme::half((up.level, up.warm), (down.level, down.warm)),
        },
    }
}

/// A frame of the world as light: one sample a pixel, twice as many rows as
/// the box has, and how far away what each pixel shows stands.
///
/// The depths are here because the embers are drawn after the world and have
/// to know what is already in front of them; nothing else asks.
pub struct Pixels {
    width: u16,
    height: u16,
    lit: Vec<Lit>,
    depth: Vec<f32>,
}

impl Pixels {
    /// An unlit field, which is the black every shot opens on.
    pub fn new(width: u16, height: u16) -> Self {
        let area = usize::from(width) * usize::from(height);
        Pixels {
            width,
            height,
            lit: vec![Lit::default(); area],
            depth: vec![f32::INFINITY; area],
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn lit(&self, x: u16, y: u16) -> Lit {
        self.at(x, y)
            .and_then(|index| self.lit.get(index).copied())
            .unwrap_or_default()
    }

    pub fn set(&mut self, x: u16, y: u16, lit: Lit) {
        if let Some(index) = self.at(x, y)
            && let Some(slot) = self.lit.get_mut(index)
        {
            *slot = lit;
        }
    }

    /// Whether something `depth` away would be seen at this pixel, or is
    /// standing behind what already is.
    pub fn in_front(&self, x: u16, y: u16, depth: f32) -> bool {
        self.at(x, y)
            .and_then(|index| self.depth.get(index))
            .is_none_or(|standing| depth < *standing)
    }

    fn at(&self, x: u16, y: u16) -> Option<usize> {
        (x < self.width && y < self.height)
            .then(|| usize::from(y) * usize::from(self.width) + usize::from(x))
    }
}

/// The world, marched: the field of light, and every step it cost to find.
///
/// The cost comes back beside the field rather than inside it because it is a
/// fact about the walk and not about the picture — and because a step is the
/// same number on every machine, which is what the frame budget is held to.
pub fn pixels(scene: &Scene, camera: &Camera, width: u16, height: u16) -> (Pixels, u64) {
    let mut marcher = Marcher::new(scene);
    let mut field = Pixels::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let (u, v) = view(x, y, width, height);
            let (lit, standing) = along(&mut marcher, scene, camera.ray(u, v));
            field.set(x, y, lit);
            if let Some(index) = field.at(x, y)
                && let Some(slot) = field.depth.get_mut(index)
            {
                *slot = standing;
            }
        }
    }
    (field, marcher.steps())
}

/// The field packed into cells: two pixel rows to a row of half blocks. An
/// odd last row has the terminal's own ground under it.
pub fn halves(field: &Pixels) -> Grid {
    let rows = field.height().div_ceil(2);
    let mut grid = Grid::new(field.width(), rows);
    for y in 0..rows {
        for x in 0..field.width() {
            let upper = field.lit(x, y * 2);
            let lower = field.lit(x, y * 2 + 1);
            grid.set(x, y, stacked(upper, lower));
        }
    }
    grid
}

/// Where the centre of a pixel falls in the view: -1 to 1 up the screen, and
/// the same across widened by the field's shape. A pixel is square — half a
/// cell, which is twice as tall as it is wide — so nothing here corrects for
/// anything but the aspect of the box itself.
fn view(x: u16, y: u16, width: u16, height: u16) -> (f32, f32) {
    let across = f32::from(width).max(1.0);
    let down = f32::from(height).max(1.0);
    let aspect = across / down;
    let u = ((f32::from(x) + 0.5) / across * 2.0 - 1.0) * aspect;
    let v = 1.0 - (f32::from(y) + 0.5) / down * 2.0;
    (u, v)
}

/// The pixel a point of the view falls in — [`view`] read the other way, for
/// the things that are projected onto the screen rather than marched into it.
/// `None` for a point outside the field.
pub fn pixel_at(u: f32, v: f32, width: u16, height: u16) -> Option<(u16, u16)> {
    let across = f32::from(width).max(1.0);
    let down = f32::from(height).max(1.0);
    let aspect = across / down;
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
    let (lamp, nearness) = lamp(marcher, scene, from, normal);
    let rim = (1.0 - normal.dot(ray.towards.times(-1.0)).abs())
        .clamp(0.0, 1.0)
        .powi(RIM_FALLOFF)
        * RIM;
    let warm = lamp + rim * nearness;
    let cool =
        (AMBIENT + sun(marcher, scene, from, normal) * SUN) * scene.sky + rim * (1.0 - nearness);
    let standing = (warm + cool).clamp(0.0, 1.0) * ruling(solid.material, hit);
    Lit {
        level: standing,
        warm: share(warm, warm + cool),
    }
}

/// The rule the floor wears where it wears one, and nothing at all on any
/// other surface: a grid drawn *on* the plane rather than built out of solids,
/// which costs the marcher not one step.
fn ruling(material: Material, hit: Hit) -> f32 {
    match material {
        Material::Ruled => ruled(hit.at, hit.travelled),
        _ => 1.0,
    }
}

/// The floor's grid at a point: full on a rule and [`BETWEEN`] off one, the
/// rule widening with distance so a far-off line stays a line.
fn ruled(point: Vec3, travelled: f32) -> f32 {
    let width = RULE_WIDTH * travelled.max(1.0);
    let edge = to_a_rule(point.x).min(to_a_rule(point.z));
    let on = 1.0 - (edge / width.max(f32::EPSILON)).min(1.0);
    BETWEEN + (1.0 - BETWEEN) * on
}

/// How far one coordinate stands from the nearest rule.
fn to_a_rule(along: f32) -> f32 {
    let into = along.rem_euclid(RULE);
    into.min(RULE - into)
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
///
/// Every body stands in the sun's way, the emissive block included: the
/// shadow it throws across the floor is the one thing in the piece that says
/// the block is *above* the floor rather than painted on it.
fn sun(marcher: &mut Marcher, scene: &Scene, from: Vec3, normal: Vec3) -> f32 {
    let facing = normal.dot(scene.sun);
    match facing * scene.sky > FAINT {
        true => facing * marcher.shadow(from, scene.sun, marcher.horizon(), Casting::Bodies),
        false => 0.0,
    }
}

/// The block's light on one surface, and how near that surface is to it —
/// which is also how warm its rim should read.
///
/// The lamp's own body is not in its way: the lamp stands at the middle of the
/// block, so a walk that counted it would find the block from every direction
/// and the world would have no lamp light in it at all.
fn lamp(marcher: &mut Marcher, scene: &Scene, from: Vec3, normal: Vec3) -> (f32, f32) {
    let offset = scene.lamp.at.minus(from);
    let distance = offset.length();
    let reach = scene.lamp.reach.max(f32::EPSILON);
    let nearness = 1.0 / (1.0 + (distance / reach) * (distance / reach));
    let facing = normal.dot(offset.unit()).max(0.0);
    let standing = facing * nearness * scene.lamp.strength;
    match standing > FAINT {
        true => (
            standing * marcher.shadow(from, offset.unit(), distance, Casting::ButTheLamp),
            nearness,
        ),
        false => (0.0, nearness),
    }
}

/// Below this a light would not move the pixel off the ink it is already
/// wearing, so what stands in its way does not matter.
const FAINT: f32 = 0.01;

/// Distance eating light. Nothing far is bright, which is the whole of how a
/// floor reads as running away to a vanishing point.
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
/// of — the guard that keeps a black pixel's warmth out of the arithmetic.
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
    use crate::painted::{daylight, in_look, no_colour, truecolor};

    /// A field wide enough to read a shape in, and its own two-to-one box.
    const BOARD: (u16, u16) = (60, 40);

    fn ball(radius: f32) -> Shape {
        // A ball is a block with no extent and a rounded edge: one primitive
        // fewer, and the same surface.
        Shape::Block {
            at: at(0.0, 0.0, 0.0),
            half: at(0.0, 0.0, 0.0),
            round: radius,
            spin: 0.0,
        }
    }

    fn lone(shape: Shape) -> Scene {
        Scene {
            solids: vec![Solid::of(shape)],
            sun: at(-0.4, 0.8, -0.45).unit(),
            ..Scene::default()
        }
    }

    fn field(scene: &Scene) -> Pixels {
        pixels(scene, &Camera::default(), BOARD.0, BOARD.1).0
    }

    fn drawn(scene: &Scene) -> String {
        halves(&field(scene))
            .lines()
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// One pixel's brightness read back off the field.
    fn lit_at(field: &Pixels, x: u16, y: u16) -> f32 {
        field.lit(x, y).level
    }

    /// The packing, pinned: a hand-built 4×2 field, both palettes.
    #[test]
    fn two_pixel_rows_pack_into_one_row_of_half_blocks() {
        // Dark over dark, lit over dark, dark over lit, lit over lit — the
        // four cases a cell can be in.
        let mut field = Pixels::new(4, 2);
        field.set(
            1,
            0,
            Lit {
                level: 0.9,
                warm: 0.0,
            },
        );
        field.set(
            2,
            1,
            Lit {
                level: 0.9,
                warm: 0.0,
            },
        );
        field.set(
            3,
            0,
            Lit {
                level: 0.9,
                warm: 1.0,
            },
        );
        field.set(
            3,
            1,
            Lit {
                level: 0.3,
                warm: 0.0,
            },
        );

        let cells = |theme| {
            crate::theme::with(theme, || {
                let grid = halves(&field);
                assert_eq!(grid.height(), 1, "two pixel rows are one cell row");
                assert_eq!(grid.lines()[0].to_string(), " ▀▄▀");
                (0..4)
                    .map(|x| {
                        let cell = grid.cell(x, 0);
                        format!(
                            "{} fg {:?} bg {:?}",
                            cell.glyph, cell.style.fg, cell.style.bg
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        };
        // The glyphs are packed once; only the ink differs between the looks,
        // which is what the two fixtures are for.
        insta::assert_snapshot!("halves_dark", cells(truecolor()));
        insta::assert_snapshot!("halves_light", cells(daylight()));
    }

    #[test]
    fn a_pixel_with_no_light_in_it_is_left_to_the_terminals_own_ground() {
        in_look(truecolor(), || {
            let nothing = stacked(Lit::default(), Lit::default());
            assert_eq!(nothing, Cell::default(), "no glyph and no colour");
            let above = stacked(
                Lit {
                    level: 0.5,
                    warm: 0.0,
                },
                Lit::default(),
            );
            assert_eq!(above.glyph, UPPER);
            assert_eq!(above.style.bg, None, "and no ground painted under it");
            String::new()
        });
    }

    #[test]
    fn a_lit_face_is_brighter_than_one_turned_away() {
        let scene = lone(ball(1.6));
        let field = field(&scene);
        // The sun stands up and to the left, so the top-left of the ball is
        // its key side and the bottom-right is its shadow side.
        let key = lit_at(&field, 22, 12);
        let shadow = lit_at(&field, 38, 28);
        assert!(key > shadow, "key {key} should beat shadow {shadow}");
    }

    #[test]
    fn distance_eats_light() {
        let plain = |fog| Scene {
            fog,
            ..lone(Shape::Ground { y: -1.0 })
        };
        let clear = lit_at(&field(&plain(0.0)), 30, 39);
        let foggy = lit_at(&field(&plain(0.25)), 30, 39);
        assert!(clear > foggy, "clear {clear} should beat foggy {foggy}");
    }

    /// The floor's rules: bright on a line, dim between, and a line that is
    /// wider the further off it stands.
    #[test]
    fn a_ruled_floor_is_bright_on_its_lines_and_dim_between_them() {
        assert!(ruled(at(0.0, -1.0, 4.0), 4.0) > 0.9, "on a rule");
        assert!(ruled(at(0.5, -1.0, 4.5), 4.0) < 0.2, "and between two");
        let near = ruled(at(0.06, -1.0, 4.5), 1.0);
        let far = ruled(at(0.06, -1.0, 4.5), 20.0);
        assert!(far > near, "a far rule is wider: {near} then {far}");
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
        let field = field(&scene);
        assert!(
            lit_at(&field, 30, 19) > 0.7,
            "the block itself is nearly the brightest thing there is: {}",
            lit_at(&field, 30, 19)
        );
        assert!(
            lit_at(&field, 28, 19) > 0.0,
            "the air beside it carries a halo"
        );
    }

    /// The shadow the whole first shot stands on: a glowing block above a
    /// floor puts a dark shape on it, because the sun counts every body.
    #[test]
    fn a_block_over_a_floor_throws_a_shadow_across_it() {
        let scene = Scene {
            solids: vec![
                Solid::of(Shape::Ground { y: -0.7 }).ruled(),
                Solid::of(Shape::Block {
                    at: at(0.0, 0.1, 0.0),
                    half: at(0.3, 0.3, 0.3),
                    round: 0.02,
                    spin: 0.0,
                })
                .lit(),
            ],
            // Straight down, so the shadow lands under the block.
            sun: at(0.0, 1.0, 0.0),
            sky: 1.0,
            ..Scene::default()
        };
        let mut marcher = Marcher::new(&scene);
        let under = at(0.0, -0.69, 0.0);
        let beside = at(2.0, -0.69, 0.0);
        let up = at(0.0, 1.0, 0.0);
        assert!(
            marcher.shadow(under, up, 9.0, Casting::Bodies) < 0.05,
            "the floor under the block is in its shadow"
        );
        assert!(
            marcher.shadow(beside, up, 9.0, Casting::Bodies) > 0.9,
            "and the floor beside it is not"
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
        let scene = lone(ball(1.6));
        let plain = in_look(no_colour(), || drawn(&scene));
        let painted = in_look(truecolor(), || drawn(&scene));
        assert_eq!(plain, painted, "the glyphs carry the world, not the colour");
    }

    #[test]
    fn a_pixel_off_the_field_has_nothing_projected_into_it() {
        assert_eq!(pixel_at(9.0, 0.0, 60, 40), None);
        assert_eq!(pixel_at(0.0, -9.0, 60, 40), None);
        assert_eq!(pixel_at(f32::NAN, 0.0, 60, 40), None);
    }

    #[test]
    fn the_middle_of_the_view_is_the_middle_of_the_field() {
        let (x, y) = pixel_at(0.0, 0.0, 60, 40).expect("the middle is on screen");
        assert_eq!((x, y), (30, 20));
        let (u, v) = view(30, 20, 60, 40);
        assert!(u.abs() < 0.05 && v.abs() < 0.05, "{u} {v}");
    }

    /// A pixel is square: a ball comes out as wide as it is tall, in pixels.
    #[test]
    fn a_pixel_is_square_so_nothing_round_comes_out_oval() {
        let scene = lone(ball(1.0));
        let field = field(&scene);
        let lit = |x, y| lit_at(&field, x, y) > 0.0;
        let across = (0..BOARD.0).filter(|x| lit(*x, 20)).count();
        let down = (0..BOARD.1).filter(|y| lit(30, *y)).count();
        assert!(
            across.abs_diff(down) <= 2,
            "a ball is {across} across and {down} down"
        );
    }
}

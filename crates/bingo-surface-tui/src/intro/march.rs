//! Walking a ray through the world until it touches something.
//!
//! Sphere tracing: from where the ray is, ask the world how far the nearest
//! surface is ([`super::sdf`]), step that far, ask again. The step can never
//! pass through anything, so a handful of steps land on the skin of whatever
//! the ray was pointed at — and the same walk, run from a surface towards a
//! light, is what a shadow is.
//!
//! Everything here is a pure function of the world and the ray. The one piece
//! of state is the tally of steps a [`Marcher`] keeps, which is what the frame
//! budget is asserted against: steps are the same number on every machine,
//! and milliseconds are not.

use super::embers::Ember;
use super::sdf::{Shape, Vec3, at};

/// What the world is made of, as the marcher needs it.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub solids: Vec<Solid>,
    /// The one point light: the cursor block's own glow. Nothing else in the
    /// opening emits.
    pub lamp: Lamp,
    /// Where the one directional light comes from — a unit vector pointing
    /// *at* the sun, as a surface normal would to face it.
    pub sun: Vec3,
    /// How much light the world has of its own, sun and ambient together. A
    /// shot that is meant to be dark turns this down rather than moving the
    /// sun away, so the one thing that lights it is the block.
    pub sky: f32,
    /// How fast distance eats light. Zero is a clear world.
    pub fog: f32,
    /// How brightly the whole world is seen. The last shot takes it to
    /// nothing as the welcome box comes up through it.
    pub exposure: f32,
    /// The points that rise through the world. They are not solids: nothing
    /// is lit by them and nothing is occluded by them but the air.
    pub embers: Vec<Ember>,
}

impl Default for Scene {
    fn default() -> Self {
        Scene {
            solids: Vec::new(),
            lamp: Lamp::default(),
            sun: at(0.0, 1.0, 0.0),
            sky: 1.0,
            fog: 0.0,
            exposure: 1.0,
            embers: Vec::new(),
        }
    }
}

/// One thing in the world: a shape, and what light does when it lands on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Solid {
    pub shape: Shape,
    pub material: Material,
}

impl Solid {
    pub fn of(shape: Shape) -> Self {
        Solid {
            shape,
            material: Material::Matte,
        }
    }

    /// The same solid, lit from inside.
    pub fn lit(self) -> Self {
        Solid {
            material: Material::Emissive,
            ..self
        }
    }

    /// The same solid with a grid ruled on it.
    pub fn ruled(self) -> Self {
        Solid {
            material: Material::Ruled,
            ..self
        }
    }

    /// The same solid, wearing a picture: what light stands on it is read off
    /// the picture rather than worked out from the lights.
    pub fn pictured(self) -> Self {
        Solid {
            material: Material::Pictured,
            ..self
        }
    }

    fn distance(&self, point: Vec3) -> f32 {
        self.shape.distance(point)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Material {
    #[default]
    Matte,
    /// Its own light. It is not asked what falls on it, and it casts no
    /// shadow — it *is* the lamp, and a lamp does not stand in its own way.
    Emissive,
    /// A picture, standing in the world as a surface: it is marched, fogged
    /// and occluded like anything else, and what it looks like comes off the
    /// picture's own pixels. It casts no shadow, because a picture is not a
    /// body.
    Pictured,
    /// Matte, with a grid ruled on it: the floor. The rules are drawn where
    /// the ray landed rather than built out of solids, so a floor that runs to
    /// the horizon costs the marcher exactly one plane.
    Ruled,
}

/// Which bodies may stand in a light's way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Casting {
    /// Every body in the world. A picture is not a body and never casts one.
    Bodies,
    /// The same, less whatever is its own light: the lamp stands at the middle
    /// of the block, so a walk towards it that counted the block would find it
    /// from every direction and nothing in the world would be lit at all.
    ButTheLamp,
}

impl Casting {
    fn counts(self, material: Material) -> bool {
        !matches!(
            (self, material),
            (_, Material::Pictured) | (Casting::ButTheLamp, Material::Emissive)
        )
    }
}

/// The one point light.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Lamp {
    pub at: Vec3,
    /// The distance at which it has fallen to a quarter of its strength.
    pub reach: f32,
    /// How bright it is where it stands.
    pub strength: f32,
}

/// Where the world is looked at from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub eye: Vec3,
    pub at: Vec3,
    /// How wide the view is, as the half-height of the plane one unit ahead.
    pub lens: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            eye: at(0.0, 0.0, -5.0),
            at: at(0.0, 0.0, 0.0),
            lens: 0.6,
        }
    }
}

/// The world's up, which the camera is levelled against.
const UP: Vec3 = at(0.0, 1.0, 0.0);

impl Camera {
    /// Where the camera's own ahead, right and up point in the world.
    ///
    /// `up × ahead`, so a camera looking down `+z` has the world's `+x` on
    /// the right of the screen — which is the way the scenes are written, and
    /// the way a mirrored picture is not.
    ///
    /// A camera looking straight up or down has no `right` to speak of — its
    /// ahead *is* the world's up and the cross of the two is zero — so the
    /// world's own x is what it settles on rather than a `NaN`.
    fn basis(&self) -> (Vec3, Vec3, Vec3) {
        let ahead = self.at.minus(self.eye).unit();
        let across = UP.cross(ahead);
        let right = match across.length() > 1e-3 {
            true => across.unit(),
            false => at(1.0, 0.0, 0.0),
        };
        (ahead, right, ahead.cross(right).unit())
    }

    /// The ray through a point of the view, `u` across and `v` up, each from
    /// -1 at one edge to 1 at the other. The caller has already corrected `u`
    /// for the shape of a cell; this only knows about the world.
    pub fn ray(&self, u: f32, v: f32) -> Ray {
        let (ahead, right, up) = self.basis();
        Ray {
            from: self.eye,
            towards: ahead
                .plus(right.times(u * self.lens))
                .plus(up.times(v * self.lens))
                .unit(),
        }
    }

    /// Where a point in the world falls in the view, in the same `u`/`v` as
    /// [`Camera::ray`] takes, with how far ahead of the camera it is. `None`
    /// for a point behind the lens, which has no place on the screen at all.
    pub fn project(&self, point: Vec3) -> Option<(f32, f32, f32)> {
        let (ahead, right, up) = self.basis();
        let offset = point.minus(self.eye);
        let depth = offset.dot(ahead);
        (depth > 0.05).then(|| {
            let scale = 1.0 / (depth * self.lens);
            (offset.dot(right) * scale, offset.dot(up) * scale, depth)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ray {
    pub from: Vec3,
    pub towards: Vec3,
}

impl Ray {
    pub fn along(&self, distance: f32) -> Vec3 {
        self.from.plus(self.towards.times(distance))
    }
}

/// Where a ray stopped.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub at: Vec3,
    /// How far the ray travelled to get here — what the fog is charged on.
    pub travelled: f32,
    /// Which of the scene's solids it landed on.
    pub solid: usize,
}

/// How many times one ray may ask the world where the nearest surface is.
pub const STEPS: u16 = 56;
/// The same, for the shorter walk towards a light.
pub const SHADOW_STEPS: u16 = 14;
/// How far a ray goes before the world is called empty in that direction.
pub const FAR: f32 = 45.0;
/// Close enough to a surface to call it touched, as a share of how far the
/// ray has come: a surface fifty units away needs nothing like the precision
/// of one at arm's length, and charging it that costs steps for nothing.
const SKIN: f32 = 0.0025;
/// How little of a surface's light has to survive the fog before the fog is
/// the answer. A ray running down a corridor grazes its walls and spends
/// every step it is given; stopping it where nothing it could still find
/// would be brighter than the ramp's first step is most of the frame budget.
const SWALLOWED: f32 = 0.02;
/// How wide a shadow's edge spreads. Larger is harder.
const SOFTNESS: f32 = 12.0;

/// One walk of the world, and what it spent.
pub struct Marcher<'a> {
    scene: &'a Scene,
    steps: u64,
}

impl<'a> Marcher<'a> {
    pub fn new(scene: &'a Scene) -> Self {
        Marcher { scene, steps: 0 }
    }

    /// Every step this marcher has taken, over every ray it has walked.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// Walk `ray` until it touches something, or until the world runs out.
    pub fn cast(&mut self, ray: Ray) -> Option<Hit> {
        let horizon = self.horizon();
        let mut travelled = 0.0f32;
        for _ in 0..STEPS {
            self.steps += 1;
            let here = ray.along(travelled);
            let (distance, solid) = self.nearest(here);
            if distance < SKIN * travelled.max(1.0) {
                return Some(Hit {
                    at: here,
                    travelled,
                    solid,
                });
            }
            travelled += distance;
            if travelled > horizon {
                break;
            }
        }
        None
    }

    /// How far this world can be seen into at all: where the fog has taken
    /// all but [`SWALLOWED`] of whatever might be standing there.
    pub fn horizon(&self) -> f32 {
        match self.scene.fog > f32::EPSILON {
            true => (-SWALLOWED.ln() / self.scene.fog).min(FAR),
            false => FAR,
        }
    }

    /// How much of the light at `towards` reaches `from`: 1 in the open, 0
    /// behind something, and the values between where an edge passes close to
    /// the ray — which is what makes a shadow soft rather than cut out.
    pub fn shadow(&mut self, from: Vec3, towards: Vec3, far: f32, casting: Casting) -> f32 {
        let mut light = 1.0f32;
        let mut travelled = 0.05;
        for _ in 0..SHADOW_STEPS {
            self.steps += 1;
            let distance = self.nearest_opaque(from.plus(towards.times(travelled)), casting);
            if distance < 1e-3 {
                return 0.0;
            }
            light = light.min(SOFTNESS * distance / travelled);
            travelled += distance.clamp(0.05, 2.0);
            if travelled > far {
                break;
            }
        }
        light.clamp(0.0, 1.0)
    }

    /// Which way the surface at `point` faces, read off the slope of the
    /// distance around it. Four samples in a tetrahedron rather than six on
    /// the axes: the same normal for two thirds of the steps.
    pub fn normal(&mut self, point: Vec3) -> Vec3 {
        const NUDGE: f32 = 0.002;
        let corners = [
            at(1.0, -1.0, -1.0),
            at(-1.0, -1.0, 1.0),
            at(-1.0, 1.0, -1.0),
            at(1.0, 1.0, 1.0),
        ];
        let mut slope = at(0.0, 0.0, 0.0);
        for corner in corners {
            self.steps += 1;
            let distance = self.nearest(point.plus(corner.times(NUDGE))).0;
            slope = slope.plus(corner.times(distance));
        }
        slope.unit()
    }

    /// The nearest surface to `point`, and whose it is. An empty world is
    /// [`FAR`] away in every direction rather than infinitely, so a step is
    /// always a number.
    fn nearest(&self, point: Vec3) -> (f32, usize) {
        let mut nearest = (FAR, 0);
        for (index, solid) in self.scene.solids.iter().enumerate() {
            let distance = solid.distance(point);
            if distance < nearest.0 {
                nearest = (distance, index);
            }
        }
        nearest
    }

    /// The same, counting only what may stand in this light's way.
    fn nearest_opaque(&self, point: Vec3, casting: Casting) -> f32 {
        self.scene
            .solids
            .iter()
            .filter(|solid| casting.counts(solid.material))
            .map(|solid| solid.distance(point))
            .fold(FAR, f32::min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ball is a block with no extent and a rounded edge.
    fn ball_at(centre: Vec3, radius: f32) -> Shape {
        Shape::Block {
            at: centre,
            half: at(0.0, 0.0, 0.0),
            round: radius,
            spin: 0.0,
        }
    }

    fn ball(radius: f32) -> Scene {
        Scene {
            solids: vec![Solid::of(ball_at(at(0.0, 0.0, 0.0), radius))],
            ..Scene::default()
        }
    }

    #[test]
    fn a_ray_pointed_at_a_ball_lands_on_its_near_face() {
        let scene = ball(1.0);
        let mut marcher = Marcher::new(&scene);
        let hit = marcher
            .cast(Ray {
                from: at(0.0, 0.0, -5.0),
                towards: at(0.0, 0.0, 1.0),
            })
            .expect("the ray meets the sphere");
        assert!((hit.travelled - 4.0).abs() < 0.02, "{}", hit.travelled);
        assert!((hit.at.z + 1.0).abs() < 0.02, "{:?}", hit.at);
    }

    #[test]
    fn a_ray_pointed_at_nothing_comes_back_empty() {
        let scene = ball(1.0);
        let mut marcher = Marcher::new(&scene);
        assert!(
            marcher
                .cast(Ray {
                    from: at(0.0, 0.0, -5.0),
                    towards: at(0.0, 1.0, 0.0),
                })
                .is_none()
        );
    }

    #[test]
    fn the_normal_of_a_ball_points_away_from_its_centre() {
        let scene = ball(1.0);
        let mut marcher = Marcher::new(&scene);
        let normal = marcher.normal(at(0.0, 0.0, -1.0));
        assert!((normal.z + 1.0).abs() < 0.05, "{normal:?}");
    }

    #[test]
    fn a_solid_between_a_point_and_a_light_puts_it_in_shadow() {
        let scene = ball(1.0);
        let mut marcher = Marcher::new(&scene);
        let behind = at(0.0, 0.0, 4.0);
        let lit = marcher.shadow(behind, at(0.0, 1.0, 0.0), 10.0, Casting::Bodies);
        let shaded = marcher.shadow(behind, at(0.0, 0.0, -1.0), 10.0, Casting::Bodies);
        assert!(lit > 0.9, "nothing stands overhead: {lit}");
        assert!(shaded < 0.05, "the sphere stands in the way: {shaded}");
    }

    /// A lamp's body is a body: it stands in the sun's way like anything
    /// else, which is what puts a shadow on the floor under a glowing block —
    /// but it is never in the way of its own light, whose walks all end
    /// inside it.
    #[test]
    fn a_lamp_casts_a_shadow_of_its_body_but_never_of_its_own_light() {
        let scene = Scene {
            solids: vec![Solid::of(ball_at(at(0.0, 0.0, 0.0), 1.0)).lit()],
            ..Scene::default()
        };
        let mut marcher = Marcher::new(&scene);
        let from = at(0.0, 0.0, 4.0);
        let towards = at(0.0, 0.0, -1.0);
        assert!(
            marcher.shadow(from, towards, 10.0, Casting::Bodies) < 0.05,
            "a body stands in the sun's way whether it glows or not"
        );
        assert!(
            marcher.shadow(from, towards, 10.0, Casting::ButTheLamp) > 0.9,
            "and the light itself is not in its own way"
        );
    }

    /// A picture is not a body: it is drawn where it stands and shadows
    /// nothing.
    #[test]
    fn a_picture_stands_in_no_lights_way() {
        let scene = Scene {
            solids: vec![
                Solid::of(Shape::Block {
                    at: at(0.0, 0.0, 0.0),
                    half: at(2.0, 2.0, 0.02),
                    round: 0.0,
                    spin: 0.0,
                })
                .pictured(),
            ],
            ..Scene::default()
        };
        let mut marcher = Marcher::new(&scene);
        let through = marcher.shadow(at(0.0, 0.0, 4.0), at(0.0, 0.0, -1.0), 10.0, Casting::Bodies);
        assert!(through > 0.9, "{through}");
    }

    #[test]
    fn a_camera_looking_straight_down_still_has_a_ray() {
        let camera = Camera {
            eye: at(0.0, 5.0, 0.0),
            at: at(0.0, 0.0, 0.0),
            lens: 0.6,
        };
        let ray = camera.ray(0.4, -0.3);
        assert!(ray.towards.x.is_finite(), "{:?}", ray.towards);
        assert!((ray.towards.length() - 1.0).abs() < 1e-4);
    }

    #[test]
    fn every_ray_out_of_a_camera_is_one_unit_long() {
        let camera = Camera::default();
        for u in [-1.0, -0.3, 0.0, 0.7, 1.0] {
            for v in [-1.0, 0.0, 1.0] {
                let ray = camera.ray(u, v);
                assert!((ray.towards.length() - 1.0).abs() < 1e-4, "{u} {v}");
            }
        }
    }

    #[test]
    fn a_point_projects_back_to_the_ray_that_finds_it() {
        let camera = Camera {
            eye: at(1.0, 2.0, -6.0),
            at: at(0.0, 0.0, 0.0),
            lens: 0.7,
        };
        let (u, v, depth) = camera
            .project(at(2.0, -1.0, 3.0))
            .expect("ahead of the lens");
        let ray = camera.ray(u, v);
        let back = ray.along(depth / ray.towards.dot(camera.basis().0));
        assert!((back.x - 2.0).abs() < 1e-3, "{back:?}");
        assert!((back.y + 1.0).abs() < 1e-3, "{back:?}");
        assert!((back.z - 3.0).abs() < 1e-3, "{back:?}");
    }

    #[test]
    fn a_point_behind_the_lens_has_no_place_on_the_screen() {
        assert!(Camera::default().project(at(0.0, 0.0, -9.0)).is_none());
    }

    #[test]
    fn a_walk_that_finds_nothing_still_stops() {
        let scene = Scene::default();
        let mut marcher = Marcher::new(&scene);
        assert!(marcher.cast(Camera::default().ray(0.0, 0.0)).is_none());
        assert!(marcher.steps() <= u64::from(STEPS));
    }
}

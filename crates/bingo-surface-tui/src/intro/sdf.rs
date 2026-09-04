//! The world, as the distance to it.
//!
//! Every solid in the opening answers one question — *from this point, how
//! far is your surface?* — and that one answer is enough to walk a ray up to
//! it ([`super::march`]), to find which way it faces, and to know whether
//! something stands between it and a light. Nothing here draws; nothing here
//! knows about a terminal.
//!
//! Distances are signed: negative inside the solid, zero on its skin. Every
//! one of them is a *lower bound* on the true distance, which is what makes
//! sphere tracing safe — a step of `d` can never pass through a surface.

/// A point in the world, or a direction through it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

/// A point, spelled the way the scenes read.
pub const fn at(x: f32, y: f32, z: f32) -> Vec3 {
    Vec3 { x, y, z }
}

impl Vec3 {
    pub fn plus(self, other: Vec3) -> Vec3 {
        at(self.x + other.x, self.y + other.y, self.z + other.z)
    }

    pub fn minus(self, other: Vec3) -> Vec3 {
        at(self.x - other.x, self.y - other.y, self.z - other.z)
    }

    pub fn times(self, scale: f32) -> Vec3 {
        at(self.x * scale, self.y * scale, self.z * scale)
    }

    pub fn dot(self, other: Vec3) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    pub fn cross(self, other: Vec3) -> Vec3 {
        at(
            self.y * other.z - self.z * other.y,
            self.z * other.x - self.x * other.z,
            self.x * other.y - self.y * other.x,
        )
    }

    pub fn length(self) -> f32 {
        self.dot(self).max(0.0).sqrt()
    }

    /// The same direction, one unit long. A zero vector keeps its length,
    /// because a direction that points nowhere has no unit form and dividing
    /// by nothing would put a `NaN` into the march.
    pub fn unit(self) -> Vec3 {
        let length = self.length();
        match length > f32::EPSILON {
            true => self.times(1.0 / length),
            false => self,
        }
    }

    /// Each part's distance from zero — the fold a box's distance is built on.
    pub fn abs(self) -> Vec3 {
        at(self.x.abs(), self.y.abs(), self.z.abs())
    }

    /// Each part raised to at least `floor`.
    pub fn floored(self, floor: f32) -> Vec3 {
        at(self.x.max(floor), self.y.max(floor), self.z.max(floor))
    }

    /// The largest of the three.
    pub fn largest(self) -> f32 {
        self.x.max(self.y).max(self.z)
    }
}

/// One solid's shape. No shape holds another: a scene is a list, and the two
/// ways of joining them — the nearest of many, and one taken out of another —
/// live in [`super::scenes::Solid`] where the list does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    Sphere {
        at: Vec3,
        radius: f32,
    },
    /// A box with its edges rounded off by `round`, spun about the world's
    /// up axis by `spin` radians. The rounding is what gives an edge a lit
    /// rim instead of a hard corner, which is what a person reads as depth.
    Block {
        at: Vec3,
        half: Vec3,
        round: f32,
        spin: f32,
    },
    Torus {
        at: Vec3,
        major: f32,
        minor: f32,
    },
    /// The ground, level at `y`.
    Ground {
        y: f32,
    },
    /// A [`Shape::Block`] at every point of a grid without end: the world is
    /// folded into one cell of `period` and the block is measured inside it.
    /// One shape, however far the corridor runs.
    Lattice {
        period: Vec3,
        half: Vec3,
        round: f32,
    },
}

impl Shape {
    /// How far `point` is from this solid's surface.
    pub fn distance(&self, point: Vec3) -> f32 {
        match *self {
            Shape::Sphere { at, radius } => point.minus(at).length() - radius,
            Shape::Block {
                at,
                half,
                round,
                spin,
            } => block(spun(point.minus(at), -spin), half, round),
            Shape::Torus { at, major, minor } => torus(point.minus(at), major, minor),
            Shape::Ground { y } => point.y - y,
            Shape::Lattice {
                period,
                half,
                round,
            } => block(folded(point, period), half, round),
        }
    }
}

/// The distance to a rounded box centred on the origin: the classic fold —
/// outside is the length of what is beyond the box in each axis, inside is
/// how far the nearest wall is.
fn block(point: Vec3, half: Vec3, round: f32) -> f32 {
    let beyond = point.abs().minus(half);
    beyond.floored(0.0).length() + beyond.largest().min(0.0) - round
}

/// The distance to a ring lying in the world's `xz` plane.
fn torus(point: Vec3, major: f32, minor: f32) -> f32 {
    let ring = at(point.x, point.z, 0.0).length() - major;
    at(ring, point.y, 0.0).length() - minor
}

/// `point` turned about the up axis by `spin` radians.
fn spun(point: Vec3, spin: f32) -> Vec3 {
    let (sin, cos) = spin.sin_cos();
    at(
        point.x * cos - point.z * sin,
        point.y,
        point.x * sin + point.z * cos,
    )
}

/// The world folded into one cell of the grid, `point` brought to where it
/// falls inside it. A period of zero on an axis leaves that axis alone, so a
/// lattice can run endlessly in one direction and be finite in another.
fn folded(point: Vec3, period: Vec3) -> Vec3 {
    at(
        wrapped(point.x, period.x),
        wrapped(point.y, period.y),
        wrapped(point.z, period.z),
    )
}

fn wrapped(coordinate: f32, period: f32) -> f32 {
    match period > f32::EPSILON {
        true => coordinate - period * (coordinate / period).round(),
        false => coordinate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CENTRE: Vec3 = at(0.0, 0.0, 0.0);

    #[test]
    fn a_sphere_is_its_radius_away_from_its_centre() {
        let ball = Shape::Sphere {
            at: CENTRE,
            radius: 2.0,
        };
        assert!(
            (ball.distance(CENTRE) + 2.0).abs() < 1e-5,
            "inside is signed"
        );
        assert!(
            (ball.distance(at(2.0, 0.0, 0.0))).abs() < 1e-5,
            "on the skin"
        );
        assert!((ball.distance(at(5.0, 0.0, 0.0)) - 3.0).abs() < 1e-5);
    }

    #[test]
    fn a_block_measures_the_nearest_wall_from_inside_and_the_corner_from_out() {
        let cube = Shape::Block {
            at: CENTRE,
            half: at(1.0, 1.0, 1.0),
            round: 0.0,
            spin: 0.0,
        };
        assert!((cube.distance(CENTRE) + 1.0).abs() < 1e-5);
        assert!((cube.distance(at(2.0, 0.0, 0.0)) - 1.0).abs() < 1e-5);
        let corner = cube.distance(at(2.0, 2.0, 0.0));
        assert!((corner - 2f32.sqrt()).abs() < 1e-5, "{corner}");
    }

    #[test]
    fn a_spun_block_carries_its_corners_round_with_it() {
        let square = |spin| Shape::Block {
            at: CENTRE,
            half: at(1.0, 1.0, 1.0),
            round: 0.0,
            spin,
        };
        let probe = at(1.3, 0.0, 0.0);
        assert!(square(0.0).distance(probe) > 0.0, "outside the flat face");
        assert!(
            square(std::f32::consts::FRAC_PI_4).distance(probe) < 0.0,
            "the corner has swung over the probe"
        );
    }

    #[test]
    fn a_torus_is_a_ring_about_the_up_axis() {
        let ring = Shape::Torus {
            at: CENTRE,
            major: 2.0,
            minor: 0.5,
        };
        assert!(
            (ring.distance(at(2.0, 0.5, 0.0))).abs() < 1e-5,
            "on the skin"
        );
        assert!(
            (ring.distance(CENTRE) - 1.5).abs() < 1e-5,
            "through the hole"
        );
    }

    #[test]
    fn the_ground_is_signed_by_which_side_of_it_you_stand() {
        let floor = Shape::Ground { y: -1.0 };
        assert!((floor.distance(CENTRE) - 1.0).abs() < 1e-5);
        assert!(floor.distance(at(0.0, -3.0, 0.0)) < 0.0);
    }

    #[test]
    fn a_lattice_repeats_one_block_down_the_grid() {
        let grid = Shape::Lattice {
            period: at(4.0, 0.0, 4.0),
            half: at(1.0, 1.0, 1.0),
            round: 0.0,
        };
        for cell in 0..4 {
            let centre = at(4.0 * cell as f32, 0.0, 0.0);
            assert!(grid.distance(centre) < 0.0, "cell {cell} has a block in it");
            assert!(
                grid.distance(centre.plus(at(2.0, 0.0, 0.0))) > 0.0,
                "and a gap between them"
            );
        }
    }

    #[test]
    fn a_lattice_axis_with_no_period_does_not_repeat() {
        let wall = Shape::Lattice {
            period: at(4.0, 0.0, 0.0),
            half: at(1.0, 1.0, 1.0),
            round: 0.0,
        };
        assert!(wall.distance(at(0.0, 20.0, 0.0)) > 18.0, "up is not folded");
    }

    #[test]
    fn a_direction_that_points_nowhere_has_no_nan_in_it() {
        let unit = at(0.0, 0.0, 0.0).unit();
        assert!(unit.x.is_finite() && unit.y.is_finite() && unit.z.is_finite());
    }

    #[test]
    fn every_distance_is_a_lower_bound_on_the_true_one() {
        let shapes = [
            Shape::Sphere {
                at: at(1.0, 0.0, 2.0),
                radius: 1.5,
            },
            Shape::Block {
                at: at(-1.0, 0.5, 0.0),
                half: at(0.7, 1.2, 0.4),
                round: 0.1,
                spin: 0.6,
            },
            Shape::Torus {
                at: CENTRE,
                major: 2.0,
                minor: 0.4,
            },
        ];
        // Stepping by the reported distance may touch a surface but never
        // crosses one: that is the whole contract the marcher stands on.
        for shape in shapes {
            for step in 0..40 {
                let from = at(-6.0 + step as f32 * 0.3, 0.2, 0.3);
                let reported = shape.distance(from);
                if reported <= 0.0 {
                    continue;
                }
                let walked = from.plus(at(1.0, 0.0, 0.0).times(reported * 0.999));
                assert!(
                    shape.distance(walked) > -1e-4,
                    "a step of {reported} passed through {shape:?}"
                );
            }
        }
    }
}

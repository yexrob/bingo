//! The one point of light that draws the welcome box's border, and the tail
//! behind it.
//!
//! The perimeter is walked as a single path, clockwise from the top-left
//! corner, so a corner is just another cell of it and the light turns one
//! without knowing it did.

/// How much of the perimeter the tail covers.
///
/// A share, and not a count of cells: the light crosses a box of 120 columns
/// in the same nine tenths of a second it crosses one of 80, so a tail of a
/// fixed length would be *shorter* than the head's own travel between two
/// frames on the wider box — and a tail shorter than that reads as a strobe
/// rather than as one light moving.
const TAIL: f32 = 0.22;

/// What one cell of the box is to the line drawing its border.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edge {
    /// Inside the border, where the line never goes.
    Interior,
    /// On the border, before the light has come to it.
    Dark,
    /// On the border, with light of this age on it: 0 under the head, 1 where
    /// it has cooled to the hairline the border rests as.
    Lit(f32),
}

/// The box's cells, row by row, as the light has left them by the time it is
/// `come` of the way round.
///
/// The head runs a tail's length *past* home, so every cell has cooled to the
/// hairline by the end of the lap — the same reason [`crate::clock::sweep`]
/// leaves past the last cell of its run.
///
/// `come` is spent evenly: a light running a border has one speed, and the
/// easings the surface has are too steep for a run this long. Cubic ease-in-out
/// puts three times the average speed in the middle of the beat, which draws
/// four fifths of the perimeter in its middle third and leaves the rest of the
/// beat with nothing moving on the screen.
pub fn lap(come: f32, width: usize, height: usize) -> Vec<Edge> {
    let mut cells = vec![Edge::Interior; width * height];
    let path = path(width, height);
    let tail = (path.len() as f32 * TAIL).max(1.0);
    let head = come.clamp(0.0, 1.0) * (path.len() as f32 + tail);
    for (step, (x, y)) in path.into_iter().enumerate() {
        let behind = head - step as f32;
        let edge = match behind < 0.0 {
            true => Edge::Dark,
            false => Edge::Lit((behind / tail).min(1.0)),
        };
        if let Some(slot) = cells.get_mut(y * width + x) {
            *slot = edge;
        }
    }
    cells
}

/// The perimeter of a `width` × `height` box, clockwise from its top-left
/// corner. A box one cell across or one row tall is a single run, which is the
/// whole of its perimeter.
fn path(width: usize, height: usize) -> Vec<(usize, usize)> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    if width == 1 {
        return (0..height).map(|y| (0, y)).collect();
    }
    if height == 1 {
        return (0..width).map(|x| (x, 0)).collect();
    }
    let down = || 1..height - 1;
    let across = (0..width).map(|x| (x, 0));
    let right = down().map(|y| (width - 1, y));
    let back = (0..width).rev().map(|x| (x, height - 1));
    let left = down().rev().map(|y| (0, y));
    across.chain(right).chain(back).chain(left).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lap as a picture: `.` inside the border, ` ` where the line has not
    /// come, and the age of the light as one digit where it has — `0` under
    /// the head, `9` cooled to the hairline.
    fn drawn(come: f32, width: usize, height: usize) -> String {
        lap(come, width, height)
            .chunks(width)
            .map(|row| {
                row.iter()
                    .map(|edge| match edge {
                        Edge::Interior => '.',
                        Edge::Dark => ' ',
                        Edge::Lit(age) => {
                            char::from_digit((age * 9.0).round() as u32, 10).unwrap_or('?')
                        }
                    })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The fixture: a box of ten by three at four points of the lap. The path
    /// is 22 cells and the tail a fifth of it, so the head runs to 26.84 and
    /// every cell has cooled by the end.
    #[test]
    fn the_light_leaves_the_corner_runs_the_perimeter_and_comes_home() {
        assert_eq!(
            drawn(0.0, 10, 3),
            ["0         ", " ........ ", "          "].join("\n"),
            "it starts on the corner alone"
        );
        assert_eq!(
            drawn(0.25, 10, 3),
            ["9997531   ", " ........ ", "          "].join("\n"),
            "a quarter in it is along the top with its tail behind it"
        );
        assert_eq!(
            drawn(0.6, 10, 3),
            ["9999999999", " ........9", "    024689"].join("\n"),
            "past halfway it is coming back along the bottom, tail to the right"
        );
        assert_eq!(
            drawn(1.0, 10, 3),
            ["9999999999", "9........9", "9999999999"].join("\n"),
            "and at the end the whole border is the resting hairline"
        );
    }

    /// One path, so a corner is a cell of it like any other and the light is
    /// never in two places.
    #[test]
    fn the_perimeter_is_one_path_with_no_cell_on_it_twice() {
        for (width, height) in [(10, 3), (1, 1), (1, 5), (5, 1), (2, 2), (80, 6)] {
            let path = path(width, height);
            let mut seen = path.clone();
            seen.sort_unstable();
            seen.dedup();
            assert_eq!(seen.len(), path.len(), "{width}x{height}: {path:?}");
            let on =
                |(x, y): &(usize, usize)| *x == 0 || *x + 1 == width || *y == 0 || *y + 1 == height;
            assert!(path.iter().all(on), "{width}x{height}: {path:?}");
            let corners = [
                (0, 0),
                (width - 1, 0),
                (0, height - 1),
                (width - 1, height - 1),
            ];
            for corner in corners {
                assert!(path.contains(&corner), "{width}x{height} misses {corner:?}");
            }
        }
        assert!(path(0, 4).is_empty(), "a box of no width has no border");
        assert!(path(4, 0).is_empty());
    }

    #[test]
    fn the_light_is_one_point_with_one_tail_behind_it() {
        let cells = lap(0.3, 80, 6);
        let lit: Vec<f32> = cells
            .iter()
            .filter_map(|edge| match edge {
                Edge::Lit(age) => Some(*age),
                _ => None,
            })
            .collect();
        let youngest = lit.iter().copied().fold(f32::MAX, f32::min);
        assert!(youngest < 0.05, "the head is the newest light: {youngest}");
        let warm = lit.iter().filter(|age| **age < 1.0).count();
        let perimeter = path(80, 6).len();
        assert!(
            warm > 20 && warm < perimeter / 2,
            "the tail is a share of the perimeter, not all of it: {warm} of {perimeter}"
        );
    }
}

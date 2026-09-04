//! Which of the piece's beats a frame is in, and how far into each of them.
//!
//! Not one beat with a progress but three progresses and a beam, because the
//! beats *overlap*: the first row arrives while the mark is still sparkling.
//! Each is 0 before its own beat begins and 1 once it is over, and every one of
//! them is a pure function of the second the frame is for.

use std::time::Duration;

/// One beat: the second it begins at, and how long it lasts.
type Span = (f32, f32);

/// One frame of the sparkle in seconds — the surface's own rhythm (§6).
const SPARKLE: f32 = crate::theme::SPARKLE_MS as f32 / 1000.0;

/// How long the piece is. The frame at or after it is the welcome box itself.
pub const END: f32 = 2.4;

/// A point of light leaves the top-left corner and runs the perimeter.
const LINE: Span = (0.0, 0.9);
/// The mark ignites where the light came home: one whole turn of the sparkle,
/// so the beat ends on the resting `✻` rather than part way round.
const MARK: Span = (0.9, 4.0 * SPARKLE);
/// The rows arrive under a beam, one after another.
const WORDS: Span = (1.1, 0.9);
/// The border takes one breath — and then rests, which is why the beat ends
/// before the piece does: the border is back at its own hairline a frame or two
/// before the box lands, so the piece does not finish on a step of colour.
const BREATH: Span = (2.0, 0.35);

/// How far apart two rows arrive at most (§11). A box with more rows than the
/// beat has room for closes the gap rather than running past the end of it.
const STEP: f32 = 0.15;

/// How long one row's beam takes to cross the box.
const CROSS: f32 = 0.45;

/// Where `t` stands in each beat of the piece.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Beat {
    /// How far round the perimeter the light has come.
    pub line: f32,
    /// How far the mark's ignition has come, once it has begun — and nothing at
    /// all before, because a share alone cannot tell *not yet* from *just now*
    /// and there is no mark on the screen until the light comes home.
    pub mark: Option<f32>,
    /// How far into its one breath the border is.
    pub breath: f32,
}

pub fn beat(t: f32) -> Beat {
    Beat {
        line: share(t, LINE),
        mark: begun(t, MARK),
        breath: share(t, BREATH),
    }
}

/// How far into the beam that reveals row `which` of the `rows` that say
/// something this frame is. The last of them always lands on the beat's own
/// end, whether they arrive [`STEP`] apart or closer.
pub fn row(t: f32, which: usize, rows: usize) -> f32 {
    let step = STEP.min((WORDS.1 - CROSS) / rows.saturating_sub(1).max(1) as f32);
    share(t, (WORDS.0 + which as f32 * step, CROSS))
}

/// How far into the sparkle's own turn the mark's ignition is: the beat is one
/// whole turn of it, so a `p` of 1 lands back on the resting frame.
pub fn sparkling(p: f32) -> Duration {
    Duration::from_secs_f32(MARK.1 * p.clamp(0.0, 1.0))
}

/// How far `t` is through one beat, and never outside it.
fn share(t: f32, span: Span) -> f32 {
    match span.1 <= 0.0 {
        true => 1.0,
        false => ((t - span.0) / span.1).clamp(0.0, 1.0),
    }
}

/// The same, for a beat that has something to say about not having begun.
fn begun(t: f32, span: Span) -> Option<f32> {
    (t >= span.0).then(|| share(t, span))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table of §11: the second each beat begins and ends at.
    #[test]
    fn the_beats_begin_and_end_where_the_storyboard_says() {
        for (t, line, mark, breath) in [
            (0.0, 0.0, None, 0.0),
            (0.45, 0.5, None, 0.0),
            (0.89, 0.988_888_9, None, 0.0),
            (0.9, 1.0, Some(0.0), 0.0),
            (1.1, 1.0, Some(1.0 / 3.0), 0.0),
            (1.5, 1.0, Some(1.0), 0.0),
            (2.0, 1.0, Some(1.0), 0.0),
            (2.175, 1.0, Some(1.0), 0.5),
            (2.35, 1.0, Some(1.0), 1.0),
            (END, 1.0, Some(1.0), 1.0),
        ] {
            let got = beat(t);
            assert!(
                (got.line - line).abs() < 1e-5,
                "at {t}s the line is {}, not {line}",
                got.line
            );
            assert_eq!(got.mark.is_some(), mark.is_some(), "the mark at {t}s");
            if let (Some(got), Some(want)) = (got.mark, mark) {
                assert!((got - want).abs() < 1e-5, "the mark at {t}s is {got}");
            }
            assert!(
                (got.breath - breath).abs() < 1e-5,
                "at {t}s the breath is {}, not {breath}",
                got.breath
            );
        }
    }

    /// Every beat is inside the piece, and the last of them ends before it does
    /// so the border is at rest when the box lands.
    #[test]
    fn no_beat_runs_past_the_end_of_the_piece() {
        for (name, span) in [("line", LINE), ("mark", MARK), ("words", WORDS)] {
            assert!(span.0 + span.1 <= END, "{name} ends at {}", span.0 + span.1);
        }
        assert!(
            BREATH.0 + BREATH.1 < END,
            "the breath rests before the box lands"
        );
        assert!(
            row(END, 8, 9) == 1.0,
            "and so does the last row of a tall box"
        );
    }

    #[test]
    fn the_piece_is_two_and_four_tenths_of_a_second_long() {
        assert_eq!(END, 2.4);
        let over = beat(9.0);
        assert_eq!(over.line, 1.0);
        assert_eq!(over.breath, 1.0, "and stays over");
    }

    /// Rows arrive [`STEP`] apart, and the last of them always finishes on the
    /// end of the words' own beat rather than past it.
    #[test]
    fn the_rows_arrive_in_order_and_the_last_lands_with_the_beat() {
        for rows in 1..=8usize {
            let last = row(WORDS.0 + WORDS.1, rows - 1, rows);
            assert_eq!(last, 1.0, "the last of {rows} rows is home by the end");
            for which in 1..rows {
                assert!(
                    row(1.4, which - 1, rows) >= row(1.4, which, rows),
                    "row {which} of {rows} arrives after the one above it"
                );
            }
        }
        assert_eq!(row(1.1, 0, 4), 0.0, "the first begins the beat");
        assert_eq!(row(1.25, 1, 4), 0.0, "and the second a step later");
        assert_eq!(row(1.1, 1, 4), 0.0, "which has not begun yet");
    }

    /// The ignition is one turn of the sparkle, so it ends where it began.
    #[test]
    fn the_marks_ignition_is_one_whole_turn_of_the_sparkle() {
        assert_eq!(crate::theme::sparkle(sparkling(0.0)), "✻");
        assert_eq!(crate::theme::sparkle(sparkling(0.5)), "✶");
        assert_eq!(crate::theme::sparkle(sparkling(1.0)), "✻", "and comes home");
        assert_eq!(
            crate::theme::sparkle(sparkling(9.0)),
            "✻",
            "and stays there past the beat"
        );
    }
}

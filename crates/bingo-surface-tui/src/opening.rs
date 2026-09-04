//! The opening: the welcome box drawing itself, in two and four tenths of a
//! second, out of the motions the product already has.
//!
//! `docs/design/tui.md` §11 is the storyboard in words and this is it in code.
//! One point of warm light runs the box's perimeter with a comet tail behind it
//! ([`lap`]), the mark ignites through the sparkle's own frames where the light
//! came home, the three rows arrive under a beam that crosses them left to
//! right, and the border takes one breath and rests ([`beat`], [`frame`]).
//!
//! Nothing on the screen moves but the box's own cells: the piece plays at the
//! box's resting height, so the transcript never reflows and the composer never
//! jumps (§3, *nothing jumps*).
//!
//! The whole of it is a pure function of one number — the second of the piece a
//! frame is for — and it costs microseconds, so it is drawn *in* the draw. A
//! frame that arrives late is skipped rather than played slowly, and the same
//! second is the same picture on a fast machine and a slow one.
//!
//! What is not here: when it plays. That is [`crate::run::opening`]'s.

mod beat;
mod cells;
mod frame;
mod lap;

use std::time::Instant;

use crate::clock::Now;

pub use beat::END;
pub use frame::frame;

/// The frames the beats are reviewed from.
#[cfg(test)]
mod storyboard;

/// The opening as the surface holds it while it plays: the instant it started,
/// and nothing else.
///
/// A frame is a pure function of how long ago that was, so there is nothing to
/// hold in step and nothing to go stale — and the moment the piece has run out
/// the fact is taken away, leaving the box the transcript has always had.
#[derive(Clone, Copy, Debug)]
pub struct Playing(Instant);

impl Playing {
    pub fn from(now: Instant) -> Self {
        Playing(now)
    }

    /// Which second of the piece this frame is for.
    pub fn seconds(&self, now: Now) -> f32 {
        now.since(self.0).as_secs_f32()
    }

    /// Whether the piece has played out.
    pub fn over(&self, now: Now) -> bool {
        self.seconds(now) >= END
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{later, scene};

    #[test]
    fn the_piece_is_over_when_its_seconds_have_run_out() {
        let (_, now) = scene();
        let playing = Playing::from(now.instant);
        assert_eq!(playing.seconds(now), 0.0);
        assert!(!playing.over(now));
        assert!(!playing.over(later(now, 2_300)));
        assert!(playing.over(later(now, 2_400)));
    }
}

//! The clocks a frame is drawn against, and the shapes every motion takes.
//!
//! Two clocks, kept apart on purpose: the surface's own timers are monotonic
//! (`Instant`), while every deadline the kernel states — an interaction's
//! guard, a turn's start — is wall time. A third thing rides with them:
//! whether this run moves at all, so a cue asks the frame it is drawing in
//! rather than the environment (`BINGO_MOTION=off`, design §7).
//!
//! Everything below is a pure function of a duration: time in, and a number
//! between 0 and 1 — or, for the one a person reads, a phrase — out. What a
//! number means in colour is [`crate::theme`]'s, and what it means on the
//! screen is the view's — so every row of §6 is a sample at a named instant
//! rather than something to watch for.

use std::time::{Duration, Instant};

use jiff::Timestamp;

/// One frame of motion: thirty a second (§6).
pub const FRAME: Duration = Duration::from_millis(33);

#[derive(Clone, Copy, Debug)]
pub struct Now {
    pub instant: Instant,
    pub wall: Timestamp,
    /// Whether this run animates at all. Stillness is drawn, not skipped: a
    /// still surface shows every cue's resting frame.
    pub motion: bool,
}

impl Now {
    pub fn real() -> Self {
        Self {
            // The clock the loop sleeps on is the clock it measures with:
            // outside a runtime, and in one, this is the monotonic clock, and
            // under a paused runtime it is the one the timers obey — so a test
            // that stops time stops the animation with it.
            instant: tokio::time::Instant::now().into_std(),
            wall: Timestamp::now(),
            motion: moves(std::env::var("BINGO_MOTION").ok().as_deref()),
        }
    }

    /// How long this frame is past an instant, and never a negative moment.
    pub fn since(&self, started: Instant) -> Duration {
        self.instant.saturating_duration_since(started)
    }

    /// How long this frame is past a deadline the kernel stated.
    pub fn past(&self, when: Timestamp) -> Duration {
        self.wall.duration_since(when).unsigned_abs()
    }

    /// Whether a wall-clock deadline has arrived.
    pub fn reached(&self, when: Timestamp) -> bool {
        self.wall >= when
    }
}

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

/// How long, as a person says it: the largest unit that fits, rounded down.
/// `40s`, `22m`, `3h`, `2d` — the one place a duration is put into words, so
/// two rows that say a span say it the same way.
pub fn span(past: Duration) -> String {
    let seconds = past.as_secs();
    match seconds {
        ..MINUTE => format!("{seconds}s"),
        MINUTE..HOUR => format!("{}m", seconds / MINUTE),
        HOUR..DAY => format!("{}h", seconds / HOUR),
        _ => format!("{}d", seconds / DAY),
    }
}

/// The same span behind now. Coarser at the near end: a row that offers a
/// session to resume has to tell this morning from last week, and a count of
/// seconds there would be read as precision nobody asked for.
pub fn ago(past: Duration) -> String {
    match past.as_secs() < MINUTE {
        true => "just now".to_string(),
        false => format!("{} ago", span(past)),
    }
}

/// `BINGO_MOTION=off` stills everything (design §7). Anything else — unset,
/// empty, `on` — moves.
pub fn moves(motion: Option<&str>) -> bool {
    motion != Some("off")
}

/// An animation that happens once: a tail fading, a notice arriving, a block
/// settling into place. What is animated is the caller's; when it started and
/// how long it takes is this.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Anim {
    pub started: Instant,
    pub len: Duration,
}

impl Anim {
    pub fn new(started: Instant, len: Duration) -> Self {
        Self { started, len }
    }

    /// 0 where it began, 1 once it is over, and never outside those.
    pub fn progress(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.started).as_secs_f32();
        let len = self.len.as_secs_f32();
        if len <= 0.0 {
            return 1.0;
        }
        (elapsed / len).clamp(0.0, 1.0)
    }

    /// Which step of `steps` it is on, from 0 to `steps` — what a reveal
    /// counts in and what a closing layer counts down from.
    pub fn step(&self, now: Instant, steps: u16) -> u16 {
        let at = self.progress(now) * f32::from(steps);
        (at.floor() as u16).min(steps)
    }
}

/// Fast away from where it was, gentle into where it lands: what the
/// transcript's scroll does.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// How wide the crest of a [`sweep`] is, as a share of the run it crosses:
/// wide enough to read as one light passing over, narrow enough that the run
/// is never lit all at once.
const CREST: f32 = 0.35;

/// A light crossing a run of cells from left to right — what a sent line runs
/// along the input box's border, and what a landed answer runs along a tool's
/// name. `t` is how far the sweep has come, from 0 to 1; the answer is how
/// brightly the cell at `column` of `width` is lit under it: 1 at the crest, 0
/// a crest's width away from it, and 0 everywhere at both ends of the sweep —
/// so a run that is swept starts at rest and finishes there.
pub fn sweep(t: f32, column: usize, width: usize) -> f32 {
    if width == 0 {
        return 0.0;
    }
    let at = (column as f32 + 0.5) / width as f32;
    let head = t.clamp(0.0, 1.0) * (1.0 + 2.0 * CREST) - CREST;
    (1.0 - (at - head).abs() / CREST).clamp(0.0, 1.0)
}

/// Slow at both ends: what a breath does.
pub fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match t < 0.5 {
        true => 4.0 * t * t * t,
        false => 1.0 - (-2.0 * t + 2.0).powi(3) / 2.0,
    }
}

// ---- what runs free ------------------------------------------------------
//
// A breath, a sparkle and a pulse have no beginning to count from — they are
// the same breath whenever you look at them — so they take the wall clock's
// own turn of their period. Every part of the surface then reads the same
// phase without anybody holding an origin, and a test names an instant rather
// than a stopwatch.

/// How far into the current turn of `period` this frame is.
pub fn cycle(now: Now, period: Duration) -> Duration {
    let period = i128::try_from(period.as_nanos()).unwrap_or(i128::MAX);
    if period <= 0 {
        return Duration::ZERO;
    }
    let into = now.wall.as_nanosecond().rem_euclid(period);
    Duration::from_nanos(u64::try_from(into).unwrap_or(u64::MAX))
}

/// Where that turn is, between 0 and 1.
pub fn phase(now: Now, period: Duration) -> f32 {
    let secs = period.as_secs_f32();
    if secs <= 0.0 {
        return 0.0;
    }
    cycle(now, period).as_secs_f32() / secs
}

/// A cycle that goes there and comes back, eased at both ends: 0 at the turn,
/// 1 halfway through it, 0 again at the next. A breath that only rose would
/// snap back, and a snap is not a breath.
pub fn breath(now: Now, period: Duration) -> f32 {
    let there_and_back = 1.0 - (2.0 * phase(now, period) - 1.0).abs();
    ease_in_out(there_and_back)
}

/// A cycle that alternates rather than ramps: what pulses at 1 Hz. Everything
/// that wants a person blinks on the same beat, because they count the same
/// periods off the same clock.
pub fn alternating(now: Now, period: Duration) -> bool {
    let period = i128::try_from(period.as_nanos()).unwrap_or(i128::MAX);
    period > 0 && now.wall.as_nanosecond().div_euclid(period) % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_span_is_said_in_the_largest_unit_that_fits() {
        let secs = |n| span(Duration::from_secs(n));
        assert_eq!(secs(0), "0s");
        assert_eq!(secs(59), "59s");
        assert_eq!(secs(60), "1m");
        assert_eq!(secs(90), "1m", "rounded down, never up");
        assert_eq!(secs(22 * 60), "22m");
        assert_eq!(secs(60 * 60), "1h");
        assert_eq!(secs(24 * 60 * 60), "1d");
    }

    #[test]
    fn how_long_ago_is_said_in_the_largest_unit_that_fits() {
        let secs = |n| ago(Duration::from_secs(n));
        assert_eq!(secs(0), "just now");
        assert_eq!(secs(59), "just now");
        assert_eq!(secs(60), "1m ago");
        assert_eq!(secs(90), "1m ago", "rounded down, never up");
        assert_eq!(secs(59 * 60), "59m ago");
        assert_eq!(secs(60 * 60), "1h ago");
        assert_eq!(secs(2 * 60 * 60), "2h ago");
        assert_eq!(secs(24 * 60 * 60 - 1), "23h ago");
        assert_eq!(secs(24 * 60 * 60), "1d ago");
        assert_eq!(secs(45 * 24 * 60 * 60), "45d ago");
    }

    #[test]
    fn only_off_stills_the_surface() {
        assert!(!moves(Some("off")));
        assert!(moves(None));
        assert!(moves(Some("")));
        assert!(moves(Some("on")));
    }

    #[test]
    fn an_animation_runs_from_zero_to_one_and_stops_there() {
        let start = Instant::now();
        let anim = Anim::new(start, ms(180));
        assert_eq!(anim.progress(start), 0.0);
        assert_eq!(anim.progress(start + ms(90)), 0.5);
        assert_eq!(anim.progress(start + ms(180)), 1.0);
        assert_eq!(anim.progress(start + ms(900)), 1.0, "and no further");
        assert_eq!(anim.progress(start - ms(50)), 0.0, "nor before it started");
    }

    #[test]
    fn a_step_counts_the_frames_an_animation_is_drawn_in() {
        let start = Instant::now();
        let anim = Anim::new(start, ms(99));
        let steps: Vec<u16> = (0..5).map(|i| anim.step(start + ms(33 * i), 3)).collect();
        assert_eq!(steps, vec![0, 1, 2, 3, 3]);
    }

    #[test]
    fn the_two_easings_are_the_only_two_and_both_end_where_they_start_from() {
        for ease in [ease_out as fn(f32) -> f32, ease_in_out] {
            assert_eq!(ease(0.0), 0.0);
            assert_eq!(ease(1.0), 1.0);
            assert_eq!(ease(-1.0), 0.0, "clamped");
            assert_eq!(ease(2.0), 1.0, "clamped");
        }
        assert!(ease_out(0.25) > 0.5, "ease-out covers ground early");
        assert!(ease_in_out(0.25) < 0.25, "ease-in-out starts slowly");
        assert_eq!(ease_in_out(0.5), 0.5, "and is symmetric about the middle");
    }

    /// The one brick both new beats stand on: the light that crosses a run.
    #[test]
    fn a_sweep_crosses_its_run_and_leaves_it_at_rest_at_both_ends() {
        let across = |t| (0..10).map(|c| sweep(t, c, 10)).collect::<Vec<f32>>();
        let brightest = |t: f32| {
            let row = across(t);
            row.iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(at, _)| at)
                .unwrap_or_default()
        };
        assert!(
            across(0.0).iter().all(|level| *level == 0.0),
            "nothing is lit before it starts: {:?}",
            across(0.0)
        );
        assert!(
            across(1.0).iter().all(|level| *level == 0.0),
            "nor after it has passed: {:?}",
            across(1.0)
        );
        assert!(
            brightest(0.25) < brightest(0.5),
            "and it goes left to right"
        );
        assert!(brightest(0.5) < brightest(0.75));
        assert_eq!(
            across(0.5).iter().filter(|level| **level > 0.0).count(),
            6,
            "the crest is a band, not the whole run: {:?}",
            across(0.5)
        );
        for t in [-1.0, 0.5, 2.0] {
            assert_eq!(sweep(t, 0, 0), 0.0, "a run of no cells lights nothing");
            assert!((0.0..=1.0).contains(&sweep(t, 3, 4)), "and never outside");
        }
    }

    #[test]
    fn a_breath_rises_and_falls_across_its_period() {
        let (_, now) = crate::test_support::scene();
        let period = ms(1600);
        let sampled: Vec<f32> = [0i64, 400, 800, 1200, 1600]
            .iter()
            .map(|at| breath(crate::test_support::later(now, *at), period))
            .collect();
        assert_eq!(sampled[0], 0.0);
        assert_eq!(sampled[1], 0.5);
        assert_eq!(sampled[2], 1.0, "fullest halfway through");
        assert_eq!(sampled[3], 0.5);
        assert_eq!(sampled[4], 0.0, "and back where it started");
    }

    #[test]
    fn a_pulse_alternates_on_the_boundary_of_its_period() {
        let (_, now) = crate::test_support::scene();
        let at = |ms| alternating(crate::test_support::later(now, ms), ms_1000());
        assert!(!at(0));
        assert!(!at(999));
        assert!(at(1_000));
        assert!(at(1_999));
        assert!(!at(2_000));
    }

    fn ms_1000() -> Duration {
        ms(1_000)
    }

    /// Every free-running cue reads the same clock, so two of them at the
    /// same period are never out of step with one another.
    #[test]
    fn a_cycle_is_the_wall_clocks_own_turn_of_the_period() {
        let (_, now) = crate::test_support::scene();
        let later = crate::test_support::later;
        let period = ms(600);
        let from = cycle(now, period);
        assert!(from < period, "somewhere inside the turn: {from:?}");
        assert_eq!(cycle(later(now, 150), period), from + ms(150));
        assert_eq!(
            cycle(later(now, 600), period),
            from,
            "and it comes back round"
        );
        assert_eq!(phase(later(now, 600), period), phase(now, period));
    }
}

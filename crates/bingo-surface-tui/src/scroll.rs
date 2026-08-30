//! Where the transcript is parked.
//!
//! Two states and no third: it either follows the tail — what arrives is what
//! you see — or it is held at a line and stays there while the transcript
//! grows under it. Holding a line from the top rather than an offset from the
//! bottom is what makes that true without anyone remembering to adjust it.
//!
//! The move to a new line eases out over [`EASE`] as a pure function of the
//! clock, so a test samples it instead of sleeping through it.

use std::time::{Duration, Instant};

/// How long the transcript takes to reach where a key sent it (§6).
pub const EASE: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Scroll {
    /// At the bottom, following what arrives.
    #[default]
    Tail,
    /// Held, easing from one line to another since an instant.
    Held {
        from: usize,
        to: usize,
        since: Instant,
    },
}

impl Scroll {
    /// The first transcript line to draw, for a transcript of `total` lines in
    /// a region `rows` tall.
    pub fn top(&self, total: usize, rows: usize, now: Instant) -> usize {
        let bottom = bottom(total, rows);
        match self {
            Scroll::Tail => bottom,
            Scroll::Held { from, to, since } => eased(*from, *to, *since, now).min(bottom),
        }
    }

    /// Whether what arrives is shown as it arrives — the transcript is at its
    /// foot and the person is writing, not reading back.
    pub fn following(&self) -> bool {
        matches!(self, Scroll::Tail)
    }

    /// Whether the frame after this one would draw it somewhere else.
    pub fn moving(&self, now: Instant) -> bool {
        match self {
            Scroll::Tail => false,
            Scroll::Held { since, .. } => now.saturating_duration_since(*since) < EASE,
        }
    }

    /// Move `lines` towards the top of the transcript, or towards its foot
    /// when negative. Reaching the foot is following it again.
    pub fn by(&mut self, lines: isize, total: usize, rows: usize, now: Instant) {
        let here = self.top(total, rows, now);
        let want = here.saturating_add_signed(-lines);
        self.hold(here, want, total, rows, now);
    }

    /// The top of the transcript.
    pub fn home(&mut self, total: usize, rows: usize, now: Instant) {
        let here = self.top(total, rows, now);
        self.hold(here, 0, total, rows, now);
    }

    /// Bring a line into view, a third of the way down, and hold it there.
    pub fn show(&mut self, line: usize, total: usize, rows: usize, now: Instant) {
        let here = self.top(total, rows, now);
        self.hold(here, line.saturating_sub(rows / 3), total, rows, now);
    }

    /// The foot of it, following again.
    pub fn end(&mut self) {
        *self = Scroll::Tail;
    }

    fn hold(&mut self, from: usize, to: usize, total: usize, rows: usize, now: Instant) {
        let bottom = bottom(total, rows);
        *self = if to >= bottom {
            Scroll::Tail
        } else {
            Scroll::Held {
                from: from.min(bottom),
                to,
                since: now,
            }
        };
    }
}

/// The line the tail starts at: everything above it has scrolled off.
fn bottom(total: usize, rows: usize) -> usize {
    total.saturating_sub(rows)
}

/// Ease-out cubic: fast away from where it was, gentle into where it lands.
fn eased(from: usize, to: usize, since: Instant, now: Instant) -> usize {
    let elapsed = now.saturating_duration_since(since).as_secs_f64();
    let t = elapsed / EASE.as_secs_f64();
    if t >= 1.0 {
        return to;
    }
    let progress = 1.0 - (1.0 - t).powi(3);
    let travelled = (to as f64 - from as f64) * progress;
    from.saturating_add_signed(travelled.round() as isize)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hundred lines of transcript in a twenty-row region.
    const TOTAL: usize = 100;
    const ROWS: usize = 20;

    fn at(scroll: &Scroll, now: Instant) -> usize {
        scroll.top(TOTAL, ROWS, now)
    }

    #[test]
    fn the_tail_is_the_last_screenful_and_follows_what_arrives() {
        let scroll = Scroll::default();
        let now = Instant::now();
        assert_eq!(at(&scroll, now), 80);
        assert_eq!(scroll.top(140, ROWS, now), 120, "the tail moved with it");
        assert_eq!(scroll, Scroll::Tail);
        assert!(!scroll.moving(now));
    }

    #[test]
    fn a_page_back_eases_over_a_tenth_of_a_second() {
        let now = Instant::now();
        let mut scroll = Scroll::default();
        scroll.by(ROWS as isize, TOTAL, ROWS, now);
        let sampled: Vec<usize> = [0u64, 25, 50, 100, 200]
            .iter()
            .map(|ms| at(&scroll, now + Duration::from_millis(*ms)))
            .collect();
        assert_eq!(sampled[0], 80, "it starts where it was");
        assert_eq!(sampled[4], 60, "and lands a page up");
        assert!(
            sampled[1] < sampled[0] && sampled[2] < sampled[1] && sampled[3] == 60,
            "it eases out: {sampled:?}"
        );
        assert!(
            sampled[1] < 70,
            "ease-out covers more than half the way in the first quarter: {sampled:?}"
        );
        assert!(scroll.moving(now + Duration::from_millis(99)));
        assert!(!scroll.moving(now + EASE));
    }

    #[test]
    fn a_held_transcript_keeps_its_line_while_more_arrives() {
        let now = Instant::now();
        let mut scroll = Scroll::default();
        scroll.by(ROWS as isize, TOTAL, ROWS, now);
        let settled = now + EASE;
        assert_eq!(at(&scroll, settled), 60);
        assert_eq!(
            scroll.top(TOTAL + 40, ROWS, settled),
            60,
            "forty lines arrived and the line being read did not move"
        );
        assert_ne!(scroll, Scroll::Tail);
    }

    #[test]
    fn coming_back_to_the_foot_follows_the_tail_again() {
        let now = Instant::now();
        let mut scroll = Scroll::default();
        scroll.by(ROWS as isize, TOTAL, ROWS, now);
        scroll.by(-(ROWS as isize), TOTAL, ROWS, now + EASE);
        assert_eq!(scroll, Scroll::Tail, "a page down at the foot is the tail");

        scroll.by(3, TOTAL, ROWS, now);
        scroll.end();
        assert_eq!(scroll, Scroll::Tail);
    }

    #[test]
    fn home_goes_to_the_first_line_and_stays_there() {
        let now = Instant::now();
        let mut scroll = Scroll::default();
        scroll.home(TOTAL, ROWS, now);
        assert_eq!(at(&scroll, now + EASE), 0);
        assert_eq!(scroll.top(TOTAL * 2, ROWS, now + EASE), 0);
    }

    #[test]
    fn a_transcript_shorter_than_its_region_is_never_scrolled() {
        let now = Instant::now();
        let mut scroll = Scroll::default();
        scroll.by(10, 5, ROWS, now);
        assert_eq!(scroll, Scroll::Tail);
        assert_eq!(scroll.top(5, ROWS, now), 0);
    }

    #[test]
    fn showing_a_line_puts_it_a_third_down_the_region() {
        let now = Instant::now();
        let mut scroll = Scroll::default();
        scroll.show(50, TOTAL, ROWS, now);
        assert_eq!(at(&scroll, now + EASE), 50 - ROWS / 3);
        scroll.show(2, TOTAL, ROWS, now);
        assert_eq!(
            at(&scroll, now + EASE),
            0,
            "a line near the head is the head"
        );
    }

    #[test]
    fn a_move_that_lands_past_the_top_stops_at_it() {
        let now = Instant::now();
        let mut scroll = Scroll::default();
        scroll.by(500, TOTAL, ROWS, now);
        assert_eq!(at(&scroll, now + EASE), 0);
    }
}

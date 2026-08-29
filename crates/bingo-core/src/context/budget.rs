//! The one ruler (ADR-0006): where the lines are for a model, and how far
//! the conversation has gone against the server's own count.

use bingo_sdk::ContextUsage;

/// Estimated tokens of growth, or rounds, after which an exact count is
/// asked for again.
pub const RECOUNT_GROWTH: u64 = 20_000;
pub const RECOUNT_ROUNDS: u32 = 5;

/// Tool results the microcompact leaves intact, normally and on the retry
/// after an overflow.
pub const KEEP_RECENT_RESULTS: usize = 10;
pub const KEEP_RECENT_AFTER_OVERFLOW: usize = 4;
/// A result shorter than this is not worth eliding.
pub const ELIDE_MIN_CHARS: usize = 1_000;

/// The lines for one model, all from the effective window: what is left for
/// input once the output budget is reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    pub effective: u64,
    /// Past this the wire loses stale tool results.
    pub micro: u64,
    /// Past this the person is told once.
    pub warn: u64,
    /// Past this the plugin is asked for a summary.
    pub trigger: u64,
    /// The newest tokens a compaction leaves intact.
    pub keep: u64,
}

impl Thresholds {
    pub fn of(window: u64, max_tokens: u32) -> Self {
        let effective = window.saturating_sub(u64::from(max_tokens));
        let trigger = effective * 9 / 10;
        Self {
            effective,
            micro: effective / 2,
            warn: trigger.saturating_sub(20_000),
            trigger,
            keep: effective / 4,
        }
    }
}

/// Where the estimate was last tied to the truth: what the server counted
/// for a request, and what the estimate said for that same request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Anchor {
    pub server: u64,
    pub estimate: u64,
    /// Rounds since the anchor was exact (a count, not a response).
    pub rounds_since_count: u32,
}

impl Anchor {
    /// The conversation as the server would count it now: its last count,
    /// plus whatever the estimate says was added since.
    pub fn used(&self, estimate: u64) -> u64 {
        self.server + estimate.saturating_sub(self.estimate)
    }

    pub fn recount_due(&self, estimate: u64) -> bool {
        self.rounds_since_count >= RECOUNT_ROUNDS
            || estimate.saturating_sub(self.estimate) >= RECOUNT_GROWTH
    }

    /// A response told us what the request really was.
    pub fn from_response(server: u64, estimate: u64, rounds_since_count: u32) -> Self {
        Self {
            server,
            estimate,
            rounds_since_count,
        }
    }

    /// An exact count for the request about to be sent.
    pub fn from_count(server: u64, estimate: u64) -> Self {
        Self {
            server,
            estimate,
            rounds_since_count: 0,
        }
    }
}

/// One measurement of the request about to be sent.
pub fn usage(anchor: Option<&Anchor>, estimate: u64, lines: &Thresholds) -> ContextUsage {
    ContextUsage {
        used: anchor.map_or(estimate, |a| a.used(estimate)),
        window: lines.effective,
        trigger: lines.trigger,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn the_lines_follow_the_effective_window() {
        let lines = Thresholds::of(50_000, 10_000);
        assert_eq!(lines.effective, 40_000);
        assert_eq!(lines.micro, 20_000);
        assert_eq!(lines.warn, 16_000);
        assert_eq!(lines.trigger, 36_000);
        assert_eq!(lines.keep, 10_000);
    }

    #[test]
    fn a_tiny_window_never_goes_negative() {
        let lines = Thresholds::of(1_000, 10_000);
        assert_eq!(lines.effective, 0);
        assert_eq!(lines.warn, 0);
    }

    #[test]
    fn the_anchor_adds_only_what_grew_since_the_server_counted() {
        let anchor = Anchor::from_response(30_000, 25_000, 1);
        assert_eq!(anchor.used(25_000), 30_000);
        assert_eq!(anchor.used(28_000), 33_000);
        assert_eq!(
            anchor.used(20_000),
            30_000,
            "a shrinking estimate never lowers the count"
        );
        assert!(!anchor.recount_due(28_000));
        assert!(anchor.recount_due(45_000), "twenty thousand of growth");
        assert!(Anchor::from_response(1, 1, RECOUNT_ROUNDS).recount_due(1));
        assert!(!Anchor::from_count(1, 1).recount_due(1));
    }

    proptest! {
        #[test]
        fn used_never_drops_below_the_servers_count(
            server in 0u64..1_000_000, at in 0u64..1_000_000, now in 0u64..1_000_000
        ) {
            let anchor = Anchor::from_response(server, at, 0);
            prop_assert!(anchor.used(now) >= server);
            prop_assert!(anchor.used(now) <= server + now);
        }

        #[test]
        fn the_lines_are_ordered_and_leave_half_the_window(
            window in 1_000u64..2_000_000, max_tokens in 1u32..1_000_000
        ) {
            let lines = Thresholds::of(window, max_tokens);
            prop_assert!(lines.micro <= lines.trigger);
            prop_assert!(lines.warn <= lines.trigger);
            prop_assert!(lines.trigger <= lines.effective);
            prop_assert!(lines.keep <= lines.effective);
        }
    }
}

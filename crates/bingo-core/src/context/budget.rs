//! The one ruler (ADR-0006): where the lines are for a model, and how far
//! the conversation has gone against the server's own count.

use bingo_sdk::ContextUsage;

/// Estimated tokens of growth, or rounds, after which an exact count is
/// asked for again.
pub const RECOUNT_GROWTH: u64 = 20_000;
pub const RECOUNT_ROUNDS: u32 = 5;

/// Tool results the retry leaves intact after an overflow.
pub const KEEP_RECENT_AFTER_OVERFLOW: usize = 4;
/// A result shorter than this is not worth eliding.
pub const ELIDE_MIN_CHARS: usize = 1_000;

/// The lines for one model, all from the effective window: what is left for
/// input once the output budget is reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Thresholds {
    pub effective: u64,
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
            warn: trigger.saturating_sub(20_000),
            trigger,
            keep: effective / 4,
        }
    }

    /// The lines for a context the endpoint holds (ADR-0055 §1): the window
    /// is the endpoint's own and nothing is drawn in it. There is no output
    /// budget to reserve — the kernel sends no request whose size it decides
    /// — nothing to warn at, nothing to cut at, and nothing to keep.
    pub fn held(window: u64) -> Self {
        Self {
            effective: window,
            warn: 0,
            trigger: window,
            keep: 0,
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
        assert_eq!(lines.warn, 16_000);
        assert_eq!(lines.trigger, 36_000);
        assert_eq!(lines.keep, 10_000);
    }

    /// ADR-0055 §1: a held window has no lines in it, so nothing the kernel
    /// draws can fire — and `trigger` says so by being the window itself.
    #[test]
    fn a_held_window_has_no_lines_in_it() {
        let lines = Thresholds::held(1_000_000);
        assert_eq!(lines.effective, 1_000_000);
        assert_eq!(lines.trigger, 1_000_000);
        assert_eq!(lines.warn, 0);
        assert_eq!(lines.keep, 0);
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
        fn the_lines_are_ordered_within_the_effective_window(
            window in 1_000u64..2_000_000, max_tokens in 1u32..1_000_000
        ) {
            let lines = Thresholds::of(window, max_tokens);
            prop_assert!(lines.warn <= lines.trigger);
            prop_assert!(lines.trigger <= lines.effective);
            prop_assert!(lines.keep <= lines.effective);
        }
    }
}

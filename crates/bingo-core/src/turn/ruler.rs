//! A turn's reading of the one ruler (ADR-0006): the lines for its model,
//! where the estimate was last tied to the server's count, and whether the
//! person has been warned this turn.

use bingo_sdk::ContextUsage;

use crate::context::budget::{self, Anchor, Thresholds};

pub struct Ruler {
    pub lines: Thresholds,
    anchor: Option<Anchor>,
    /// The raw estimate of the request being assembled.
    estimate: u64,
    warned: bool,
}

impl Ruler {
    pub fn new(window: u64, max_tokens: u32) -> Self {
        Self {
            lines: Thresholds::of(window, max_tokens),
            anchor: None,
            estimate: 0,
            warned: false,
        }
    }

    /// One measurement of the request being assembled, from its estimate.
    pub fn measure(&mut self, estimate: u64) -> ContextUsage {
        self.estimate = estimate;
        budget::usage(self.anchor.as_ref(), estimate, &self.lines)
    }

    /// The measurement again, after an exact count may have moved the anchor.
    pub fn anchored(&self, usage: ContextUsage) -> ContextUsage {
        ContextUsage {
            used: self.anchor.map_or(usage.used, |a| a.used(self.estimate)),
            ..usage
        }
    }

    pub fn recount_due(&self) -> bool {
        self.anchor.is_none_or(|a| a.recount_due(self.estimate))
    }

    /// The endpoint counted the request about to be sent.
    pub fn counted(&mut self, server: u64) {
        self.anchor = Some(Anchor::from_count(server, self.estimate));
    }

    /// A response said what the request really was.
    pub fn responded(&mut self, server: u64) {
        let rounds = self.anchor.map_or(1, |a| a.rounds_since_count + 1);
        self.anchor = Some(Anchor::from_response(server, self.estimate, rounds));
    }

    /// A compaction changed the conversation under the anchor.
    pub fn forget(&mut self) {
        self.anchor = None;
    }

    /// How many recent tool results the wire keeps whole: everything below
    /// the micro line, fewer on the retry after an overflow.
    pub fn keep_recent(&self, overflowed: bool, usage: &ContextUsage) -> Option<usize> {
        if overflowed {
            Some(budget::KEEP_RECENT_AFTER_OVERFLOW)
        } else if usage.used >= self.lines.micro {
            Some(budget::KEEP_RECENT_RESULTS)
        } else {
            None
        }
    }

    /// The warning the person gets once per turn past the warn line.
    pub fn warning(&mut self, usage: &ContextUsage) -> Option<String> {
        if self.warned || self.lines.warn == 0 || usage.used < self.lines.warn {
            return None;
        }
        self.warned = true;
        Some(format!(
            "context at {}% of the window; a summary replaces the older turns at {}%",
            usage.percent(),
            self.lines.trigger * 100 / self.lines.effective.max(1)
        ))
    }
}

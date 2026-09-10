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
    /// Set where the endpoint holds the context and measures it itself
    /// (ADR-0055 §1): its own last reading, and nothing of the kernel's. The
    /// anchor and the estimate say nothing about a conversation this process
    /// does not have.
    held: Option<u64>,
    warned: bool,
}

impl Ruler {
    /// The kernel's ruler for this model — or, where the endpoint `holds` the
    /// context, the endpoint's own, which draws no line and adds nothing to
    /// what it is told (ADR-0055 §1).
    pub fn new(window: u64, max_tokens: u32, holds: bool) -> Self {
        Self {
            lines: match holds {
                true => Thresholds::held(window),
                false => Thresholds::of(window, max_tokens),
            },
            anchor: None,
            estimate: 0,
            held: holds.then_some(0),
            warned: false,
        }
    }

    /// Whether the context is the endpoint's to measure and to cut.
    pub fn holds(&self) -> bool {
        self.held.is_some()
    }

    /// One measurement of the request being assembled, from its estimate.
    pub fn measure(&mut self, estimate: u64) -> ContextUsage {
        self.estimate = estimate;
        match self.holding() {
            Some(usage) => usage,
            None => budget::usage(self.anchor.as_ref(), estimate, &self.lines),
        }
    }

    /// The measurement again, after an exact count may have moved the anchor.
    pub fn anchored(&self, usage: ContextUsage) -> ContextUsage {
        match self.holding() {
            Some(held) => held,
            None => ContextUsage {
                used: self.anchor.map_or(usage.used, |a| a.used(self.estimate)),
                ..usage
            },
        }
    }

    /// What a held context reads as: the endpoint's own last count, with no
    /// estimate on top of it — what the kernel guessed is not what the
    /// endpoint holds. Zero until the first reading arrives, which is the
    /// truth about a session nothing has said anything about yet.
    fn holding(&self) -> Option<ContextUsage> {
        Some(ContextUsage {
            used: self.held?,
            window: self.lines.effective,
            trigger: self.lines.trigger,
        })
    }

    /// The endpoint said what it is holding (ADR-0055 §2). It replaces the
    /// lines, because the window it counted against is its own, and the
    /// anchor, because a count beats every estimate before it. Only a ruler
    /// that [`holds`](Self::holds) takes one: a reading is what an endpoint
    /// knows about a conversation it keeps.
    pub fn reading(&mut self, used: u64, window: u64) -> ContextUsage {
        self.lines = Thresholds::held(window);
        self.held = Some(used);
        self.anchor = None;
        self.holding().unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR-0055 §1: the kernel's estimate is not a measurement of a
    /// conversation it does not hold, so a held ruler adds none of it.
    #[test]
    fn a_held_ruler_reads_the_endpoints_own_count_and_adds_nothing() {
        let mut ruler = Ruler::new(200_000, 32_000, true);
        assert!(ruler.holds());
        let first = ruler.measure(45_000);
        assert_eq!(first.used, 0, "before a reading, nothing has been said");
        assert_eq!(first.window, 200_000, "and the window is the catalogue's");

        let read = ruler.reading(412_000, 1_000_000);
        assert_eq!(read.used, 412_000);
        assert_eq!(read.window, 1_000_000);
        assert_eq!(read.trigger, 1_000_000, "no line is drawn in it");
        assert_eq!(
            ruler.measure(900_000),
            read,
            "the next round measures what was read, not what it guessed"
        );
    }

    /// The kernel warns before its own compaction; a ruler with no line to
    /// warn about has nothing to say, whatever the reading (ADR-0055 §1).
    #[test]
    fn a_held_ruler_never_warns() {
        let mut ruler = Ruler::new(1_000_000, 32_000, true);
        let usage = ruler.reading(999_999, 1_000_000);
        assert_eq!(ruler.warning(&usage), None);
    }

    /// Nothing moves for a provider that holds nothing (ADR-0055 §5).
    #[test]
    fn a_kernel_ruler_is_as_it_was() {
        let mut ruler = Ruler::new(200_000, 32_000, false);
        assert!(!ruler.holds());
        let usage = ruler.measure(45_000);
        assert_eq!(usage.used, 45_000);
        assert_eq!(usage.window, 168_000);
        assert_eq!(usage.trigger, 151_200);
        ruler.responded(50_000);
        assert_eq!(
            ruler.measure(60_000).used,
            65_000,
            "what the server counted, plus what the estimate grew by since"
        );
        assert!(
            ruler
                .warning(&ContextUsage {
                    used: 140_000,
                    ..usage
                })
                .is_some()
        );
    }
}

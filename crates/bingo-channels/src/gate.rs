//! When a growing answer is worth saying again (ADR-0016 §2).
//!
//! Three reasons, in order of how much a person notices them: the text
//! reached a boundary a reader stops at; enough of it is new to be worth a
//! redraw; or it has simply been a while. The clock is the caller's, so a test
//! decides what "a while" means without waiting for it.

use std::time::Duration;

/// Where a reader stops. The ASCII full stop is deliberately not here: it ends
/// as many file names and decimals as it does sentences.
const BOUNDARIES: [char; 10] = ['\n', '。', '！', '？', '!', '?', '；', ';', '：', ':'];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gate {
    /// New characters that are worth a redraw on their own.
    pub min_chars: usize,
    /// The longest a person waits to see anything new.
    pub interval: Duration,
}

impl Default for Gate {
    fn default() -> Self {
        Self {
            min_chars: 48,
            interval: Duration::from_millis(700),
        }
    }
}

impl Gate {
    /// Whether `full` should replace `sent` now, `since` the last time it did.
    pub fn opens(&self, sent: &str, full: &str, since: Duration) -> bool {
        boundary(full) || self.grown(sent, full) || since >= self.interval
    }

    fn grown(&self, sent: &str, full: &str) -> bool {
        full.chars().count().saturating_sub(sent.chars().count()) >= self.min_chars
    }
}

/// Whether the text now ends where a reader would pause. A trailing space
/// does not undo a boundary — `"wait; "` has arrived as surely as `"wait;"` —
/// but a trailing newline *is* one, so only blanks are looked past.
fn boundary(text: &str) -> bool {
    text.trim_end_matches([' ', '\t'])
        .chars()
        .next_back()
        .is_some_and(|c| BOUNDARIES.contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Duration = Duration::ZERO;

    fn gate() -> Gate {
        Gate {
            min_chars: 10,
            interval: Duration::from_millis(500),
        }
    }

    #[test]
    fn a_sentence_boundary_opens_the_gate_at_once() {
        for ending in ["Done.\n", "好了。", "really?", "listen:", "wait; "] {
            assert!(gate().opens("", ending, NOW), "{ending:?}");
        }
    }

    #[test]
    fn an_ascii_full_stop_is_not_a_boundary() {
        assert!(
            !gate().opens("", "notes.", NOW),
            "a full stop ends file names as often as sentences"
        );
    }

    #[test]
    fn enough_new_characters_open_it_without_a_boundary() {
        assert!(!gate().opens("", "short", NOW));
        assert!(gate().opens("", "0123456789", NOW));
        assert!(
            !gate().opens("0123456789", "0123456789abc", NOW),
            "only what is new counts"
        );
    }

    #[test]
    fn the_timer_opens_it_when_nothing_else_does() {
        assert!(!gate().opens("", "abc", Duration::from_millis(499)));
        assert!(gate().opens("", "abc", Duration::from_millis(500)));
    }
}

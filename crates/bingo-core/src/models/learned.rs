//! Context windows learned from the server's own rejections. The catalogue
//! says what a model should take; an overflow error says what this endpoint
//! actually took, and the server is never out of date about itself. A lesson
//! is keyed `provider/model`, kept for the process, and applied under the
//! declared tier: the user's word outranks the server, the server outranks
//! the catalogue.

use std::collections::HashMap;
use std::sync::Mutex;

use super::declared::key;

/// A window below this is a misparse, not a model; above, a request id read
/// as a number.
pub const MIN_WINDOW: u64 = 8_000;
pub const MAX_WINDOW: u64 = 10_000_000;

#[derive(Debug, Default)]
pub struct Learned {
    map: Mutex<HashMap<String, u64>>,
}

impl Learned {
    /// Keep the smallest window the server has named; returns whether the
    /// lesson changed anything.
    pub fn record(&self, provider: &str, model: &str, window: u64) -> bool {
        if !(MIN_WINDOW..=MAX_WINDOW).contains(&window) {
            return false;
        }
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        let known = map.entry(key(provider, model)).or_insert(u64::MAX);
        if window < *known {
            *known = window;
            true
        } else {
            false
        }
    }

    pub fn window(&self, provider: &str, model: &str) -> Option<u64> {
        self.map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key(provider, model))
            .copied()
    }
}

/// The window an overflow message names. Two shapes state the maximum
/// outright — a number right of a `>` (`190000 + 64000 > 200000`,
/// `211000 tokens > 200000 maximum`) and the OpenAI sentence (`maximum
/// context length is 128000 tokens`). When only the rejected size is named
/// (`your messages resulted in 132450 tokens`), 85 % of it stands in for a
/// ceiling that must lie below it.
pub fn window_from_overflow(message: &str) -> Option<u64> {
    let sane = |window: u64| {
        (MIN_WINDOW..=MAX_WINDOW)
            .contains(&window)
            .then_some(window)
    };
    let stated = message
        .match_indices('>')
        .map(|(i, _)| &message[i + 1..])
        .chain(
            message
                .match_indices("maximum context length is")
                .map(|(i, m)| &message[i + m.len()..]),
        )
        .find_map(|rest| leading_number(rest).and_then(sane));
    stated.or_else(|| {
        message
            .match_indices(" token")
            .filter_map(|(i, _)| trailing_number(&message[..i]))
            .max()
            .and_then(|rejected| sane(rejected.saturating_mul(85) / 100))
    })
}

fn leading_number(text: &str) -> Option<u64> {
    let text = text.trim_start();
    let digits = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    text.get(..digits)?.parse().ok()
}

fn trailing_number(text: &str) -> Option<u64> {
    let digits = text.len() - text.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    text.get(text.len() - digits..)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stated_maximum_shapes_of_both_providers_are_read() {
        assert_eq!(
            window_from_overflow(
                "input length and max_tokens exceed context limit: 190000 + 64000 > 200000"
            ),
            Some(200_000)
        );
        assert_eq!(
            window_from_overflow("prompt is too long: 211000 tokens > 200000 maximum"),
            Some(200_000)
        );
        assert_eq!(
            window_from_overflow(
                "This model's maximum context length is 128000 tokens. However, your messages resulted in 132450 tokens."
            ),
            Some(128_000)
        );
    }

    #[test]
    fn a_rejected_size_alone_stands_in_at_eighty_five_percent() {
        assert_eq!(
            window_from_overflow("your messages resulted in 132450 tokens"),
            Some(132_450 * 85 / 100)
        );
    }

    #[test]
    fn a_message_without_a_number_or_with_an_absurd_one_teaches_nothing() {
        assert_eq!(window_from_overflow("context overflow"), None);
        assert_eq!(window_from_overflow("limit: 1 + 2 > 3"), None);
        assert_eq!(window_from_overflow("req 123456789012 tokens"), None);
    }

    #[test]
    fn a_lesson_only_lowers_the_window_and_stays_within_bounds() {
        let learned = Learned::default();
        assert_eq!(learned.window("p", "m"), None);
        assert!(learned.record("p", "m", 200_000));
        assert!(
            !learned.record("p", "m", 300_000),
            "a larger number is no lesson"
        );
        assert!(learned.record("p", "m", 128_000));
        assert_eq!(learned.window("p", "m"), Some(128_000));
        assert!(!learned.record("p", "m", 100));
        assert!(!learned.record("p", "m", MAX_WINDOW + 1));
        assert_eq!(learned.window("p", "other"), None);
    }
}

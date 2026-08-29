//! Context windows learned from the server's own rejections. The catalogue
//! says what a model should take; an overflow error says what this endpoint
//! actually took, and the server is never out of date about itself. A lesson
//! is keyed `provider/model`, kept for the process, and applied under the
//! declared tier: the user's word outranks the server, the server outranks
//! the catalogue.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::declared::key;

/// A window below this is a misparse, not a model; above, a request id read
/// as a number.
pub const MIN_WINDOW: u64 = 8_000;
pub const MAX_WINDOW: u64 = 10_000_000;

#[derive(Debug, Default)]
pub struct Learned {
    map: Mutex<HashMap<String, u64>>,
    /// Where the lessons outlive the process (ADR-0006); `None` keeps them
    /// in memory, as tests do.
    path: Option<PathBuf>,
}

impl Learned {
    /// The lessons written by earlier processes, from `path`; a file that
    /// is missing or unreadable is an empty start, never an error.
    pub fn load(path: PathBuf) -> Self {
        let map = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            map: Mutex::new(map),
            path: Some(path),
        }
    }

    fn save(&self, map: &HashMap<String, u64>) {
        let Some(path) = &self.path else { return };
        if let Err(e) = write_atomically(path, map) {
            tracing::warn!(path = %path.display(), error = %e, "learned windows not saved");
        }
    }

    /// Keep the smallest window the server has named; returns whether the
    /// lesson changed anything.
    pub fn record(&self, provider: &str, model: &str, window: u64) -> bool {
        if !(MIN_WINDOW..=MAX_WINDOW).contains(&window) {
            return false;
        }
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        let known = map.entry(key(provider, model)).or_insert(u64::MAX);
        if window >= *known {
            return false;
        }
        *known = window;
        self.save(&map);
        true
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

fn write_atomically(path: &Path, map: &HashMap<String, u64>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(map)?)?;
    std::fs::rename(tmp, path)
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
    fn a_lesson_outlives_the_process_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("learned-windows.json");
        let first = Learned::load(path.clone());
        assert!(first.record("p", "m", 128_000));
        let second = Learned::load(path.clone());
        assert_eq!(second.window("p", "m"), Some(128_000));
        assert_eq!(
            Learned::load(dir.path().join("absent.json")).window("p", "m"),
            None
        );
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

//! The wake a model set on this session, as this surface sees it.
//!
//! The schedules plugin owns it; the kernel carries its projection as
//! `extensions["bingo.schedule"]["wake"]` (ADR-0011 §2, ADR-0019 §8) and this
//! reads the one field the status line draws. Nothing here remembers a wake
//! or counts one down: the moment it comes is read at render time and turned
//! into words against the frame's own clock, so the line can never disagree
//! with the store the plugin publishes it from.

use bingo_sdk::SessionState;
use jiff::Timestamp;

/// The plugin whose extension carries the wake, and the kind, by the words it
/// publishes them under. A surface may not import a plugin (ADR-0001), so the
/// two words are the whole of the contract.
const SCHEDULES: &str = "bingo.schedule";
const KIND: &str = "wake";

/// When the wake comes, or nothing where none stands.
pub fn at(state: &SessionState) -> Option<Timestamp> {
    state
        .extensions
        .get(SCHEDULES)?
        .get(KIND)?
        .get("at")?
        .as_str()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    #[test]
    fn the_moment_is_read_from_the_kind_the_plugin_published() {
        let state = folded(vec![frame(1, pending_wake("2026-09-04T12:00:00Z"))]);
        assert_eq!(at(&state), "2026-09-04T12:00:00Z".parse().ok());
    }

    #[test]
    fn no_plugin_no_kind_and_a_kind_taken_back_all_read_as_nothing() {
        assert_eq!(at(&state()), None);
        let other = folded(vec![frame(
            1,
            plugin_view("hooks", serde_json::json!({ "events": 3 })),
        )]);
        assert_eq!(at(&other), None);
        let gone = folded(vec![
            frame(1, pending_wake("2026-09-04T12:00:00Z")),
            frame(2, wake_taken_back()),
        ]);
        assert_eq!(at(&gone), None, "a wake that fired leaves nothing behind");
    }

    #[test]
    fn a_moment_that_is_not_one_is_no_wake_rather_than_a_wrong_one() {
        let state = folded(vec![frame(1, pending_wake("half past four"))]);
        assert_eq!(at(&state), None);
    }
}

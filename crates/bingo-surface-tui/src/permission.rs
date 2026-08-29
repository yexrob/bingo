//! The permission mode as this surface sees it.
//!
//! The policy owns the mode; the kernel publishes its projection as
//! `ConfigView.plugins["permissions"]` (ADR-0009 §5) and this reads the one
//! field it draws. Nothing here remembers a mode: the chord submits
//! `/permission <next>` like any typed line and the badge moves when the
//! `ConfigChanged` frame lands, so the screen can never disagree with the
//! policy.

use bingo_sdk::SessionState;

/// The policy whose view carries the mode, keyed by its plugin id.
const POLICY: &str = "permissions";

/// The modes in the order the chord walks them. The policy owns the list and
/// rejects what it does not know; a mode this surface has never heard of
/// simply does not cycle.
const CYCLE: [&str; 5] = [
    "default",
    "acceptEdits",
    "plan",
    "bypassPermissions",
    "dontAsk",
];

/// What the policy says this session's mode is, or nothing when no policy
/// published one.
pub fn mode(state: &SessionState) -> Option<&str> {
    state.config.plugins.get(POLICY)?.get("mode")?.as_str()
}

/// The mode after this one, wrapping. `None` when it is not one of the five.
pub fn next(mode: &str) -> Option<&'static str> {
    let at = CYCLE.iter().position(|known| *known == mode)?;
    Some(CYCLE[(at + 1) % CYCLE.len()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;

    #[test]
    fn the_mode_is_read_from_the_policys_own_view() {
        let state = folded(vec![frame(1, permission_view("plan"))]);
        assert_eq!(mode(&state), Some("plan"));
    }

    #[test]
    fn no_policy_and_no_mode_field_both_read_as_nothing() {
        assert_eq!(mode(&state()), None);
        let published = folded(vec![frame(
            1,
            plugin_view("hooks", serde_json::json!({ "events": 3 })),
        )]);
        assert_eq!(mode(&published), None);
    }

    #[test]
    fn the_cycle_walks_the_five_and_comes_back() {
        let mut seen = vec!["default"];
        for _ in 0..CYCLE.len() {
            let last = seen.last().copied().expect("a mode to walk from");
            seen.push(next(last).expect("a known mode has a successor"));
        }
        assert_eq!(
            seen,
            [
                "default",
                "acceptEdits",
                "plan",
                "bypassPermissions",
                "dontAsk",
                "default"
            ]
        );
    }

    #[test]
    fn a_mode_this_surface_does_not_know_has_no_successor() {
        assert_eq!(next("acceptedits"), None);
        assert_eq!(next(""), None);
    }
}

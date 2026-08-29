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

/// What the policy says this session's mode is, or nothing when no policy
/// published one.
pub fn mode(state: &SessionState) -> Option<&str> {
    state.config.plugins.get(POLICY)?.get("mode")?.as_str()
}

/// The mode after the current one, wrapping, in the order the policy
/// published (`modes`); it owns the list, this surface only walks it. `None`
/// when there is no view, no list, or the mode is not in it.
pub fn next(state: &SessionState) -> Option<&str> {
    let view = state.config.plugins.get(POLICY)?;
    let current = view.get("mode")?.as_str()?;
    let modes: Vec<&str> = view
        .get("modes")?
        .as_array()?
        .iter()
        .filter_map(|m| m.as_str())
        .collect();
    let at = modes.iter().position(|known| *known == current)?;
    modes.get((at + 1) % modes.len()).copied()
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
    fn the_cycle_walks_the_list_the_policy_published_and_comes_back() {
        let mut seen = vec!["default".to_string()];
        for _ in 0..5 {
            let last = seen.last().cloned().expect("a mode to walk from");
            let state = with_permission_mode(&last);
            seen.push(
                next(&state)
                    .expect("a known mode has a successor")
                    .to_string(),
            );
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
    fn a_mode_outside_the_published_list_has_no_successor() {
        assert_eq!(next(&with_permission_mode("acceptedits")), None);
        assert_eq!(next(&state()), None);
        let unlisted = folded(vec![frame(
            1,
            plugin_view("permissions", serde_json::json!({ "mode": "plan" })),
        )]);
        assert_eq!(next(&unlisted), None, "no list, no cycle");
    }
}

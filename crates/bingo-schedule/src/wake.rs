//! The wake a model sets on its own session (ADR-0019 §8): the bounds it is
//! held to, what one is, and the words a surface reads it by.
//!
//! A wake is not a schedule. A schedule is a file in a store that one
//! process per store runs; a wake is the session's own, held and delivered
//! by the process running that session ([`crate::wakes`]), and it never
//! touches the store. Everything anyone can say about a pending wake is
//! read from the one value this module defines — there is no second record.

use bingo_sdk::{HostHandle, Origin, SessionId};
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

/// The least a wake may be. Anything shorter is a busy loop wearing a
/// schedule's clothes: the turn that set it has barely ended.
pub const WAKE_LEAST: SignedDuration = SignedDuration::from_secs(10);

/// The most. A model that wants tomorrow wants a schedule, and
/// `ScheduleCreate` is where one is written.
pub const WAKE_MOST: SignedDuration = SignedDuration::from_secs(60 * 60);

/// The plugin a pending wake is published under, and the kind (ADR-0011 §2).
/// A surface may not import a plugin (ADR-0001), so these two words are the
/// whole of the contract.
pub const PLUGIN: &str = "bingo.schedule";
pub const KIND: &str = "wake";

/// The surface a woken turn's input carries. Deliberately not `schedule`: a
/// scheduled turn is the machinery reporting in, and this is the model's own
/// words to itself, which a transcript marks rather than hides.
pub const SURFACE: &str = "wake";

/// When the wake comes, as the payload spells it.
pub const AT: &str = "at";
/// What it will say when it does.
pub const NOTE: &str = "note";

/// One wake: when it comes, and what the next turn opens with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wake {
    pub at: Timestamp,
    pub note: String,
}

/// A wake's interval, held inside the bounds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held {
    pub after: SignedDuration,
    /// What was asked for, where the bounds would not have it.
    pub clamped: Option<SignedDuration>,
}

/// `asked`, held between [`WAKE_LEAST`] and [`WAKE_MOST`]. The clamp is
/// carried rather than swallowed: a model that asked for a second is told it
/// got ten, in the same breath as the wake it did get.
pub fn hold(asked: SignedDuration) -> Held {
    let after = asked.clamp(WAKE_LEAST, WAKE_MOST);
    Held {
        after,
        clamped: (after != asked).then_some(asked),
    }
}

/// The wake that comes `after` now and says `note`.
pub fn set(now: Timestamp, after: SignedDuration, note: String) -> Wake {
    Wake {
        // `after` is held to an hour at the most, so this reaches past the
        // end of time only on a clock that is already there; a wake due now
        // is the honest reading of that, and one that never comes is not.
        at: now.checked_add(after).unwrap_or(now),
        note,
    }
}

/// What a surface is told about the wake that stands: when it comes, and what
/// it will say. Derived from the wake every time it is published, so the
/// screen cannot disagree with the plugin; `Null` where nothing is pending,
/// which is how a kind is taken back (ADR-0011 §2).
pub fn payload(pending: Option<&Wake>) -> Value {
    match pending {
        Some(wake) => json!({ AT: wake.at.to_string(), NOTE: wake.note }),
        None => Value::Null,
    }
}

/// Put the pending wake where the person's surface reads it. A session that
/// is gone cannot be told, and there is nobody left to tell.
pub async fn publish(host: &HostHandle, session: &SessionId, pending: Option<&Wake>) {
    if let Err(error) = host.extend(session, PLUGIN, KIND, payload(pending)).await {
        tracing::debug!(%error, "the pending wake was not published");
    }
}

/// Who a woken turn's input is from: an earlier turn of this same
/// conversation, and nobody at the keyboard.
pub fn origin() -> Origin {
    Origin::surface(SURFACE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interval_inside_the_bounds_is_the_one_that_was_asked_for() {
        for seconds in [10, 60, 900, 3600] {
            let asked = SignedDuration::from_secs(seconds);
            assert_eq!(
                hold(asked),
                Held {
                    after: asked,
                    clamped: None
                }
            );
        }
    }

    #[test]
    fn an_interval_outside_them_is_held_and_says_what_was_asked_for() {
        let brief = hold(SignedDuration::from_secs(1));
        assert_eq!(brief.after, WAKE_LEAST);
        assert_eq!(brief.clamped, Some(SignedDuration::from_secs(1)));

        let long = hold(SignedDuration::from_hours(9));
        assert_eq!(long.after, WAKE_MOST);
        assert_eq!(long.clamped, Some(SignedDuration::from_hours(9)));

        let backwards = hold(SignedDuration::from_secs(-30));
        assert_eq!(backwards.after, WAKE_LEAST, "a wake never comes before now");
    }

    #[test]
    fn a_wake_comes_this_long_from_now_and_says_the_note() {
        let wake = set(
            Timestamp::UNIX_EPOCH,
            SignedDuration::from_mins(5),
            "look at the build again".into(),
        );
        assert_eq!(
            wake.at,
            Timestamp::UNIX_EPOCH + SignedDuration::from_mins(5)
        );
        assert_eq!(wake.note, "look at the build again");
    }

    #[test]
    fn what_a_surface_is_told_is_when_it_comes_and_what_it_says() {
        let wake = set(
            Timestamp::UNIX_EPOCH,
            SignedDuration::from_mins(5),
            "look at the build again".into(),
        );
        assert_eq!(
            payload(Some(&wake)),
            json!({ "at": "1970-01-01T00:05:00Z", "note": "look at the build again" })
        );
        assert_eq!(
            payload(None),
            Value::Null,
            "nothing pending takes the kind back"
        );
    }

    #[test]
    fn a_woken_turn_says_it_is_a_wake_and_names_nobody() {
        assert_eq!(origin().surface, SURFACE);
        assert_eq!(origin().principal, None, "nobody is at the keyboard");
        assert_ne!(
            origin().surface,
            crate::runner::SURFACE,
            "a wake is not a scheduled turn: one is read, the other is marked"
        );
    }
}

//! The wake a model sets on its own session (ADR-0019 §8): the bounds it is
//! held to, the entry it becomes, and the words a surface reads it by.
//!
//! A wake is the fourth form of a schedule and not a fourth kind of thing: an
//! entry whose spec is a `once at` and whose `session` names a conversation
//! that already exists. Everything anyone can say about a pending wake is
//! read from that entry — there is no second record of one.

use std::path::Path;

use bingo_sdk::{HostHandle, Origin, SessionId};
use jiff::tz::TimeZone;
use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};

use crate::entry::Entry;
use crate::spec::Spec;
use crate::store::Shelf;

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

/// The wake standing on `session`, if one does. One per session is the rule,
/// and the store is where it is read: a second call finds this one and takes
/// its place.
pub fn pending<'a>(shelf: &'a Shelf, session: &SessionId) -> Option<&'a Entry> {
    shelf
        .entries
        .iter()
        .find(|entry| entry.session.as_ref() == Some(session))
}

/// The entry a wake is: a `once at` this long from now, bound to the session
/// that asked for it. It carries no permission mode — it wakes a session that
/// is already in one.
pub fn entry(
    id: String,
    session: &SessionId,
    cwd: &Path,
    note: String,
    now: Timestamp,
    after: SignedDuration,
) -> Entry {
    Entry {
        id,
        // `after` is held to an hour at the most, so this reaches past the
        // end of time only on a clock that is already there; a wake due now
        // is the honest reading of that, and one that never comes is not.
        spec: Spec::OnceAt(now.checked_add(after).unwrap_or(now)),
        text: note,
        cwd: cwd.to_path_buf(),
        session: Some(session.clone()),
        permission_mode: None,
        enabled: true,
        created: now,
        last_fired: None,
    }
}

/// When a pending wake comes. `None` once it has nothing left to give, which
/// is what a spent or hand-disabled one has.
pub fn at(entry: &Entry) -> Option<Timestamp> {
    Some(entry.next_fire(&TimeZone::UTC)?.timestamp())
}

/// What a surface is told about the wake that stands: when it comes, and what
/// it will say. Derived from the entry every time it is published, so the
/// screen cannot disagree with the store; `Null` where nothing is pending,
/// which is how a kind is taken back (ADR-0011 §2).
pub fn payload(pending: Option<&Entry>) -> Value {
    match pending.and_then(|entry| Some((entry, at(entry)?))) {
        Some((entry, at)) => json!({ AT: at.to_string(), NOTE: entry.text }),
        None => Value::Null,
    }
}

/// Put the pending wake where the person's surface reads it. A session that
/// is gone cannot be told, and there is nobody left to tell.
pub async fn publish(host: &HostHandle, session: &SessionId, pending: Option<&Entry>) {
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
    use crate::entry::tests::entry as schedule;

    fn session() -> SessionId {
        SessionId::from_raw("ses_test")
    }

    fn wake(after: SignedDuration) -> Entry {
        entry(
            "aaaa1111".into(),
            &session(),
            Path::new("/work/project"),
            "look at the build again".into(),
            Timestamp::UNIX_EPOCH,
            after,
        )
    }

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
    fn a_wake_is_a_once_at_bound_to_the_session_that_asked() {
        let wake = wake(SignedDuration::from_mins(5));
        assert!(wake.is_wake());
        assert_eq!(wake.session, Some(session()));
        assert_eq!(wake.spec.to_string(), "once at 1970-01-01T00:05:00Z");
        assert_eq!(
            wake.permission_mode, None,
            "it wakes a session already in one"
        );
        assert_eq!(
            at(&wake),
            Some(Timestamp::UNIX_EPOCH + SignedDuration::from_mins(5))
        );
    }

    #[test]
    fn the_wake_on_a_session_is_the_one_that_names_it() {
        let mine = wake(SignedDuration::from_mins(5));
        let theirs = Entry {
            id: "bbbb2222".into(),
            session: Some(SessionId::from_raw("ses_other")),
            ..mine.clone()
        };
        let shelf = Shelf {
            entries: vec![schedule(), theirs, mine.clone()],
            unreadable: Vec::new(),
        };
        assert_eq!(pending(&shelf, &session()).map(|e| &e.id), Some(&mine.id));
        assert_eq!(
            pending(&shelf, &SessionId::from_raw("ses_nobody")),
            None,
            "a schedule of a person's own is nobody's wake"
        );
    }

    #[test]
    fn what_a_surface_is_told_is_when_it_comes_and_what_it_says() {
        let wake = wake(SignedDuration::from_mins(5));
        assert_eq!(
            payload(Some(&wake)),
            json!({ "at": "1970-01-01T00:05:00Z", "note": "look at the build again" })
        );
        assert_eq!(
            payload(None),
            Value::Null,
            "nothing pending takes the kind back"
        );
        assert_eq!(
            payload(Some(&Entry {
                enabled: false,
                ..wake
            })),
            Value::Null,
            "a wake with nothing left to give is not pending"
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

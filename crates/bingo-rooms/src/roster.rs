//! The rooms this process has seen. Every entry is a fold of a frame the hook
//! observed — a room enters because its own `SessionUpdated` said what it is,
//! and its membership changes because an `Extension` frame said so — so the
//! roster is never a second source for either (ADR-0011 §2).

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};

use bingo_sdk::{SessionId, SessionSummary};
use serde_json::Value;

use crate::room::{self, Room};

#[derive(Debug, Default)]
pub struct Roster(Mutex<BTreeMap<SessionId, Room>>);

impl Roster {
    /// A room announcing itself, at the head of its stream. A reopen says the
    /// same thing again and must not forget what the frames after the first
    /// one folded in.
    pub fn register(&self, summary: &SessionSummary) {
        let Some(room) = Room::of(summary) else {
            return;
        };
        self.rooms().entry(summary.id.clone()).or_insert(room);
    }

    /// The whole of a known room's membership, as its journal now has it.
    pub fn set_members(&self, session: &SessionId, payload: &Value) {
        if let Some(room) = self.rooms().get_mut(session) {
            room.members = room::members_from(payload);
        }
    }

    /// The room a session is, for a caller that is about to await: a copy, so
    /// no lock is held across one.
    pub fn get(&self, session: &SessionId) -> Option<Room> {
        self.rooms().get(session).cloned()
    }

    fn rooms(&self) -> MutexGuard<'_, BTreeMap<SessionId, Room>> {
        self.0.lock().unwrap_or_else(|held| held.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room::payload;
    use crate::tests::{room_summary, summary};

    fn members(names: [&str; 2]) -> Value {
        payload(&names.map(str::to_string))
    }

    #[test]
    fn a_room_enters_when_it_announces_itself_and_a_reopen_changes_nothing() {
        let parent = SessionId::from_raw("ses_root");
        let announced = room_summary("ses_design", &parent, "design");
        let roster = Roster::default();

        roster.register(&announced);
        roster.set_members(&announced.id, &members(["reviewer", "scout"]));
        roster.register(&announced);

        let room = roster.get(&announced.id).expect("still one room");
        assert_eq!(room.title, "#design");
        assert_eq!(room.members, ["reviewer", "scout"]);
        assert_eq!(roster.rooms().len(), 1, "a reopen is the same room");
    }

    #[test]
    fn nothing_that_is_not_a_room_enters_and_nothing_unknown_takes_members() {
        let parent = SessionId::from_raw("ses_root");
        let agent = summary("ses_reviewer", Some("reviewer"), Some(parent));
        let roster = Roster::default();

        roster.register(&agent);
        roster.set_members(&agent.id, &members(["a", "b"]));
        assert_eq!(roster.get(&agent.id), None);
    }
}

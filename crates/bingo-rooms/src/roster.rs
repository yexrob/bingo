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
    /// one folded in, so only a room this process had not seen is handed back
    /// — the moment, and the only one, at which its journal is re-derived.
    pub fn register(&self, summary: &SessionSummary) -> Option<Room> {
        let room = Room::of(summary)?;
        let mut rooms = self.rooms();
        if rooms.contains_key(&summary.id) {
            return None;
        }
        rooms.insert(summary.id.clone(), room.clone());
        Some(room)
    }

    /// The rooms this process has seen under one session: everyone its card
    /// speaks for.
    pub fn under(&self, parent: &SessionId) -> Vec<(SessionId, Room)> {
        self.rooms()
            .iter()
            .filter(|(_, room)| &room.parent == parent)
            .map(|(id, room)| (id.clone(), room.clone()))
            .collect()
    }

    /// One of this plugin's frames in a room's journal: the whole of its
    /// membership, or one seat's own ear. Nothing else it publishes is a
    /// room's business.
    pub fn extended(&self, session: &SessionId, kind: &str, payload: &Value) {
        let mut rooms = self.rooms();
        let Some(room) = rooms.get_mut(session) else {
            return;
        };
        match kind.strip_prefix(room::EAR) {
            Some(member) => room.ears.retune(member, payload),
            None if kind == room::MEMBERS => {
                room.members = room::members_from(payload);
                room.ears.declare(payload);
            }
            None => {}
        }
    }

    /// The rooms of that title this process has seen: where a post held in a
    /// seat's queue says it came from (ADR-0029 §3).
    pub fn titled(&self, title: &str) -> Vec<Room> {
        self.rooms()
            .values()
            .filter(|room| room.title == title)
            .cloned()
            .collect()
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
    use crate::ear::{self, Ear, Seat};
    use crate::room::{MEMBERS, payload};
    use crate::tests::{room_summary, summary};

    fn members(names: [&str; 2]) -> Value {
        payload(&names.map(Seat::live))
    }

    #[test]
    fn a_room_enters_when_it_announces_itself_and_a_reopen_changes_nothing() {
        let parent = SessionId::from_raw("ses_root");
        let announced = room_summary("ses_design", &parent, "design");
        let roster = Roster::default();

        assert!(roster.register(&announced).is_some(), "a room this new");
        roster.extended(&announced.id, MEMBERS, &members(["reviewer", "scout"]));
        assert_eq!(roster.register(&announced), None, "and not again");

        let room = roster.get(&announced.id).expect("still one room");
        assert_eq!(room.title, "#design");
        assert_eq!(room.members, ["reviewer", "scout"]);
        assert_eq!(roster.rooms().len(), 1, "a reopen is the same room");
        assert_eq!(roster.titled("#design"), std::slice::from_ref(&room));
        assert_eq!(roster.under(&parent), [(announced.id, room)]);
        assert!(roster.under(&SessionId::from_raw("ses_other")).is_empty());
        assert!(roster.titled("#standup").is_empty());
    }

    /// The two frames a room's ears come in, folded by the one arm that reads
    /// them: the roster declares, and a seat retunes its own.
    #[test]
    fn a_seat_s_own_ear_is_folded_beside_the_roster_that_declared_it() {
        let parent = SessionId::from_raw("ses_root");
        let announced = room_summary("ses_design", &parent, "design");
        let roster = Roster::default();
        roster.register(&announced);

        roster.extended(&announced.id, MEMBERS, &members(["reviewer", "scout"]));
        roster.extended(
            &announced.id,
            &ear::kind("scout"),
            &ear::register(Ear::Patient(ear::FLOOR)),
        );

        let room = roster.get(&announced.id).expect("the room");
        assert_eq!(room.ears.of("scout"), Ear::Patient(ear::FLOOR));
        assert_eq!(room.ears.of("reviewer"), Ear::Live);
    }

    #[test]
    fn nothing_that_is_not_a_room_enters_and_nothing_unknown_takes_members() {
        let parent = SessionId::from_raw("ses_root");
        let agent = summary("ses_reviewer", Some("reviewer"), Some(parent));
        let roster = Roster::default();

        roster.register(&agent);
        roster.extended(&agent.id, MEMBERS, &members(["a", "b"]));
        assert_eq!(roster.get(&agent.id), None);
    }
}

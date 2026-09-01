//! What a room is, read off the kernel's own facts. A room is a `Log` session
//! (ADR-0011 §1) under a person's, keyed `rooms/…` and titled `#name`; its
//! members are the latest `members` extension published into its journal, and
//! nothing here keeps a copy of either beside them.

use bingo_sdk::{Driver, SessionId, SessionState, SessionSummary};
use serde_json::{Value, json};

/// The one kind this plugin publishes. A payload is the whole of a room's
/// membership (ADR-0011 §2), so writing it replaces it.
pub const MEMBERS: &str = "members";

/// The first segment of a room's key; a store key is `owner/path`, and this
/// plugin owns `rooms`.
pub const KEY: &str = "rooms/";

/// A room, as its own journal says it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Room {
    /// `#design`: the name a member is told a post came from.
    pub title: String,
    /// The session the room hangs under. Its members are that session's other
    /// children, so a room reaches exactly as far as the tree it sits in.
    pub parent: SessionId,
    pub members: Vec<String>,
}

impl Room {
    /// The room a summary announces, or `None` for a session that is not one.
    pub fn of(summary: &SessionSummary) -> Option<Room> {
        if summary.driver != Driver::Log {
            return None;
        }
        if !summary
            .key
            .as_deref()
            .is_some_and(|key| key.starts_with(KEY))
        {
            return None;
        }
        Some(Room {
            title: summary.title.clone()?,
            parent: summary.parent.as_ref()?.session.clone(),
            members: Vec::new(),
        })
    }
}

/// Who is in a room, as the room's own journal has it. Every reader — the
/// command, the fold, a test — comes through here, so none of them can hold a
/// second idea of a membership.
pub fn members_of(state: &SessionState) -> Vec<String> {
    state
        .extensions
        .get(crate::PLUGIN)
        .and_then(|kinds| kinds.get(MEMBERS))
        .map(members_from)
        .unwrap_or_default()
}

/// The names a `members` payload holds. Anything else in it is not a name.
pub fn members_from(payload: &Value) -> Vec<String> {
    payload[MEMBERS]
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A membership as it is published: the whole of it, under one key.
pub fn payload(members: &[String]) -> Value {
    json!({ MEMBERS: members })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{room_summary, summary};

    #[test]
    fn a_log_session_keyed_rooms_under_a_parent_is_a_room() {
        let parent = SessionId::from_raw("ses_root");
        let room = Room::of(&room_summary("ses_design", &parent, "design")).expect("a room");
        assert_eq!(room.title, "#design");
        assert_eq!(room.parent, parent);
        assert!(room.members.is_empty(), "a summary says nothing of them");
    }

    #[test]
    fn every_other_session_is_not_one() {
        let parent = SessionId::from_raw("ses_root");
        let agent = summary("ses_reviewer", Some("reviewer"), Some(parent.clone()));
        assert_eq!(Room::of(&agent), None, "a model answers in it");

        let mut untitled = room_summary("ses_x", &parent, "design");
        untitled.title = None;
        assert_eq!(Room::of(&untitled), None, "nothing to call it");

        let mut foreign = room_summary("ses_y", &parent, "design");
        foreign.key = Some("agent/ses_root/reviewer".into());
        assert_eq!(Room::of(&foreign), None, "another plugin minted it");

        let mut orphan = room_summary("ses_z", &parent, "design");
        orphan.parent = None;
        assert_eq!(Room::of(&orphan), None, "no tree to reach into");
    }

    #[test]
    fn a_membership_round_trips_through_the_payload_it_is_published_as() {
        let members = ["reviewer", "scout"].map(str::to_string).to_vec();
        assert_eq!(members_from(&payload(&members)), members);
        assert!(members_from(&json!({})).is_empty());
        assert_eq!(
            members_from(&json!({ "members": ["reviewer", 7] })),
            ["reviewer"],
            "a name is a string"
        );
    }
}

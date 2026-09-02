//! What a room is, read off the kernel's own facts. A room is a `Log` session
//! (ADR-0011 §1) under a person's, keyed `rooms/…` and titled `#name`; its
//! members are the latest `members` extension published into its journal, and
//! nothing here keeps a copy of either beside them.

use bingo_sdk::{
    Driver, HostHandle, OpenOptions, SessionId, SessionSelector, SessionState, SessionSummary, View,
};
use serde_json::{Value, json};

use crate::ear::{self, Ears, Seat};
use crate::identity;

/// The one kind this plugin publishes for a room as a whole. A payload is the
/// whole of a room's membership (ADR-0011 §2), so writing it replaces it.
pub const MEMBERS: &str = "members";

/// The seats in that payload that are not live, with the patience each asked
/// for (ADR-0029 §2). A payload without it is an all-live roster.
pub const LISTENERS: &str = "listeners";

/// The kind one seat's own retuning is published under, before its name: a
/// register per seat, so no two seats write over each other (ADR-0029 §4).
pub const EAR: &str = "ear:";

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
    /// What each of them hears (ADR-0029 §1).
    pub ears: Ears,
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
            // A summary says who a room is, never who is in it: the frames
            // that follow it do.
            members: Vec::new(),
            ears: Ears::default(),
        })
    }

    /// The same room, with the roster its own journal has. A summary says who
    /// a room is, never who is in it, so a reader that has only just met one
    /// fills it in from the snapshot.
    pub fn seated(&self, state: &SessionState) -> Room {
        Room {
            members: members_of(state),
            ears: ear::ears_of(state),
            ..self.clone()
        }
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

/// The roster a room now has: its names, each wearing the ear its journal
/// gives it.
pub fn roster_of(state: &SessionState) -> Vec<Seat> {
    let ears = ear::ears_of(state);
    members_of(state)
        .into_iter()
        .map(|name| Seat {
            ear: ears.of(&name),
            name,
        })
        .collect()
}

/// A membership as it is published: the whole of it, under one key. A roster
/// of live seats keeps the names it was written with before there were ears,
/// and wears the tree it is drawn as (ADR-0013 §2) in the same payload.
pub fn payload(seats: &[Seat]) -> Value {
    let names: Vec<&str> = seats.iter().map(|seat| seat.name.as_str()).collect();
    let mut payload = drawn(seats);
    payload[MEMBERS] = json!(names);
    if let Some(listeners) = ear::listeners_of(seats) {
        payload[LISTENERS] = listeners;
    }
    payload
}

/// The roster in the vocabulary every surface already draws, so `ctrl+t` and
/// a rail card get a tree rather than the raw object. It rides in the same
/// payload as the names, minted here from the same seats: there is one way to
/// write a roster and it cannot write half of one.
///
/// It is the roster *as declared*. A seat that has retuned its own ear since
/// publishes that under its own `ear:` kind (ADR-0029 §4) — folding it back
/// in would mean rewriting the roster on every retune, which is the race that
/// register was split out to avoid — so what delivery hears is
/// [`ear::ears_of`], and this tree is what the room was seated with.
fn drawn(seats: &[Seat]) -> Value {
    let view = View::Tree {
        nodes: ear::nodes(seats),
    };
    serde_json::to_value(view).unwrap_or_default()
}

/// A session as this plugin reads one: its own journal, folded. A session it
/// cannot read says nothing rather than guessing, which is what every caller
/// here wants of one.
pub async fn read(host: &HostHandle, session: &SessionId) -> Option<SessionState> {
    let opened = host
        .open(
            SessionSelector::ById {
                id: session.clone(),
            },
            identity(),
            OpenOptions::default(),
        )
        .await;
    match opened {
        Ok(attachment) => Some(attachment.snapshot),
        Err(error) => {
            tracing::debug!(%error, %session, "a session that cannot be read says nothing");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ear::Ear;
    use crate::tests::{room_summary, summary};
    use std::time::Duration;

    #[test]
    fn a_log_session_keyed_rooms_under_a_parent_is_a_room() {
        let parent = SessionId::from_raw("ses_root");
        let room = Room::of(&room_summary("ses_design", &parent, "design")).expect("a room");
        assert_eq!(room.title, "#design");
        assert_eq!(room.parent, parent);
        assert!(room.members.is_empty(), "a summary says nothing of them");
        assert_eq!(room.ears, Ears::default(), "nor of what they hear");
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
        let seats = [Seat::live("reviewer"), Seat::live("scout")];
        assert_eq!(members_from(&payload(&seats)), ["reviewer", "scout"]);
        assert!(members_from(&json!({})).is_empty());
        assert_eq!(
            members_from(&json!({ "members": ["reviewer", 7] })),
            ["reviewer"],
            "a name is a string"
        );
    }

    /// The shape a room opened before there were ears left in its journal —
    /// a fixture, because this is a persisted payload and not a value this
    /// process is free to change. Every seat in it hears every post.
    #[test]
    fn a_roster_written_before_there_were_ears_reads_all_live() {
        const OLD: &str = r#"{"members":["reviewer","scout","parent"]}"#;
        let payload: Value = serde_json::from_str(OLD).expect("a membership payload");
        assert_eq!(members_from(&payload), ["reviewer", "scout", "parent"]);

        let mut ears = Ears::default();
        ears.declare(&payload);
        for member in ["reviewer", "scout", "parent"] {
            assert_eq!(ears.of(member), Ear::Live, "{member}");
        }
    }

    /// The whole of what a room's journal is handed, asserted as one value:
    /// the names a reader parses, the listeners' patience beside them and
    /// never instead of them, and the tree a surface draws (ADR-0013 §2).
    #[test]
    fn the_payload_carries_the_names_the_listeners_and_the_tree_it_draws_as() {
        assert_eq!(
            payload(&[Seat::live("scout")]),
            json!({
                "members": ["scout"],
                "kind": "tree",
                "nodes": [{"label": "scout", "tone": "neutral"}],
            })
        );
        let listening = [
            Seat::live("scout"),
            Seat {
                name: "parent".into(),
                ear: Ear::Patient(Duration::from_secs(120)),
            },
        ];
        assert_eq!(
            payload(&listening),
            json!({
                "members": ["scout", "parent"],
                "listeners": [{"name": "parent", "patience_s": 120}],
                "kind": "tree",
                "nodes": [
                    {"label": "scout", "tone": "neutral"},
                    {"label": "listening", "tone": "neutral", "children": [
                        {"label": "parent", "badge": "120s", "tone": "neutral"},
                    ]},
                ],
            })
        );
        assert_eq!(
            payload(&[])["nodes"],
            json!([{"label": "nobody yet", "tone": "neutral"}]),
            "a room nobody is in says so where a person looks"
        );
    }

    /// The tree rides beside the names, so a client that reads either sees the
    /// same roster and neither has to know about the other.
    #[test]
    fn the_payload_is_both_a_membership_and_a_view() {
        let seats = [Seat::live("reviewer"), Seat::live("scout")];
        let payload = payload(&seats);
        assert_eq!(members_from(&payload), ["reviewer", "scout"]);
        let view: View = serde_json::from_value(payload).expect("a view a surface can draw");
        assert_eq!(view.fold(), "reviewer\nscout");
    }
}

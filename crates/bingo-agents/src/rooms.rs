//! What this crate reads out of the rooms plugin's journal. That plugin may
//! not be imported (ADR-0001), so the kinds it publishes and the fields under
//! them are written down here — the whole of the contract, in one place — and
//! every payload is read as data: a shape this does not recognise says nothing
//! rather than guessing at what it meant.

use bingo_sdk::{ItemId, SessionState};
use serde_json::Value;

/// The plugin whose journal these facts are published in.
pub const ROOMS: &str = "bingo.rooms";

/// The kind one seat's cursor is published under, before its name
/// (ADR-0034 §2).
pub const CURSOR: &str = "cursor:";

/// The post a cursor's payload names: the last one that seat has read.
pub const POST: &str = "post";

/// The kind a room that has been closed is published under (ADR-0053 §4).
pub const CLOSED: &str = "closed";

/// Where one member has read this room up to: a register per seat, keyed in
/// one spelling of the name because a room compares names in any case. A seat
/// with no register has read nothing.
pub fn cursor(room: &SessionState, member: &str) -> Option<ItemId> {
    let published = published(room, &format!("{CURSOR}{}", member.to_lowercase()))?;
    Some(ItemId::from_raw(published.get(POST)?.as_str()?))
}

/// Whether the room is closed. The frame is the whole of what closing means
/// (ADR-0053 §4), so the register standing under that kind is the fact itself
/// — and a payload of `null` is nothing standing there.
pub fn is_closed(room: &SessionState) -> bool {
    published(room, CLOSED).is_some_and(|closed| !closed.is_null())
}

/// One of the rooms plugin's registers in this room's own journal.
fn published<'a>(room: &'a SessionState, kind: &str) -> Option<&'a Value> {
    room.extensions.get(ROOMS)?.get(kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::summary;
    use serde_json::json;

    /// A room whose journal holds these registers of the rooms plugin, as the
    /// other crate publishes them.
    fn room(registers: &[(&str, Value)]) -> SessionState {
        let mut room = SessionState::new(summary("ses_room", Some("#design"), None));
        room.extensions.insert(
            ROOMS.into(),
            registers
                .iter()
                .map(|(kind, payload)| (kind.to_string(), payload.clone()))
                .collect(),
        );
        room
    }

    /// The cursor as the rooms plugin publishes it, read back by hand: the
    /// contract between the two crates, written down (ADR-0034 §2).
    #[test]
    fn a_cursor_is_a_post_id_under_the_rooms_plugin_s_own_kind() {
        let empty = SessionState::new(summary("ses_room", Some("#design"), None));
        assert_eq!(cursor(&empty, "scout"), None);

        let read = room(&[("cursor:scout", json!({ "post": "itm_7" }))]);
        assert_eq!(cursor(&read, "scout"), Some(ItemId::from_raw("itm_7")));
        assert_eq!(
            cursor(&read, "Scout"),
            Some(ItemId::from_raw("itm_7")),
            "a room compares names in any case"
        );
        assert_eq!(cursor(&read, "builder"), None, "one seat is not another");
        let unknown = room(&[("cursor:scout", json!({ "read": "itm_7" }))]);
        assert_eq!(
            cursor(&unknown, "scout"),
            None,
            "a payload this does not recognise says nothing"
        );
    }

    /// The `closed` frame as the rooms plugin publishes it (ADR-0053 §4).
    #[test]
    fn a_closed_room_says_when_it_closed_who_closed_it_and_why() {
        let open = room(&[("cursor:scout", json!({ "post": "itm_7" }))]);
        assert!(!is_closed(&open), "a room nobody closed stands");

        let closed = room(&[(
            CLOSED,
            json!({
                "at": "2026-09-10T09:00:00Z",
                "by": "parent",
                "why": "the design is settled"
            }),
        )]);
        assert!(is_closed(&closed));

        let unsaid = room(&[(
            CLOSED,
            json!({ "at": "2026-09-10T09:00:00Z", "by": "parent", "why": null }),
        )]);
        assert!(
            is_closed(&unsaid),
            "a room closes whether or not why is said"
        );
        assert!(
            !is_closed(&room(&[(CLOSED, Value::Null)])),
            "nothing stands under the kind"
        );
    }
}

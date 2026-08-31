//! Fanning a post out. A room answers nobody, so what a post does is reach
//! everyone else in it: every member but its author, found by title among the
//! other children of the session the room hangs under. The room's parent is
//! not one of those, so a person's own session is never written to by their
//! own room.

use bingo_sdk::{
    Delivery, Driver, HostHandle, Input, IntentId, KernelError, Origin, SessionFilter,
    SessionSummary,
};

use crate::SURFACE;
use crate::room::Room;

/// Every member but the author, told who wrote it and where.
pub async fn fan_out(
    host: &HostHandle,
    room: &Room,
    author: &str,
    text: &str,
) -> Result<(), KernelError> {
    let siblings = host
        .sessions(SessionFilter {
            parent: Some(room.parent.clone()),
            ..SessionFilter::default()
        })
        .await?;
    for member in room.audience(author) {
        let Some(target) = seat_of(&siblings, member) else {
            tracing::debug!(room = %room.title, member, "nobody here answers to that name");
            continue;
        };
        let input = Input::text(text, origin(author, &room.title));
        host.deliver(&target.id, IntentId::mint(), input, Delivery::Wake)
            .await?;
    }
    Ok(())
}

/// The session a member name means: a sibling of the room by that title, and
/// never another room — a `Log` session answers nobody, so a post into one
/// would echo rather than arrive. A nudge (ADR-0022 §3) looks a member up
/// through this too, so the two agree on who is there to hear it.
pub(crate) fn seat_of<'a>(
    siblings: &'a [SessionSummary],
    member: &str,
) -> Option<&'a SessionSummary> {
    siblings
        .iter()
        .find(|s| s.driver != Driver::Log && s.title.as_deref() == Some(member))
}

/// Who wrote it, and where. The fold puts `[from <principal> in
/// <conversation>]` above the text (ADR-0011), so a member can tell a room's
/// post from a direct message and knows where to answer.
fn origin(author: &str, room: &str) -> Origin {
    Origin {
        surface: SURFACE.into(),
        principal: Some(author.to_string()),
        conversation: Some(room.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Fleet;
    use bingo_sdk::SessionId;

    /// A root with a room, a reviewer and a scout under it.
    fn tree(members: &[&str]) -> (Fleet, SessionId, Room) {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "reviewer");
        fleet.child(&root, "scout");
        let room = Room {
            title: "#design".into(),
            parent: root.clone(),
            members: members.iter().map(|m| m.to_string()).collect(),
        };
        (fleet, root, room)
    }

    fn said(input: &Input) -> (&str, &Origin) {
        match input {
            Input::Text { text, origin, .. } => (text, origin),
            _ => panic!("a post is text"),
        }
    }

    #[tokio::test]
    async fn everyone_but_the_author_hears_it_and_is_told_where_from() {
        let (fleet, _, room) = tree(&["reviewer", "scout"]);
        fan_out(&fleet.handle(), &room, "reviewer", "look again")
            .await
            .expect("a post this crate can deliver");

        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1, "the author is not written to");
        let (to, input, delivery) = &delivered[0];
        assert_eq!(fleet.summary(to).title.as_deref(), Some("scout"));
        assert_eq!(*delivery, Delivery::Wake);
        let (text, origin) = said(input);
        assert_eq!(text, "look again");
        assert_eq!(origin.surface, SURFACE);
        assert_eq!(origin.principal.as_deref(), Some("reviewer"));
        assert_eq!(origin.conversation.as_deref(), Some("#design"));
    }

    #[tokio::test]
    async fn a_name_nobody_has_is_skipped_and_the_rest_still_hear_it() {
        let (fleet, _, room) = tree(&["reviewer", "nobody"]);
        fan_out(&fleet.handle(), &room, "parent", "stand up")
            .await
            .expect("a post");
        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1);
        assert_eq!(
            fleet.summary(&delivered[0].0).title.as_deref(),
            Some("reviewer")
        );
    }

    #[tokio::test]
    async fn a_room_is_never_a_delivery_target() {
        let (fleet, root, room) = tree(&["#other", "reviewer"]);
        fleet.room(&root, "other");
        fan_out(&fleet.handle(), &room, "parent", "no echo")
            .await
            .expect("a post");
        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        assert_eq!(
            fleet.summary(&delivered[0].0).title.as_deref(),
            Some("reviewer")
        );
    }

    #[tokio::test]
    async fn the_parent_hears_nothing_of_its_own_room() {
        let (fleet, root, room) = tree(&["reviewer"]);
        fan_out(&fleet.handle(), &room, "parent", "hello team")
            .await
            .expect("a post");
        assert!(
            fleet.delivered().iter().all(|(to, ..)| to != &root),
            "a room reaches into the tree, not up out of it"
        );
    }
}

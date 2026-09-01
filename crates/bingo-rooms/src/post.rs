//! Fanning a post out. A room answers nobody, so what a post does is reach
//! everyone else in it: every member but its author, found by title among the
//! other children of the session the room hangs under — and, when the roster
//! names `parent`, that session itself (ADR-0028). A holder off the roster is
//! still not written to: a room reaches into the tree, not up out of it.

use bingo_sdk::{
    Delivery, Driver, HostHandle, Input, IntentId, KernelError, Origin, SessionFilter, SessionId,
    SessionSummary,
};

use crate::SURFACE;
use crate::name::{self, PARENT};
use crate::room::Room;

/// Everyone the post reaches, told who wrote it and where.
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
    let holder = holder_of(host, room).await;
    for (member, delivery) in delivered(room, holder.as_deref(), author) {
        let Some(target) = seat(room, &siblings, member) else {
            tracing::debug!(room = %room.title, member, "nobody here answers to that name");
            continue;
        };
        let input = Input::text(text, origin(author, &room.title));
        host.deliver(&target, IntentId::mint(), input, delivery)
            .await?;
    }
    Ok(())
}

/// Who a post reaches (ADR-0028 §2, amended): every member but its author, and
/// a rostered holder exactly the same. There is no second delivery mode and the
/// text decides nothing — `@parent` owes an answer (ADR-0022), it does not
/// route.
pub(crate) fn delivered<'a>(
    room: &'a Room,
    holder: Option<&str>,
    author: &str,
) -> Vec<(&'a str, Delivery)> {
    room.members
        .iter()
        .filter_map(|member| heard(member, holder, author).map(|how| (member.as_str(), how)))
        .collect()
}

/// How one seat on the roster hears a post, or nothing at all when it is not
/// delivered: nobody is ever handed their own.
fn heard(member: &str, holder: Option<&str>, author: &str) -> Option<Delivery> {
    if !name::is_holder(member) {
        return (member != author).then_some(Delivery::Wake);
    }
    // The holder's guard is the seat's signing name (ADR-0028 §5): a root
    // holder signs `parent` — the person at the composer and the root's model
    // alike — and an agent holder its title, and a seat is one author. R-shadow:
    // a member deliberately titled with that name shadows the holder here and
    // the holder does not hear that post; sibling naming forbids a duplicate
    // beside the room, so the collision costs a deliberate act and is accepted.
    let holder = holder?;
    (!name::same(holder, author)).then_some(Delivery::Wake)
}

/// The name a rostered holder's posts sign, or nothing when the roster does
/// not seat it — which is every room that did not ask for one, and so the host
/// is not read for them at all.
async fn holder_of(host: &HostHandle, room: &Room) -> Option<String> {
    if !room.members.iter().any(|member| name::is_holder(member)) {
        return None;
    }
    let sessions = host.sessions(SessionFilter::default()).await.ok()?;
    let seat = sessions.into_iter().find(|s| s.id == room.parent)?;
    Some(seat.title.unwrap_or_else(|| PARENT.to_string()))
}

/// The session a member name means: the one the room hangs under for `parent`
/// — nothing beside a room is titled that, so the roster's own word is the
/// whole address — and otherwise a sibling of the room by that title. A nudge
/// (ADR-0022 §3) looks a member up through this too, so the two agree on who
/// is there to hear it.
pub(crate) fn seat(room: &Room, siblings: &[SessionSummary], member: &str) -> Option<SessionId> {
    if name::is_holder(member) {
        return Some(room.parent.clone());
    }
    seat_of(siblings, member).map(|summary| summary.id.clone())
}

/// A sibling of the room by title, and never another room — a `Log` session
/// answers nobody, so a post into one would echo rather than arrive.
fn seat_of<'a>(siblings: &'a [SessionSummary], member: &str) -> Option<&'a SessionSummary> {
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

    /// A roster and nothing else: what the decision below is made of.
    fn roster(members: &[&str]) -> Room {
        Room {
            title: "#design".into(),
            parent: SessionId::from_raw("ses_root"),
            members: members.iter().map(|m| m.to_string()).collect(),
        }
    }

    /// The whole of ADR-0028 §2 and §5, as one table. A holder is on the roster
    /// or it is not; when it is, it wakes like anyone else, and the seat's
    /// signing name decides whether it is written to at all. Nothing here reads
    /// the text, because nothing about a delivery depends on it.
    #[test]
    fn every_seat_but_the_author_wakes_and_a_rostered_holder_with_them() {
        let plain = roster(&["reviewer", "scout"]);
        let seated = roster(&["reviewer", "scout", PARENT]);
        let table = [
            (
                &plain,
                Some(PARENT),
                "reviewer",
                vec![("scout", Delivery::Wake)],
            ),
            (
                &plain,
                Some(PARENT),
                PARENT,
                vec![("reviewer", Delivery::Wake), ("scout", Delivery::Wake)],
            ),
            (
                &seated,
                Some(PARENT),
                "reviewer",
                vec![("scout", Delivery::Wake), (PARENT, Delivery::Wake)],
            ),
            (
                &seated,
                Some(PARENT),
                PARENT,
                vec![("reviewer", Delivery::Wake), ("scout", Delivery::Wake)],
            ),
            (
                &seated,
                Some("reviewer"),
                "scout",
                vec![("reviewer", Delivery::Wake), (PARENT, Delivery::Wake)],
            ),
            (
                &seated,
                Some("reviewer"),
                "reviewer",
                vec![("scout", Delivery::Wake)],
            ),
            (&seated, None, "reviewer", vec![("scout", Delivery::Wake)]),
        ];
        for (room, holder, author, expected) in table {
            assert_eq!(
                delivered(room, holder, author),
                expected,
                "{holder:?} holds {:?}, {author} wrote it",
                room.members
            );
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

    /// The roster names the holder, so the holder hears the room — awake, like
    /// every other seat on it.
    #[tokio::test]
    async fn a_rostered_holder_is_woken_by_a_post_like_any_member() {
        let (fleet, root, room) = tree(&["reviewer", PARENT]);
        fan_out(&fleet.handle(), &room, "reviewer", "the build is green")
            .await
            .expect("a post");

        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        let (to, input, delivery) = &delivered[0];
        assert_eq!(to, &root, "the session the room hangs under");
        assert_eq!(*delivery, Delivery::Wake);
        let (text, origin) = said(input);
        assert_eq!(text, "the build is green");
        assert_eq!(origin.principal.as_deref(), Some("reviewer"));
        assert_eq!(origin.conversation.as_deref(), Some("#design"));
    }

    /// ADR-0028 §5: the holder's seat is one author, and a root holder's posts
    /// — the person's own and its model's alike — sign `parent`.
    #[tokio::test]
    async fn a_rostered_holder_never_hears_its_own_post() {
        let (fleet, root, room) = tree(&["reviewer", PARENT]);
        fan_out(&fleet.handle(), &room, PARENT, "hello team")
            .await
            .expect("a post");
        assert!(
            fleet.delivered().iter().all(|(to, ..)| to != &root),
            "the holder was handed its own post: {:?}",
            fleet.delivered()
        );
    }

    /// An agent-held room: the holder signs its title, and that is the guard.
    #[tokio::test]
    async fn an_agent_holder_hears_the_room_but_not_itself() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let reviewer = fleet.child(&root, "reviewer");
        let helper = fleet.child(&reviewer, "helper");
        let room = Room {
            title: "#design".into(),
            parent: reviewer.clone(),
            members: ["helper", PARENT].map(str::to_string).to_vec(),
        };
        let host = fleet.handle();

        fan_out(&host, &room, "helper", "found it")
            .await
            .expect("a post");
        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        assert_eq!(delivered[0].0, reviewer);
        assert_eq!(delivered[0].2, Delivery::Wake);

        fan_out(&host, &room, "reviewer", "look again")
            .await
            .expect("a post");
        assert_eq!(
            fleet.delivered().len(),
            2,
            "only the helper heard the holder's own post"
        );
        assert_eq!(fleet.delivered()[1].0, helper);
    }

    /// Every delivery is one delivery: the holder is counted with the rest
    /// (ADR-0025 §5).
    #[tokio::test]
    async fn each_seat_is_written_to_once_per_post() {
        let (fleet, _, room) = tree(&["reviewer", "scout", PARENT]);
        fan_out(&fleet.handle(), &room, "reviewer", "stand-up")
            .await
            .expect("a post");

        let mut seats: Vec<String> = fleet
            .delivered()
            .iter()
            .map(|(to, ..)| to.to_string())
            .collect();
        let count = seats.len();
        seats.sort();
        seats.dedup();
        assert_eq!(seats.len(), count, "one delivery per seat: {seats:?}");
        assert_eq!(count, 2, "the scout and the holder");
    }
}

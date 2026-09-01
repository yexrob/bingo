//! Fanning a post out. A room answers nobody, so what a post does is reach
//! everyone else in it: every member but its author, found by title among the
//! other children of the session the room hangs under — and, when the roster
//! names `parent`, that session itself (ADR-0028). A holder off the roster is
//! still not written to: a room reaches into the tree, not up out of it.
//!
//! How it reaches them is the seat's own ear (ADR-0029): a live seat is woken
//! by every post, a patient one is handed it held. Obligation pierces both —
//! a post that calls on a seat by name wakes it whatever it wears.

use bingo_sdk::{
    Delivery, Driver, HostHandle, Input, IntentId, KernelError, Origin, SessionFilter, SessionId,
    SessionSummary,
};

use crate::SURFACE;
use crate::mentions::{self, Owed};
use crate::name;
use crate::room::Room;

/// Everyone the post reaches, told who wrote it and where.
pub async fn fan_out(
    host: &HostHandle,
    room: &Room,
    author: &str,
    text: &str,
) -> Result<(), KernelError> {
    let siblings = siblings_of(host, room).await?;
    let holder = holder_of(host, room).await;
    for (member, delivery) in delivered(room, holder.as_deref(), author, text) {
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

/// Who a post reaches (ADR-0028 §2, amended), and how (ADR-0029 §1, §5): every
/// member but its author, a rostered holder exactly the same, each in the ear
/// it wears — unless the post calls on it by name, which wakes any ear.
pub(crate) fn delivered<'a>(
    room: &'a Room,
    holder: Option<&str>,
    author: &str,
    text: &str,
) -> Vec<(&'a str, Delivery)> {
    // Who the post calls on, asked of the mention fold against the room's own
    // roster: the one matcher, so `@all` — which picked no member — pierces
    // nothing, and a name nobody has calls on nobody.
    let called = mentions::named(text, &room.members);
    room.members
        .iter()
        .filter(|member| written_to(member, holder, author))
        .map(|member| (member.as_str(), heard(room, member, &called)))
        .collect()
}

/// Whether a seat is written to at all: nobody is ever handed their own post.
fn written_to(member: &str, holder: Option<&str>, author: &str) -> bool {
    if !name::is_holder(member) {
        return member != author;
    }
    // The holder's guard is the seat's signing name (ADR-0028 §5): a root
    // holder signs `parent` — the person at the composer and the root's model
    // alike — and an agent holder its title, and a seat is one author. R-shadow:
    // a member deliberately titled with that name shadows the holder here and
    // the holder does not hear that post; sibling naming forbids a duplicate
    // beside the room, so the collision costs a deliberate act and is accepted.
    holder.is_some_and(|holder| !name::same(holder, author))
}

/// How one seat hears it: its own ear, unless it was called on — obligation
/// pierces every ear, and the debt it opens is chased as ever, so a patient
/// seat dodges nothing.
fn heard(room: &Room, member: &str, called: &[Owed]) -> Delivery {
    if room.ears.of(member).is_live() || pierced(called, member) {
        return Delivery::Wake;
    }
    Delivery::Hold
}

/// Whether this seat is among the post's mentions.
fn pierced(called: &[Owed], member: &str) -> bool {
    called
        .iter()
        .filter_map(Owed::chased)
        .any(|name| name::same(name, member))
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
    Some(name::signed_by(&seat))
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
    sibling(siblings, member).map(|summary| summary.id.clone())
}

/// The same, for a caller that holds no listing yet: every chaser in this
/// crate looks a member up the way a post does, so they agree on who is there
/// to hear it.
pub(crate) async fn seat_of(host: &HostHandle, room: &Room, member: &str) -> Option<SessionId> {
    seat(room, &siblings_of(host, room).await.ok()?, member)
}

/// A sibling of the room by title, and never another room — a `Log` session
/// answers nobody, so a post into one would echo rather than arrive.
fn sibling<'a>(siblings: &'a [SessionSummary], member: &str) -> Option<&'a SessionSummary> {
    siblings
        .iter()
        .find(|s| s.driver != Driver::Log && s.title.as_deref() == Some(member))
}

/// Everyone in the tree the room sits in: the seats it can reach, and no
/// further.
pub(crate) async fn siblings_of(
    host: &HostHandle,
    room: &Room,
) -> Result<Vec<SessionSummary>, KernelError> {
    host.sessions(SessionFilter {
        parent: Some(room.parent.clone()),
        ..SessionFilter::default()
    })
    .await
}

/// A nudge, into a seat's own queue: a delivery from the room and nobody in it
/// (ADR-0022 §3, ADR-0029 §3). The fold reads it as `[in #design]`, which a
/// post — always signed — never reads as, so nothing here opens a debt or
/// counts toward the serial rule.
pub(crate) async fn nudge(host: &HostHandle, seat: &SessionId, title: &str, said: String) {
    let input = Input::text(
        said,
        Origin {
            surface: SURFACE.into(),
            principal: None,
            conversation: Some(title.to_string()),
        },
    );
    let sent = host
        .deliver(seat, IntentId::mint(), input, Delivery::Wake)
        .await;
    if let Err(error) = sent {
        tracing::debug!(room = %title, %error, "a nudge did not arrive");
    }
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
    use crate::ear::{self, Ear, Ears, Seat};
    use crate::name::PARENT;
    use crate::room;
    use crate::tests::Fleet;

    /// A root with a room, a reviewer and a scout under it.
    fn tree(members: &[&str]) -> (Fleet, SessionId, Room) {
        let fleet = Fleet::default();
        let root = fleet.root();
        fleet.child(&root, "reviewer");
        fleet.child(&root, "scout");
        let room = Room {
            parent: root.clone(),
            ..roster(members)
        };
        (fleet, root, room)
    }

    fn said(input: &Input) -> (&str, &Origin) {
        match input {
            Input::Text { text, origin, .. } => (text, origin),
            _ => panic!("a post is text"),
        }
    }

    /// A roster and nothing else: what the decision below is made of. A name
    /// under the `~` sigil is a patient seat, spelled as a roster spells it.
    fn roster(members: &[&str]) -> Room {
        let seats: Vec<Seat> = members
            .iter()
            .map(|word| Seat::read(word).expect("a roster word"))
            .collect();
        let mut ears = Ears::default();
        ears.declare(&room::payload(&seats));
        Room {
            title: "#design".into(),
            parent: SessionId::from_raw("ses_root"),
            members: seats.into_iter().map(|seat| seat.name).collect(),
            ears,
        }
    }

    /// The whole of ADR-0028 §2 and §5, as one table. A holder is on the roster
    /// or it is not; when it is, it is written to like anyone else, and the
    /// seat's signing name decides whether it is written to at all.
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
                delivered(room, holder, author, "look again"),
                expected,
                "{holder:?} holds {:?}, {author} wrote it",
                room.members
            );
        }
    }

    /// The whole of ADR-0029 §1 and §5, as one table over one roster: a live
    /// seat wakes, a patient one is handed the post held, and a post that
    /// calls on a seat by name wakes it whatever it wears. `@all` picked no
    /// member, so it pierces nobody.
    #[test]
    fn a_patient_seat_holds_what_it_is_not_called_on_by_name() {
        let room = roster(&["reviewer", "~scout", "~parent:120"]);
        let table = [
            (
                "the build is green",
                vec![
                    ("reviewer", Delivery::Wake),
                    ("scout", Delivery::Hold),
                    (PARENT, Delivery::Hold),
                ],
            ),
            (
                "@scout what does the log say?",
                vec![
                    ("reviewer", Delivery::Wake),
                    ("scout", Delivery::Wake),
                    (PARENT, Delivery::Hold),
                ],
            ),
            (
                "@Parent and @scout, both of you",
                vec![
                    ("reviewer", Delivery::Wake),
                    ("scout", Delivery::Wake),
                    (PARENT, Delivery::Wake),
                ],
            ),
            (
                "@all stand-up in five",
                vec![
                    ("reviewer", Delivery::Wake),
                    ("scout", Delivery::Hold),
                    (PARENT, Delivery::Hold),
                ],
            ),
            (
                "mail@scout is an address, and @nobody is nobody",
                vec![
                    ("reviewer", Delivery::Wake),
                    ("scout", Delivery::Hold),
                    (PARENT, Delivery::Hold),
                ],
            ),
        ];
        for (text, expected) in table {
            assert_eq!(
                delivered(&room, Some(PARENT), "author", text),
                expected,
                "{text:?}"
            );
        }
    }

    /// A seat that retuned its own ear is heard as it now hears, and the
    /// roster's declaration is what it retuned from.
    #[test]
    fn a_retuning_decides_the_delivery_over_what_the_roster_declared() {
        let mut room = roster(&["reviewer", "~scout"]);
        room.ears.retune("scout", &ear::register(Ear::Live));
        room.ears
            .retune("reviewer", &ear::register(Ear::Patient(ear::FLOOR)));
        assert_eq!(
            delivered(&room, None, "author", "the build is green"),
            [("reviewer", Delivery::Hold), ("scout", Delivery::Wake)]
        );
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
            parent: reviewer.clone(),
            ..roster(&["helper", PARENT])
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
    /// (ADR-0025 §5), and an ear changes how a seat is written to rather than
    /// how often.
    #[tokio::test]
    async fn each_seat_is_written_to_once_per_post() {
        let (fleet, _, room) = tree(&["reviewer", "~scout", "~parent"]);
        fan_out(&fleet.handle(), &room, "reviewer", "@scout stand-up")
            .await
            .expect("a post");

        let delivered = fleet.delivered();
        let mut seats: Vec<String> = delivered.iter().map(|(to, ..)| to.to_string()).collect();
        let count = seats.len();
        seats.sort();
        seats.dedup();
        assert_eq!(seats.len(), count, "one delivery per seat: {seats:?}");
        assert_eq!(count, 2, "the scout and the holder");
        assert_eq!(
            delivered.iter().map(|(.., how)| *how).collect::<Vec<_>>(),
            [Delivery::Wake, Delivery::Hold],
            "the scout was called on by name; the holder was not"
        );
    }
}

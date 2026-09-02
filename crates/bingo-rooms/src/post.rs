//! What a post does. A post is written once — into the room's own journal —
//! and copied nowhere (ADR-0034 §1): every member reads the room at the head of
//! its next turn, through the cursor it keeps. So the only decision left here
//! is whether a post opens that turn now.
//!
//! A seat is woken when its ear is live, and whatever its ear when the post
//! calls on it by name (ADR-0029 §5). A patient seat the post does not name is
//! left where it is; its own patience wakes it later if it stays behind
//! (`deadline`). The wake itself is a nudge — from the room and nobody in it,
//! carrying no post — so nothing it does opens a debt or counts as read.
//!
//! Who is reachable at all is the tree: every member but the author, found by
//! title among the other children of the session the room hangs under, and —
//! when the roster names `parent` — that session itself (ADR-0028). A holder
//! off the roster is still not written to: a room reaches into the tree, not up
//! out of it.

use bingo_sdk::{
    Delivery, Driver, HostHandle, Input, IntentId, KernelError, Origin, SessionFilter, SessionId,
    SessionSummary,
};

use crate::SURFACE;
use crate::mentions::{self, Owed};
use crate::name;
use crate::room::Room;

/// What a post does to one seat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Heard {
    /// A turn opens on it now, by a nudge.
    Wake,
    /// Nothing happens; the seat reads the room at its next turn, and its
    /// patience is the bound on when that is.
    Wait,
}

/// Wake everyone the post wakes, and hand back the seats it left waiting so
/// their patience can be timed.
pub async fn fan_out<'a>(
    host: &HostHandle,
    room: &'a Room,
    author: &str,
    text: &str,
) -> Result<Vec<&'a str>, KernelError> {
    let siblings = siblings_of(host, room).await?;
    let holder = holder_of(host, room).await;
    let mut waiting = Vec::new();
    for (member, how) in heard(room, holder.as_deref(), author, text) {
        let Some(target) = seat(room, &siblings, member) else {
            tracing::debug!(room = %room.title, member, "nobody here answers to that name");
            continue;
        };
        match how {
            Heard::Wake => nudge(host, &target, &room.title, unread(&room.title)).await,
            Heard::Wait => waiting.push(member),
        }
    }
    Ok(waiting)
}

/// What a post says when it wakes a seat: that there is something to read, and
/// where. The posts themselves are folded into the turn this opens (ADR-0034
/// §4), so the nudge points at them rather than carrying them.
pub(crate) fn unread(room: &str) -> String {
    format!("{room} has posts you have not read. Post in {room} if any of it falls to you.")
}

/// Who a post wakes (ADR-0028 §2, ADR-0029 §1 and §5, ADR-0034 §3): every
/// member but its author, a rostered holder exactly the same, woken if its ear
/// is live — and woken whatever its ear if the post calls on it by name.
pub(crate) fn heard<'a>(
    room: &'a Room,
    holder: Option<&str>,
    author: &str,
    text: &str,
) -> Vec<(&'a str, Heard)> {
    // Who the post calls on, asked of the mention fold against the room's own
    // roster: the one matcher, so `@all` — which picked no member — pierces
    // nothing, and a name nobody has calls on nobody.
    let called = mentions::named(text, &room.members);
    room.members
        .iter()
        .filter(|member| written_to(member, holder, author))
        .map(|member| (member.as_str(), wakes(room, member, &called)))
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

/// Whether one seat wakes for it: its own ear says, unless it was called on —
/// obligation pierces every ear, and the debt it opens is chased as ever, so a
/// patient seat dodges nothing.
fn wakes(room: &Room, member: &str, called: &[Owed]) -> Heard {
    if room.ears.of(member).is_live() || pierced(called, member) {
        return Heard::Wake;
    }
    Heard::Wait
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

    /// A roster and nothing else: what the decision below is made of. A bare
    /// name is a patient seat and `name:0` a live one, spelled as a roster
    /// spells them.
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
    /// or it is not; when it is, it is decided for like anyone else, and the
    /// seat's signing name decides whether it is decided for at all.
    #[test]
    fn every_seat_but_the_author_is_decided_for_and_a_rostered_holder_with_them() {
        let plain = roster(&["reviewer:0", "scout:0"]);
        let seated = roster(&["reviewer:0", "scout:0", "parent:0"]);
        let table = [
            (
                &plain,
                Some(PARENT),
                "reviewer",
                vec![("scout", Heard::Wake)],
            ),
            (
                &plain,
                Some(PARENT),
                PARENT,
                vec![("reviewer", Heard::Wake), ("scout", Heard::Wake)],
            ),
            (
                &seated,
                Some(PARENT),
                "reviewer",
                vec![("scout", Heard::Wake), (PARENT, Heard::Wake)],
            ),
            (
                &seated,
                Some(PARENT),
                PARENT,
                vec![("reviewer", Heard::Wake), ("scout", Heard::Wake)],
            ),
            (
                &seated,
                Some("reviewer"),
                "scout",
                vec![("reviewer", Heard::Wake), (PARENT, Heard::Wake)],
            ),
            (
                &seated,
                Some("reviewer"),
                "reviewer",
                vec![("scout", Heard::Wake)],
            ),
            (&seated, None, "reviewer", vec![("scout", Heard::Wake)]),
        ];
        for (room, holder, author, expected) in table {
            assert_eq!(
                heard(room, holder, author, "look again"),
                expected,
                "{holder:?} holds {:?}, {author} wrote it",
                room.members
            );
        }
    }

    /// The whole of ADR-0029 §1 and §5 under ADR-0034 §3, as one table over one
    /// roster: a live seat wakes, a patient one waits for its own next turn,
    /// and a post that calls on a seat by name wakes it whatever it wears.
    /// `@all` picked no member, so it pierces nobody.
    #[test]
    fn a_patient_seat_waits_for_what_it_is_not_called_on_by_name() {
        let room = roster(&["reviewer:0", "scout", "parent:120"]);
        let table = [
            (
                "the build is green",
                vec![
                    ("reviewer", Heard::Wake),
                    ("scout", Heard::Wait),
                    (PARENT, Heard::Wait),
                ],
            ),
            (
                "@scout what does the log say?",
                vec![
                    ("reviewer", Heard::Wake),
                    ("scout", Heard::Wake),
                    (PARENT, Heard::Wait),
                ],
            ),
            (
                "@Parent and @scout, both of you",
                vec![
                    ("reviewer", Heard::Wake),
                    ("scout", Heard::Wake),
                    (PARENT, Heard::Wake),
                ],
            ),
            (
                "@all stand-up in five",
                vec![
                    ("reviewer", Heard::Wake),
                    ("scout", Heard::Wait),
                    (PARENT, Heard::Wait),
                ],
            ),
            (
                "mail@scout is an address, and @nobody is nobody",
                vec![
                    ("reviewer", Heard::Wake),
                    ("scout", Heard::Wait),
                    (PARENT, Heard::Wait),
                ],
            ),
        ];
        for (text, expected) in table {
            assert_eq!(
                heard(&room, Some(PARENT), "author", text),
                expected,
                "{text:?}"
            );
        }
    }

    /// A seat that retuned its own ear is heard as it now hears, and the
    /// roster's declaration is what it retuned from.
    #[test]
    fn a_retuning_decides_the_wake_over_what_the_roster_declared() {
        let mut room = roster(&["reviewer:0", "scout"]);
        room.ears.retune("scout", &ear::register(Ear::Live));
        room.ears
            .retune("reviewer", &ear::register(Ear::Patient(ear::FLOOR)));
        assert_eq!(
            heard(&room, None, "author", "the build is green"),
            [("reviewer", Heard::Wait), ("scout", Heard::Wake)]
        );
    }

    /// The wake itself (ADR-0034 §3): a nudge from the room and nobody in it,
    /// pointing at what there is to read rather than carrying it.
    #[tokio::test]
    async fn a_woken_seat_is_nudged_and_the_post_itself_is_copied_nowhere() {
        let (fleet, _, room) = tree(&["reviewer:0", "scout:0"]);
        let waiting = fan_out(&fleet.handle(), &room, "reviewer", "look again")
            .await
            .expect("a post this crate can fan out");
        assert!(waiting.is_empty(), "both seats were woken");

        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1, "the author is not written to");
        let (to, input, delivery) = &delivered[0];
        assert_eq!(fleet.summary(to).title.as_deref(), Some("scout"));
        assert_eq!(*delivery, Delivery::Wake);
        let (text, origin) = said(input);
        assert_eq!(text, unread("#design"));
        assert!(!text.contains("look again"), "{text}");
        assert_eq!(origin.surface, SURFACE);
        assert_eq!(origin.principal, None, "a nudge is nobody's post");
        assert_eq!(origin.conversation.as_deref(), Some("#design"));
    }

    /// A patient seat the post did not name is left where it is, and handed
    /// back so its patience can be timed.
    #[tokio::test]
    async fn a_patient_seat_is_left_waiting_and_named_by_the_fan_out() {
        let (fleet, _, room) = tree(&["reviewer:0", "scout"]);
        let waiting = fan_out(&fleet.handle(), &room, "parent", "stand up")
            .await
            .expect("a post");
        assert_eq!(waiting, ["scout"]);
        assert_eq!(fleet.delivered().len(), 1, "only the live seat was woken");
    }

    #[tokio::test]
    async fn a_name_nobody_has_is_skipped_and_the_rest_are_still_woken() {
        let (fleet, _, room) = tree(&["reviewer:0", "nobody:0"]);
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
    async fn a_room_is_never_a_nudge_target() {
        let (fleet, root, room) = tree(&["#other:0", "reviewer:0"]);
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
        let (fleet, root, room) = tree(&["reviewer:0"]);
        fan_out(&fleet.handle(), &room, "parent", "hello team")
            .await
            .expect("a post");
        assert!(
            fleet.delivered().iter().all(|(to, ..)| to != &root),
            "a room reaches into the tree, not up out of it"
        );
    }

    /// The roster names the holder, so the holder is woken by the room — like
    /// every other live seat on it.
    #[tokio::test]
    async fn a_rostered_holder_is_woken_by_a_post_like_any_member() {
        let (fleet, root, room) = tree(&["reviewer:0", "parent:0"]);
        fan_out(&fleet.handle(), &room, "reviewer", "the build is green")
            .await
            .expect("a post");

        let delivered = fleet.delivered();
        assert_eq!(delivered.len(), 1, "{delivered:?}");
        let (to, input, delivery) = &delivered[0];
        assert_eq!(to, &root, "the session the room hangs under");
        assert_eq!(*delivery, Delivery::Wake);
        let (text, origin) = said(input);
        assert_eq!(text, unread("#design"));
        assert_eq!(origin.principal, None);
        assert_eq!(origin.conversation.as_deref(), Some("#design"));
    }

    /// ADR-0028 §5: the holder's seat is one author, and a root holder's posts
    /// — the person's own and its model's alike — sign `parent`.
    #[tokio::test]
    async fn a_rostered_holder_is_never_woken_for_its_own_post() {
        let (fleet, root, room) = tree(&["reviewer:0", "parent:0"]);
        fan_out(&fleet.handle(), &room, PARENT, "hello team")
            .await
            .expect("a post");
        assert!(
            fleet.delivered().iter().all(|(to, ..)| to != &root),
            "the holder was woken for its own post: {:?}",
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
            ..roster(&["helper:0", "parent:0"])
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

    /// Every wake is one wake: the holder is counted with the rest (ADR-0025
    /// §5), and an ear decides whether a seat is woken rather than how often.
    #[tokio::test]
    async fn each_seat_is_woken_at_most_once_per_post() {
        let (fleet, _, room) = tree(&["reviewer:0", "scout", PARENT]);
        let waiting = fan_out(&fleet.handle(), &room, "reviewer", "@scout stand-up")
            .await
            .expect("a post");

        let delivered = fleet.delivered();
        let mut seats: Vec<String> = delivered.iter().map(|(to, ..)| to.to_string()).collect();
        let count = seats.len();
        seats.sort();
        seats.dedup();
        assert_eq!(seats.len(), count, "one nudge per seat: {seats:?}");
        assert_eq!(count, 1, "the scout, called on by name");
        assert_eq!(waiting, [PARENT], "the holder was not");
    }
}

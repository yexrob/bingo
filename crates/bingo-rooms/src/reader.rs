//! Reading a room (ADR-0034 §4). A post is never copied into a member, so a
//! member reads its rooms here and nowhere else: at the head of every round,
//! everything each of its rooms said after its cursor is folded into one piece
//! and the cursor moves to the head in the same step. What a seat has read is
//! exactly what its cursor says, and the reading is journaled once, in the
//! member's own voice, rather than post by post in the room's.
//!
//! The rooms a session sits in are the tree's to say: the rooms beside it that
//! name its title, and — for a holder on its own room's roster (ADR-0028) —
//! the rooms under it that name `parent`. A session in none of them reads one
//! listing and stops.
//!
//! Order: whatever opened the turn is already in the journal (a held briefing
//! first of all, ADR-0027 §2), and this piece follows it, because a round-start
//! contributor speaks after the inputs the turn absorbed.
//!
//! The protocol itself is said here too, once, ahead of the first reading: a
//! seat learns what a room is at the moment it has one, and a session that
//! never sits in one is never told. Nowhere else may say it — a rule stated
//! twice is a rule somebody has to remember to change twice.

use async_trait::async_trait;
use bingo_sdk::{
    CONTRIBUTOR_PREFIX, ContentPart, ContextContributor, ContextError, ContextPiece, ContextQuery,
    Item, ItemBody, Placement, SessionId, SessionState, SessionSummary,
};

use crate::cursor::{self, Unread};
use crate::mentions::Post;
use crate::name::{self, PARENT};
use crate::room::{self, Room};

/// The name this contributor's pieces are journaled under.
const ID: &str = "rooms";

/// What being in a room means, in the words a member acts on: how it reaches
/// you, what opens a turn for you, and what you owe for a post that calls on
/// you (ADR-0028 §2, ADR-0029, ADR-0034 §3–4).
const PROTOCOL: &str = "\
# Rooms

You are seated in a room: a conversation every member reads. It is read, not \
delivered — at the head of each of your turns you are handed everything each of \
your rooms has said since you last read it, under `[#<room>, since you last \
read]`, and nothing of it reaches you between turns.

A turn opens for you when a post says `@<your name>`, when it says `@all` — \
which calls on every member but the one who wrote it — and once your patience \
runs out with something unread: 300 seconds, unless your seat was given \
another. `Listen` retunes your own seat and nobody else's.

Being called on is owed an answer: post it back to the room with \
`SendMessage(to: \"#<room>\")` so whoever is next can carry it on, and say \
`@<name>` when it falls to someone in particular. When what a post names is not \
yours, end your turn without posting rather than answering for someone else.

A room is opened for one purpose, said at the head of its reading; when the \
work moves on, open another room rather than reseating this one.";

/// What a member reads of its rooms, at the head of its own turn.
#[derive(Debug, Default, Clone, Copy)]
pub struct Reader;

#[async_trait]
impl ContextContributor for Reader {
    fn id(&self) -> &str {
        ID
    }

    fn placement(&self) -> Placement {
        Placement::RoundStart
    }

    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        let seats = seated_in(&query).await;
        let mut pieces = Vec::from_iter(protocol(&seats, query.items));
        for seat in seats {
            if let Some(piece) = read(&query, &seat).await {
                pieces.push(piece);
            }
        }
        Ok(pieces)
    }
}

/// The protocol, for a session that has a seat and has not been handed it.
/// What it has already been told is in its own journal, so nothing beside the
/// journal remembers, and a compaction that dropped the piece says it again.
fn protocol(seats: &[Seated], items: &[Item]) -> Option<ContextPiece> {
    let unsaid = !seats.is_empty() && !items.iter().any(spoken_here);
    unsaid.then(|| ContextPiece::User {
        parts: vec![ContentPart::text(PROTOCOL)],
        label: ID.to_string(),
    })
}

/// Whether an item is one this contributor put in the journal.
fn spoken_here(item: &Item) -> bool {
    let ItemBody::User { origin, .. } = &item.body else {
        return false;
    };
    origin.surface.strip_prefix(CONTRIBUTOR_PREFIX) == Some(ID)
}

/// One room this session sits in, as its own journal has it: which session it
/// is, what it is called and what for, the name it seats this session under,
/// and the snapshot every one of those answers was read from.
struct Seated {
    id: SessionId,
    title: String,
    purpose: Option<String>,
    member: String,
    state: SessionState,
}

/// One room, read: everything it said after this seat's cursor, and the cursor
/// moved to the head of it. A seat level with its room reads nothing and says
/// nothing.
async fn read(query: &ContextQuery<'_>, seat: &Seated) -> Option<ContextPiece> {
    let unread = Unread::of(&seat.state, &seat.member);
    let head = unread.head.as_ref()?;
    if let Err(error) = cursor::advance(query.host, &seat.id, &seat.member, head).await {
        tracing::debug!(room = %seat.title, %error, "a seat's cursor did not move");
    }
    let text = said(&seat.title, seat.purpose.as_deref(), &unread.posts)?;
    Some(ContextPiece::User {
        parts: vec![ContentPart::text(text)],
        label: seat.title.clone(),
    })
}

/// The posts as the member reads them: the room, what it is for and the
/// reading above them, then one line per post under the name that wrote it.
fn said(title: &str, purpose: Option<&str>, posts: &[Post]) -> Option<String> {
    if posts.is_empty() {
        return None;
    }
    let lines: Vec<String> = posts
        .iter()
        .map(|post| format!("{}: {}", post.author, post.text.trim()))
        .collect();
    Some(format!("{}\n{}", header(title, purpose), lines.join("\n")))
}

/// The line above a reading. A room carries its one purpose here, where every
/// member reads it every time (ADR-0053 §1); a room opened without one reads as
/// it always did.
fn header(title: &str, purpose: Option<&str>) -> String {
    match purpose {
        Some(purpose) => format!("[{title} — {purpose}, since you last read]"),
        None => format!("[{title}, since you last read]"),
    }
}

/// The rooms this session sits in, each with the name it is seated under. A
/// room is read only if its roster names this seat: a room beside a session it
/// never seated reaches it not at all. Only rooms are opened here — a `Log`
/// session answers nobody, so reading one takes nothing away from it, which is
/// not true of the seats themselves.
async fn seated_in(query: &ContextQuery<'_>) -> Vec<Seated> {
    let mut seated = Vec::new();
    for (id, room) in rooms_around(query).await {
        let Some(state) = room::read(query.host, &id).await else {
            continue;
        };
        let room = room.seated(&state);
        let called = seated_as(query.session, &room);
        let Some(member) = room.members.iter().find(|m| name::same(m, &called)) else {
            continue;
        };
        if spent(&room, &state, member) {
            continue;
        }
        seated.push(Seated {
            id,
            title: room.title.clone(),
            purpose: room.purpose.clone(),
            member: member.clone(),
            state,
        });
    }
    seated
}

/// Whether a room has nothing left for this seat, ever: it has closed and the
/// seat has read it to its end (ADR-0053 §4). Such a room is not a seat at all
/// any more — it costs no reading and no protocol.
fn spent(room: &Room, state: &SessionState, member: &str) -> bool {
    room.closed && Unread::of(state, member).is_empty()
}

/// The name a room's roster would call this session: `parent` for the session
/// the room hangs under, and its own title for a member beside it.
fn seated_as(session: &SessionSummary, room: &Room) -> String {
    match session.id == room.parent {
        true => PARENT.to_string(),
        false => session.title.clone().unwrap_or_default(),
    }
}

/// Every room this session could be seated in: the ones beside it, and the
/// ones hanging under it — nothing further, because a room reaches exactly as
/// far as the tree it sits in.
async fn rooms_around(query: &ContextQuery<'_>) -> Vec<(SessionId, Room)> {
    let mut around = under(query, &query.session.id).await;
    if let Some(parent) = query.session.parent.as_ref() {
        around.extend(under(query, &parent.session).await);
    }
    around
}

async fn under(query: &ContextQuery<'_>, parent: &SessionId) -> Vec<(SessionId, Room)> {
    room::under(query.host, parent).await.unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ear::Seat;
    use crate::seat;
    use crate::tests::{Fleet, item, ts};
    use bingo_sdk::{
        ContextUsage, HostHandle, ItemId, ModelCapabilities, Origin, SessionState, TurnId,
    };
    use std::path::Path;

    /// The turn's own facts, and the journal the session opens the round with
    /// — which is all a reading reads of it: whether the protocol was said.
    struct Turn {
        turn: TurnId,
        usage: ContextUsage,
        capabilities: ModelCapabilities,
        items: Vec<Item>,
    }

    impl Default for Turn {
        fn default() -> Self {
            Turn {
                turn: TurnId::from_raw("trn_1"),
                usage: ContextUsage::default(),
                capabilities: ModelCapabilities {
                    context_window: 100_000,
                    max_output: 1_000,
                    images: false,
                    reasoning: false,
                    count_tokens: false,
                    caching: false,
                    holds_context: false,
                },
                items: Vec::new(),
            }
        }
    }

    impl Turn {
        /// A session that has already been handed the protocol, which is
        /// every turn but a seat's first.
        fn told() -> Turn {
            Turn {
                items: vec![item(ItemBody::User {
                    parts: vec![ContentPart::text(PROTOCOL)],
                    origin: Origin::surface(format!("{CONTRIBUTOR_PREFIX}{ID}")),
                })],
                ..Turn::default()
            }
        }

        fn query<'a>(
            &'a self,
            session: &'a SessionSummary,
            host: &'a HostHandle,
        ) -> ContextQuery<'a> {
            ContextQuery {
                session,
                host,
                turn: &self.turn,
                round: 0,
                items: &self.items,
                usage: &self.usage,
                capabilities: &self.capabilities,
                cwd: Path::new("/work/project"),
            }
        }
    }

    /// A root with a scout under it, and a room seating them both.
    async fn tree(members: &[&str]) -> (Fleet, SessionId, SessionId, SessionId) {
        opened_for(None, members).await
    }

    /// The same, with the purpose the room was opened for.
    async fn opened_for(
        purpose: Option<&str>,
        members: &[&str],
    ) -> (Fleet, SessionId, SessionId, SessionId) {
        let fleet = Fleet::default();
        let root = fleet.root();
        let scout = fleet.child(&root, "scout");
        let seats: Vec<Seat> = members
            .iter()
            .map(|word| Seat::read(word).expect("a roster word"))
            .collect();
        let room = seated(&fleet, &root, purpose, &seats).await;
        (fleet, root, scout, room)
    }

    /// `#design` under this session, opened or reseated as a person's door
    /// does it.
    async fn seated(
        fleet: &Fleet,
        parent: &SessionId,
        purpose: Option<&str>,
        seats: &[Seat],
    ) -> SessionId {
        seat::seat(
            &fleet.handle(),
            parent,
            Path::new("/work/project"),
            seat::Opening::person("design", purpose),
            seats,
        )
        .await
        .expect("a room this crate can open")
    }

    /// What one session's turn would be handed at its head, for a seat the
    /// protocol has already reached — every turn but its first.
    async fn read_by(fleet: &Fleet, session: &SessionId) -> Vec<String> {
        handed(fleet, session, &Turn::told()).await
    }

    /// The same, for a seat that has never been handed anything.
    async fn first_read_by(fleet: &Fleet, session: &SessionId) -> Vec<String> {
        handed(fleet, session, &Turn::default()).await
    }

    async fn handed(fleet: &Fleet, session: &SessionId, turn: &Turn) -> Vec<String> {
        let summary = fleet.summary(session);
        let host = fleet.handle();
        let pieces = Reader
            .contribute(turn.query(&summary, &host))
            .await
            .expect("a reading this crate can make");
        pieces
            .into_iter()
            .map(|piece| match piece {
                ContextPiece::User { parts, .. } => {
                    parts.iter().filter_map(ContentPart::as_text).collect()
                }
                ContextPiece::System(_) => panic!("a room is read as the member's own turn"),
            })
            .collect()
    }

    /// Where one member has read this room up to, as the room's journal says.
    fn cursor_of(fleet: &Fleet, room: &SessionId, member: &str) -> Option<ItemId> {
        cursor::of_state(&fleet.state(room), member)
    }

    /// The last thing said in a room, which is where a seat level with it is.
    fn head_of(fleet: &Fleet, room: &SessionId) -> Option<ItemId> {
        fleet.state(room).items.last().map(|item| item.id.clone())
    }

    /// The whole of ADR-0034 §4: everything since the cursor, under one label,
    /// and the cursor at the head afterwards.
    #[tokio::test]
    async fn a_member_reads_what_the_room_said_since_its_cursor_and_no_more() {
        let (fleet, _, scout, room) = tree(&["scout", "reviewer"]).await;
        fleet.post(&room, "the build is green", Some("reviewer"), ts());
        fleet.post(&room, "and the tests pass", Some("reviewer"), ts());

        assert_eq!(
            read_by(&fleet, &scout).await,
            [
                "[#design, since you last read]\nreviewer: the build is green\nreviewer: and the tests pass"
            ],
            "one piece, both posts, under the room's own label"
        );
        assert!(
            read_by(&fleet, &scout).await.is_empty(),
            "a seat level with its room reads nothing twice"
        );

        fleet.post(&room, "shipping now", Some("reviewer"), ts());
        assert_eq!(
            read_by(&fleet, &scout).await,
            ["[#design, since you last read]\nreviewer: shipping now"],
            "and only what landed after it"
        );
    }

    /// A post nobody signed came from the session the room hangs under, and a
    /// seat is never handed its own post back.
    #[tokio::test]
    async fn the_holder_s_post_is_read_by_its_name_and_a_seat_skips_its_own() {
        let (fleet, _, scout, room) = tree(&["scout", "parent"]).await;
        fleet.post(&room, "stand-up in five", None, ts());
        fleet.post(&room, "on my way", Some("scout"), ts());

        assert_eq!(
            read_by(&fleet, &scout).await,
            ["[#design, since you last read]\nparent: stand-up in five"]
        );
        assert_eq!(
            cursor_of(&fleet, &room, "scout"),
            head_of(&fleet, &room),
            "and its own post still moved the cursor"
        );
    }

    /// ADR-0034 §7: the holder on the roster reads like any seat.
    #[tokio::test]
    async fn a_rostered_holder_reads_the_room_and_an_off_roster_one_reads_nothing() {
        let (fleet, root, _, room) = tree(&["scout", "parent"]).await;
        fleet.post(&room, "the build is green", Some("scout"), ts());
        assert_eq!(
            read_by(&fleet, &root).await,
            ["[#design, since you last read]\nscout: the build is green"]
        );

        let (fleet, root, _, room) = tree(&["scout"]).await;
        fleet.post(&room, "the build is green", Some("scout"), ts());
        assert!(
            read_by(&fleet, &root).await.is_empty(),
            "a room reaches into the tree, not up out of it"
        );
    }

    /// A session no room seats reads nothing at all, and neither does one
    /// beside a room it is not on the roster of.
    #[tokio::test]
    async fn a_session_no_roster_names_reads_nothing() {
        let (fleet, _, _, room) = tree(&["reviewer"]).await;
        let stranger = fleet.child(&fleet.root(), "stranger");
        fleet.post(&room, "the build is green", Some("reviewer"), ts());
        assert!(read_by(&fleet, &stranger).await.is_empty());
    }

    /// Seating writes the cursor at the room's head, so a seat that joins a
    /// running room does not read its history (ADR-0034 §2).
    #[tokio::test]
    async fn a_seat_joins_a_running_room_at_its_head() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let scout = fleet.child(&root, "scout");
        let room = fleet.room(&root, "design");
        fleet.post(&room, "said before you joined", Some("reviewer"), ts());

        seated(
            &fleet,
            &root,
            None,
            &[Seat::read("scout").expect("a roster word")],
        )
        .await;

        assert_eq!(
            cursor_of(&fleet, &room, "scout"),
            head_of(&fleet, &room),
            "seated at the head"
        );
        assert!(read_by(&fleet, &scout).await.is_empty());

        fleet.post(&room, "and this is for you", Some("reviewer"), ts());
        assert_eq!(
            read_by(&fleet, &scout).await,
            ["[#design, since you last read]\nreviewer: and this is for you"]
        );
    }

    /// A reseat is a roster and not a join: the same names again leave every
    /// cursor where it was, so a restart — which reseats every declared room —
    /// does not sweep away what a seat has not read yet.
    #[tokio::test]
    async fn reseating_the_same_roster_marks_nothing_read() {
        let (fleet, root, scout, room) = tree(&["scout"]).await;
        fleet.post(&room, "the build is green", Some("reviewer"), ts());
        assert_eq!(cursor_of(&fleet, &room, "scout"), None, "nothing read yet");

        seated(
            &fleet,
            &root,
            None,
            &[Seat::read("scout").expect("a roster word")],
        )
        .await;

        assert_eq!(cursor_of(&fleet, &room, "scout"), None);
        assert_eq!(
            read_by(&fleet, &scout).await,
            ["[#design, since you last read]\nreviewer: the build is green"],
            "the post is still there to be read"
        );
    }

    /// The fold is one piece however much the room said, and the cursor lands
    /// on the last of it.
    #[tokio::test]
    async fn ten_posts_are_read_as_one_piece_under_one_label() {
        let (fleet, _, scout, room) = tree(&["scout", "reviewer"]).await;
        for n in 1..=10 {
            fleet.post(&room, &format!("post {n}"), Some("reviewer"), ts());
        }

        let read = read_by(&fleet, &scout).await;
        let [said] = read.as_slice() else {
            panic!("one piece, whatever the room said: {read:?}");
        };
        assert_eq!(said.lines().count(), 11, "the label and ten posts: {said}");
        assert!(
            said.starts_with("[#design, since you last read]\n"),
            "{said}"
        );
        assert!(said.ends_with("\nreviewer: post 10"), "{said}");
        assert_eq!(
            cursor_of(&fleet, &room, "scout"),
            head_of(&fleet, &room),
            "and the cursor is at the head"
        );
        assert!(read_by(&fleet, &scout).await.is_empty());
    }

    /// The piece is a fold of posts, so a room with nothing to say makes none.
    #[test]
    fn a_reading_of_no_posts_is_no_piece_at_all() {
        assert_eq!(said("#design", None, &[]), None);
        let state = SessionState::new(crate::tests::summary("ses_x", None, None));
        assert_eq!(cursor::of_state(&state, "scout"), None);
    }

    /// ADR-0053 §1: the purpose is at the head of every reading, so a member
    /// meets what the room is for before it meets what the room said.
    #[tokio::test]
    async fn a_reading_carries_the_purpose_the_room_was_opened_for() {
        let (fleet, _, scout, room) =
            opened_for(Some("settle the storage layout"), &["scout"]).await;
        fleet.post(&room, "the build is green", Some("reviewer"), ts());
        assert_eq!(
            read_by(&fleet, &scout).await,
            [
                "[#design — settle the storage layout, since you last read]\nreviewer: the build is green"
            ]
        );

        let (fleet, _, scout, room) = tree(&["scout"]).await;
        fleet.post(&room, "the build is green", Some("reviewer"), ts());
        assert_eq!(
            read_by(&fleet, &scout).await,
            ["[#design, since you last read]\nreviewer: the build is green"],
            "a room opened without one reads as it always did"
        );
    }

    /// ADR-0053 §4: a closed room is read to its end and then never — and once
    /// there is nothing left in it, it is not a seat this session has at all.
    #[tokio::test]
    async fn a_closed_room_is_read_to_its_end_and_then_never_again() {
        let (fleet, _, scout, room) = tree(&["scout"]).await;
        fleet.post(&room, "it shipped", Some("reviewer"), ts());
        seat::close(
            &fleet.handle(),
            &room,
            &Room::of(&fleet.summary(&room)).expect("a room"),
            "parent",
            None,
        )
        .await
        .expect("a room this crate can close");

        assert_eq!(
            read_by(&fleet, &scout).await,
            ["[#design, since you last read]\nreviewer: it shipped"],
            "what was said before it closed is still read"
        );
        assert!(read_by(&fleet, &scout).await.is_empty(), "and then never");
        assert!(
            first_read_by(&fleet, &scout).await.is_empty(),
            "a room with nothing left in it is no longer a seat to be told about"
        );
    }

    #[test]
    fn it_speaks_at_the_head_of_a_round_under_its_own_name() {
        assert_eq!(Reader.id(), "rooms");
        assert_eq!(Reader.placement(), Placement::RoundStart);
    }

    /// The protocol comes before the first thing a seat reads, and never
    /// again: what it was told is the journal's to say, so nothing beside it
    /// remembers.
    #[tokio::test]
    async fn a_seat_is_told_the_protocol_once_ahead_of_its_first_reading() {
        let (fleet, _, scout, room) = tree(&["scout", "reviewer"]).await;
        fleet.post(&room, "the build is green", Some("reviewer"), ts());

        let first = first_read_by(&fleet, &scout).await;
        assert_eq!(
            first,
            [
                PROTOCOL.to_string(),
                "[#design, since you last read]\nreviewer: the build is green".to_string(),
            ]
        );

        fleet.post(&room, "and the tests pass", Some("reviewer"), ts());
        assert_eq!(
            read_by(&fleet, &scout).await,
            ["[#design, since you last read]\nreviewer: and the tests pass"],
            "a seat that has it is not told twice"
        );
    }

    /// A seated member is told before it has anything to read: a standby
    /// member's first turn opens on its brief, and the room it will work in
    /// is part of what it was seated for.
    #[tokio::test]
    async fn a_seat_with_nothing_to_read_is_still_told_what_a_room_is() {
        let (fleet, _, scout, _) = tree(&["scout"]).await;
        assert_eq!(first_read_by(&fleet, &scout).await, [PROTOCOL]);
    }

    /// And a session no room seats is told nothing: the protocol costs the
    /// prompts that have a room in them and no others.
    #[tokio::test]
    async fn a_session_in_no_room_is_told_nothing() {
        let (fleet, _, _, _) = tree(&["reviewer"]).await;
        let stranger = fleet.child(&fleet.root(), "stranger");
        assert!(first_read_by(&fleet, &stranger).await.is_empty());
    }

    /// The rules the protocol is the one owner of (ADR-0034 §3–4). Each was
    /// stated in a plugin that owns no rooms until M83; the words may move,
    /// but a member that is not told one of them cannot act on it.
    #[test]
    fn the_protocol_says_how_a_room_is_read_what_wakes_a_seat_and_what_it_owes() {
        for rule in [
            "[#<room>, since you last read]",
            "`@<your name>`",
            "`@all`",
            "every member but the one who wrote it",
            "once your patience runs out with something unread",
            "300 seconds",
            "`Listen`",
            "post it back to the room",
            "SendMessage(to: \"#<room>\")",
            "end your turn without posting",
            "opened for one purpose",
            "open another room rather than reseating this one",
        ] {
            assert!(PROTOCOL.contains(rule), "{rule} is unsaid: {PROTOCOL}");
        }
    }
}

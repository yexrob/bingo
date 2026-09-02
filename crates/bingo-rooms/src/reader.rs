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

use async_trait::async_trait;
use bingo_sdk::{
    ContentPart, ContextContributor, ContextError, ContextPiece, ContextQuery, Placement,
    SessionFilter, SessionId, SessionSummary,
};

use crate::cursor::{self, Unread};
use crate::mentions::Post;
use crate::name::{self, PARENT};
use crate::room::{self, Room};

/// What a member reads of its rooms, at the head of its own turn.
#[derive(Debug, Default, Clone, Copy)]
pub struct Reader;

#[async_trait]
impl ContextContributor for Reader {
    fn id(&self) -> &str {
        "rooms"
    }

    fn placement(&self) -> Placement {
        Placement::RoundStart
    }

    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        let mut pieces = Vec::new();
        for (id, room, member) in seated_in(&query).await {
            if let Some(piece) = read(&query, &id, &room, &member).await {
                pieces.push(piece);
            }
        }
        Ok(pieces)
    }
}

/// One room, read: everything it said after this seat's cursor, and the cursor
/// moved to the head of it. A seat level with its room reads nothing and says
/// nothing.
async fn read(
    query: &ContextQuery<'_>,
    id: &SessionId,
    room: &Room,
    member: &str,
) -> Option<ContextPiece> {
    let session = &query.session.id;
    let unread = Unread::of(query.host, id, &room.title, session, member).await;
    let head = unread.head.as_ref()?;
    if let Err(error) = cursor::advance(query.host, session, &room.title, head).await {
        tracing::debug!(room = %room.title, %error, "a seat's cursor did not move");
    }
    let text = said(&room.title, &unread.posts)?;
    Some(ContextPiece::User {
        parts: vec![ContentPart::text(text)],
        label: room.title.clone(),
    })
}

/// The posts as the member reads them: the room and the reading above them,
/// then one line per post under the name that wrote it.
fn said(title: &str, posts: &[Post]) -> Option<String> {
    if posts.is_empty() {
        return None;
    }
    let lines: Vec<String> = posts
        .iter()
        .map(|post| format!("{}: {}", post.author, post.text.trim()))
        .collect();
    Some(format!(
        "[{title}, since you last read]\n{}",
        lines.join("\n")
    ))
}

/// The rooms this session sits in, each with the name it is seated under. A
/// room is read only if its roster names this seat: a room beside a session it
/// never seated reaches it not at all.
async fn seated_in(query: &ContextQuery<'_>) -> Vec<(SessionId, Room, String)> {
    let mut seated = Vec::new();
    for (id, room) in rooms_around(query).await {
        let Some(state) = room::read(query.host, &id).await else {
            continue;
        };
        let room = room.seated(&state);
        let called = seated_as(query.session, &room);
        if let Some(member) = room.members.iter().find(|m| name::same(m, &called)) {
            seated.push((id, room.clone(), member.clone()));
        }
    }
    seated
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
    let children = query
        .host
        .sessions(SessionFilter {
            parent: Some(parent.clone()),
            ..SessionFilter::default()
        })
        .await
        .unwrap_or_default();
    children
        .into_iter()
        .filter_map(|child| Room::of(&child).map(|room| (child.id, room)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ear::Seat;
    use crate::seat;
    use crate::tests::{Fleet, ts};
    use bingo_sdk::{ContextUsage, HostHandle, ItemId, ModelCapabilities, SessionState, TurnId};
    use std::path::Path;

    /// The turn's own facts, none of which a room reads: what a contributor is
    /// handed beside the session it speaks for.
    struct Turn {
        turn: TurnId,
        usage: ContextUsage,
        capabilities: ModelCapabilities,
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
                },
            }
        }
    }

    impl Turn {
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
                items: &[],
                usage: &self.usage,
                capabilities: &self.capabilities,
                cwd: Path::new("/work/project"),
            }
        }
    }

    /// A root with a scout under it, and a room seating them both.
    async fn tree(members: &[&str]) -> (Fleet, SessionId, SessionId, SessionId) {
        let fleet = Fleet::default();
        let root = fleet.root();
        let scout = fleet.child(&root, "scout");
        let seats: Vec<Seat> = members
            .iter()
            .map(|word| Seat::read(word).expect("a roster word"))
            .collect();
        let room = seat::seat(
            &fleet.handle(),
            &root,
            Path::new("/work/project"),
            "design",
            &seats,
        )
        .await
        .expect("a room this crate can open");
        (fleet, root, scout, room)
    }

    /// What one session's turn would be handed at its head.
    async fn read_by(fleet: &Fleet, session: &SessionId) -> Vec<String> {
        let summary = fleet.summary(session);
        let host = fleet.handle();
        let turn = Turn::default();
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

    fn cursor_of(fleet: &Fleet, seat: &SessionId, room: &str) -> Option<ItemId> {
        cursor::of_state(&fleet.state(seat), room)
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
            cursor_of(&fleet, &scout, "#design"),
            fleet.state(&room).items.last().map(|item| item.id.clone()),
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

        seat::seat(
            &fleet.handle(),
            &root,
            Path::new("/work/project"),
            "design",
            &[Seat::read("scout").expect("a roster word")],
        )
        .await
        .expect("a room this crate can open");

        assert!(
            cursor_of(&fleet, &scout, "#design").is_some(),
            "seated at the head"
        );
        assert!(read_by(&fleet, &scout).await.is_empty());

        fleet.post(&room, "and this is for you", Some("reviewer"), ts());
        assert_eq!(
            read_by(&fleet, &scout).await,
            ["[#design, since you last read]\nreviewer: and this is for you"]
        );
    }

    /// The piece is a fold of posts, so a room with nothing to say makes none.
    #[test]
    fn a_reading_of_no_posts_is_no_piece_at_all() {
        assert_eq!(said("#design", &[]), None);
        let state = SessionState::new(crate::tests::summary("ses_x", None, None));
        assert_eq!(cursor::of_state(&state, "#design"), None);
    }

    #[test]
    fn it_speaks_at_the_head_of_a_round_under_its_own_name() {
        assert_eq!(Reader.id(), "rooms");
        assert_eq!(Reader.placement(), Placement::RoundStart);
    }
}

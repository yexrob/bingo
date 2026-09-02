//! Where a seat has read a room up to (ADR-0034 §2). A post is written once,
//! into the room's own journal, and what a seat has read of it is one fact on
//! that seat's own session. Everything that asks "has this seat seen that?" —
//! the reader at the head of a turn, the patience deadline, the serial rule —
//! asks this, and nothing keeps a second answer.
//!
//! ADR-0034 calls the cursor the room's `seq`. A `Seq` addresses a frame, and
//! the only door onto frames by seq is `events_since`, a stream that never ends
//! on its own — nothing a turn can drain. The kernel's own address for a post
//! is the `ItemId` its journal gave it, which a snapshot hands back in order,
//! so that is what the cursor holds: the same watermark, in the vocabulary the
//! host actually offers.
//!
//! A seat with no cursor has not read the room at all, and starts at the head
//! the room had when that seat's own session opened: a post fanned out before a
//! session existed reached nobody, so it is not a backlog that session owes
//! (ADR-0025 §2). Seating writes the cursor for a seat that is already there,
//! so a member joining a running room does not inherit its history either.

use bingo_sdk::{HostHandle, ItemId, KernelError, SessionId, SessionState};
use jiff::Timestamp;
use serde_json::{Value, json};

use crate::mentions::Post;
use crate::{PLUGIN, name, room};

/// The kind a seat's cursor into one room is published under, before that
/// room's title: a kind per room, so a seat in two of them keeps two cursors
/// and neither writes over the other (the `ear:` precedent, ADR-0029 §4).
pub const CURSOR: &str = "cursor:";

/// The one thing a cursor payload holds.
const POST: &str = "post";

pub fn kind(room: &str) -> String {
    format!("{CURSOR}{room}")
}

/// What a cursor is published as.
pub fn payload(post: &ItemId) -> Value {
    json!({ POST: post.as_str() })
}

/// The post a payload points at, or nothing for a payload that is not one.
pub fn stored(payload: &Value) -> Option<ItemId> {
    payload[POST].as_str().map(ItemId::from_raw)
}

/// Where this seat has read one room up to — what `--continue` finds where the
/// last process left it, and what a surface counts the unread from.
pub fn of_state(state: &SessionState, room: &str) -> Option<ItemId> {
    stored(state.extensions.get(PLUGIN)?.get(&kind(room))?)
}

/// The posts a seat has not read: the ones after its cursor, or — for a seat
/// that has none — the ones the room said after that seat came into being.
pub fn unread<'a>(posts: &'a [Post], cursor: Option<&ItemId>, since: Timestamp) -> &'a [Post] {
    if let Some(at) = cursor.and_then(|read| posts.iter().position(|post| &post.id == read)) {
        return &posts[at + 1..];
    }
    let from = posts
        .iter()
        .position(|post| post.at >= since)
        .unwrap_or(posts.len());
    &posts[from..]
}

/// What one seat has not read of one room: the posts it has to read, and the
/// head to move its cursor to once it has. A seat's own posts are in neither
/// ledger — it wrote them — but they do move the cursor, because a seat that
/// wrote the room's last word is not behind on it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Unread {
    pub posts: Vec<Post>,
    pub head: Option<ItemId>,
}

impl Unread {
    /// What a seat has not read of a room, as the two journals say it. A
    /// session either of them cannot be read of says nothing.
    pub async fn of(
        host: &HostHandle,
        room: &SessionId,
        title: &str,
        seat: &SessionId,
        member: &str,
    ) -> Unread {
        let (Some(there), Some(here)) =
            (room::read(host, room).await, room::read(host, seat).await)
        else {
            return Unread::default();
        };
        Unread::between(&there, &here, title, member)
    }

    /// The same, from two snapshots a caller already holds.
    pub fn between(room: &SessionState, seat: &SessionState, title: &str, member: &str) -> Unread {
        let posts: Vec<Post> = room.items.iter().filter_map(Post::of).collect();
        let unread = unread(
            &posts,
            of_state(seat, title).as_ref(),
            seat.summary.created_at,
        );
        Unread {
            head: unread.last().map(|post| post.id.clone()),
            posts: unread
                .iter()
                .filter(|post| !name::same(&post.author, member))
                .cloned()
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.posts.is_empty()
    }
}

/// Move a seat's cursor to a post it has now read.
pub async fn advance(
    host: &HostHandle,
    seat: &SessionId,
    room: &str,
    post: &ItemId,
) -> Result<(), KernelError> {
    host.extend(seat, PLUGIN, &kind(room), payload(post)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Fleet, posted_item};
    use bingo_sdk::SessionState;

    fn post(n: i64, author: &str) -> Post {
        Post {
            id: ItemId::from_raw(format!("itm_{n}")),
            author: author.into(),
            at: Timestamp::from_second(n).expect("a timestamp"),
            text: format!("post {n}"),
        }
    }

    fn ids(posts: &[Post]) -> Vec<&str> {
        posts.iter().map(|post| post.id.as_str()).collect()
    }

    /// The persisted shape, written down: a cursor is a payload another process
    /// — and another crate's serial rule — reads back by hand (ADR-0034 §2).
    #[test]
    fn a_cursor_is_one_post_id_under_a_kind_of_its_own() {
        assert_eq!(kind("#design"), "cursor:#design");
        assert_eq!(
            payload(&ItemId::from_raw("itm_7")),
            json!({ "post": "itm_7" })
        );

        const WRITTEN: &str = r#"{"post":"itm_7"}"#;
        let payload: Value = serde_json::from_str(WRITTEN).expect("a cursor payload");
        assert_eq!(stored(&payload), Some(ItemId::from_raw("itm_7")));
        assert_eq!(stored(&json!({})), None, "a payload that is not one");
        assert_eq!(stored(&Value::Null), None);
    }

    /// The whole of the split, as one table: after the cursor, or — with none —
    /// after the seat itself came into being.
    #[test]
    fn what_is_unread_is_what_landed_after_the_cursor() {
        let posts = [post(1, "scout"), post(2, "reviewer"), post(3, "scout")];
        let at = |n: i64| Timestamp::from_second(n).expect("a timestamp");

        let read_two = ItemId::from_raw("itm_2");
        assert_eq!(ids(unread(&posts, Some(&read_two), at(0))), ["itm_3"]);
        assert!(
            unread(&posts, Some(&ItemId::from_raw("itm_3")), at(0)).is_empty(),
            "a seat at the head has nothing to read"
        );
        assert_eq!(
            ids(unread(&posts, None, at(0))),
            ["itm_1", "itm_2", "itm_3"],
            "a seat as old as the room reads all of it"
        );
        assert_eq!(
            ids(unread(&posts, None, at(2))),
            ["itm_2", "itm_3"],
            "and one that arrived later reads only what it was there for"
        );
        assert!(
            unread(&posts, None, at(9)).is_empty(),
            "a seat newer than every post starts level"
        );
        assert_eq!(
            ids(unread(&posts, Some(&ItemId::from_raw("itm_gone")), at(2))),
            ["itm_2", "itm_3"],
            "a cursor the room does not hold falls back to when the seat began"
        );
    }

    /// A kind per room, so a seat in two of them keeps two cursors and neither
    /// writes over the other.
    #[tokio::test]
    async fn a_seat_s_own_journal_says_where_it_has_read_each_room_up_to() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let scout = fleet.child(&root, "scout");
        let host = fleet.handle();

        advance(&host, &scout, "#design", &ItemId::from_raw("itm_2"))
            .await
            .expect("a cursor this crate can write");
        advance(&host, &scout, "#standup", &ItemId::from_raw("itm_9"))
            .await
            .expect("a cursor this crate can write");

        let state = room::read(&host, &scout).await.expect("the seat");
        assert_eq!(of_state(&state, "#design"), Some(ItemId::from_raw("itm_2")));
        assert_eq!(
            of_state(&state, "#standup"),
            Some(ItemId::from_raw("itm_9")),
            "one room's cursor is not another's"
        );
        assert_eq!(of_state(&state, "#elsewhere"), None);
    }

    /// What a seat is handed to read: everyone else's posts after its cursor,
    /// and the head that its own post still moves.
    #[test]
    fn a_seat_reads_the_others_and_its_own_post_still_moves_the_cursor() {
        let mut room = SessionState::new(crate::tests::summary("ses_room", Some("#design"), None));
        room.items = vec![
            posted_item("first", Some("reviewer")),
            posted_item("mine", Some("scout")),
        ];
        let seat = SessionState::new(crate::tests::summary("ses_scout", Some("scout"), None));

        let unread = Unread::between(&room, &seat, "#design", "scout");
        assert_eq!(
            unread
                .posts
                .iter()
                .map(|p| p.text.as_str())
                .collect::<Vec<_>>(),
            ["first"],
            "a seat is never handed its own post"
        );
        assert_eq!(
            unread.head.as_ref(),
            Some(&room.items[1].id),
            "and the cursor still moves past it"
        );
    }
}

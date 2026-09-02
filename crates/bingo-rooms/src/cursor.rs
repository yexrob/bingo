//! Where a seat has read a room up to (ADR-0034 §2). A post is written once,
//! into the room's own journal, and what each seat has read of it is one fact
//! beside the posts: a `cursor:<member>` register in that same journal, a kind
//! per seat, so two seats reading at once write two facts rather than racing
//! over one (the `ear:` precedent, ADR-0029 §4). Everything that asks "has this
//! seat seen that?" — the reader at the head of a turn, the patience deadline,
//! the serial rule — asks the room, and nothing keeps a second answer.
//!
//! The room's journal is where it lives because a room is a `Log` session: it
//! answers nobody, so reading it costs nothing and takes nothing away. A
//! member's own session is not like that — a reader that opens one and lets go
//! is its last client, and closing it under an idle seat loses the very wake
//! this plugin just sent. So the cursor is kept where it can be read without
//! touching the seat at all, and a name nobody holds yet can be seated with one.
//!
//! ADR-0034 calls the cursor the room's `seq`. A `Seq` addresses a frame, and
//! the only door onto frames by seq is `events_since`, a stream that never ends
//! on its own — nothing a turn can drain. The kernel's own address for a post
//! is the `ItemId` its journal gave it, which a snapshot hands back in order,
//! so that is what the cursor holds: the same watermark, in the vocabulary the
//! host actually offers.

use bingo_sdk::{HostHandle, ItemId, KernelError, SessionId, SessionState};
use serde_json::{Value, json};

use crate::mentions::Post;
use crate::{PLUGIN, name};

/// The kind one seat's cursor is published under, before its name.
pub const CURSOR: &str = "cursor:";

/// The one thing a cursor payload holds.
const POST: &str = "post";

/// A room compares names in any case, so a register is keyed in one spelling
/// of it — the same rule an ear's register keeps.
pub fn kind(member: &str) -> String {
    format!("{CURSOR}{}", member.to_lowercase())
}

/// What a cursor is published as.
pub fn payload(post: &ItemId) -> Value {
    json!({ POST: post.as_str() })
}

/// The post a payload points at, or nothing for a payload that is not one.
pub fn stored(payload: &Value) -> Option<ItemId> {
    payload[POST].as_str().map(ItemId::from_raw)
}

/// Where one seat has read this room up to, as the room's own journal says.
/// A seat with no register has read none of it.
pub fn of_state(room: &SessionState, member: &str) -> Option<ItemId> {
    stored(room.extensions.get(PLUGIN)?.get(&kind(member))?)
}

/// The posts a seat has not read: the ones after its cursor, and all of them
/// for a seat that has none.
pub fn unread<'a>(posts: &'a [Post], cursor: Option<&ItemId>) -> &'a [Post] {
    let Some(read) = cursor else {
        return posts;
    };
    match posts.iter().position(|post| &post.id == read) {
        Some(at) => &posts[at + 1..],
        // A cursor the room does not hold points at nothing it can measure
        // against, so it says as little as no cursor at all.
        None => posts,
    }
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
    /// What a seat has not read, from the room's own snapshot and nothing else.
    pub fn of(room: &SessionState, member: &str) -> Unread {
        let posts: Vec<Post> = room.items.iter().filter_map(Post::of).collect();
        let unread = unread(&posts, of_state(room, member).as_ref());
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
    room: &SessionId,
    member: &str,
    post: &ItemId,
) -> Result<(), KernelError> {
    host.extend(room, PLUGIN, &kind(member), payload(post))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::room;
    use crate::tests::{Fleet, posted_item, summary};
    use jiff::Timestamp;

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
        assert_eq!(kind("Scout"), "cursor:scout");
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

    /// The whole of the split, as one table: what landed after the cursor.
    #[test]
    fn what_is_unread_is_what_landed_after_the_cursor() {
        let posts = [post(1, "scout"), post(2, "reviewer"), post(3, "scout")];

        let read_two = ItemId::from_raw("itm_2");
        assert_eq!(ids(unread(&posts, Some(&read_two))), ["itm_3"]);
        assert!(
            unread(&posts, Some(&ItemId::from_raw("itm_3"))).is_empty(),
            "a seat at the head has nothing to read"
        );
        assert_eq!(
            ids(unread(&posts, None)),
            ["itm_1", "itm_2", "itm_3"],
            "a seat with no cursor has read none of it"
        );
        assert_eq!(
            ids(unread(&posts, Some(&ItemId::from_raw("itm_gone")))),
            ["itm_1", "itm_2", "itm_3"],
            "and a cursor the room does not hold measures nothing"
        );
    }

    /// A kind per seat, so a room with two of them keeps two cursors and
    /// neither writes over the other.
    #[tokio::test]
    async fn a_room_s_own_journal_says_where_each_seat_has_read_up_to() {
        let fleet = Fleet::default();
        let root = fleet.root();
        let id = fleet.room(&root, "design");
        let host = fleet.handle();

        advance(&host, &id, "scout", &ItemId::from_raw("itm_2"))
            .await
            .expect("a cursor this crate can write");
        advance(&host, &id, "Reviewer", &ItemId::from_raw("itm_9"))
            .await
            .expect("a cursor this crate can write");

        let state = room::read(&host, &id).await.expect("the room");
        assert_eq!(of_state(&state, "scout"), Some(ItemId::from_raw("itm_2")));
        assert_eq!(
            of_state(&state, "reviewer"),
            Some(ItemId::from_raw("itm_9")),
            "one seat's cursor is not another's, in any case"
        );
        assert_eq!(of_state(&state, "nobody"), None);
    }

    /// What a seat is handed to read: everyone else's posts after its cursor,
    /// and the head that its own post still moves.
    #[test]
    fn a_seat_reads_the_others_and_its_own_post_still_moves_the_cursor() {
        let mut room = SessionState::new(summary("ses_room", Some("#design"), None));
        room.items = vec![
            posted_item("first", Some("reviewer")),
            posted_item("mine", Some("scout")),
        ];

        let unread = Unread::of(&room, "scout");
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

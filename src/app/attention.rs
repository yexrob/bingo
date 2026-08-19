//! What the user has read, and what is still owed to them.
//!
//! Attention is the one piece of conversation state a frontend cannot safely
//! infer from text: whether a room post has been looked at, whether a colleague
//! is still waiting on an answer, whether the user's own name went by unnoticed.
//! It lives here so both frontends read one answer.
//!
//! **Unread is derived, not counted** — the rule the terminal front end has held
//! since D88 (`tui::buffer`), kept because it is right: a counter fed by events
//! can drift from the thing it counts, and a cursor cannot. What is stored is one
//! cursor per conversation; every badge is a subtraction against the log.
//!
//! **Reading is not marking.** `conversation/read` and every prefetch leave these
//! cursors exactly where they were (spec invariant #14). Only
//! `conversation/markRead` moves one, and it carries the revision the client
//! believed it was looking at, so a stale view cannot clear attention it never
//! saw.
//!
//! **Main is not counted**, and that is D103 rather than an omission: main's flow
//! is the screen the user is already sitting in front of, so a badge on it would
//! be the console asking the user to look at what they are looking at.

use std::collections::HashMap;

use crate::app::conversation::{ConvKey, Record};
use crate::app::ids::{ItemId, UnixMillis};
use crate::app::snapshot::{Item, ItemBody, Obligation, ObligationKind};

/// One conversation's read cursor.
#[derive(Debug, Default, Clone)]
struct Cursor {
    /// The last item the user said they had seen.
    read: Option<ItemId>,
    /// How many items of the log that was. Kept beside the identifier because a
    /// retry checkpoint can remove the item a cursor names, and a count that
    /// still means "everything up to here" outlives the identifier that did.
    read_count: usize,
    /// The last room sequence the user said they had seen. Rooms are paged by
    /// their own sequence, which is what a client already has in hand.
    read_room_seq: u64,
}

/// Every conversation's read cursor, and the counts derived from them.
#[derive(Debug, Default)]
pub(crate) struct Attention {
    cursors: HashMap<ConvKey, Cursor>,
}

/// What one conversation owes the user, as a summary carries it.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Standing {
    pub unread: u32,
    pub mentions: u32,
    pub read_cursor: Option<ItemId>,
    pub obligations: Vec<Obligation>,
}

impl Attention {
    /// Move one conversation's cursor. The only thing that ever does.
    ///
    /// `last_item_id` names the furthest item the frontend actually showed;
    /// `last_room_seq` does the same in a room's own unit. Both are clamped to
    /// what exists, so a client cannot mark ahead of the log.
    pub(crate) fn mark_read(
        &mut self,
        key: &ConvKey,
        record: &Record,
        last_item_id: Option<&ItemId>,
        last_room_seq: Option<u64>,
    ) {
        let cursor = self.cursors.entry(key.clone()).or_default();
        if let Some(item) = last_item_id {
            // An identifier the log no longer holds marks nothing rather than
            // resetting the cursor to the start.
            if let Some(index) = record.index_of(item) {
                let count = index.saturating_add(1);
                if count > cursor.read_count {
                    cursor.read_count = count;
                    cursor.read = Some(item.clone());
                }
            }
        }
        if let Some(seq) = last_room_seq {
            cursor.read_room_seq = cursor.read_room_seq.max(seq);
        }
    }

    /// Mark everything currently in the log as read. What entering a
    /// conversation means: the whole page is on screen.
    pub(crate) fn mark_all_read(&mut self, key: &ConvKey, record: &Record) {
        let cursor = self.cursors.entry(key.clone()).or_default();
        cursor.read_count = record.items.len();
        cursor.read = record.last_item_id();
        cursor.read_room_seq = cursor.read_room_seq.max(last_room_seq(&record.items));
    }

    /// A conversation seen for the first time starts read: its past is not news.
    pub(crate) fn seed(&mut self, key: &ConvKey, record: &Record) {
        if self.cursors.contains_key(key) {
            return;
        }
        self.mark_all_read(key, record);
    }

    pub(crate) fn read_room_seq(&self, key: &ConvKey) -> u64 {
        self.cursors
            .get(key)
            .map_or(0, |cursor| cursor.read_room_seq)
    }

    /// What this conversation owes the user right now.
    ///
    /// `owed` is what the collaboration registries say is outstanding — a room
    /// mention nobody answered, a message the user sent that nobody replied to,
    /// a question waiting on them. It is passed in rather than read here because
    /// the registries are the actor's and this is only the arithmetic.
    pub(crate) fn standing(
        &self,
        key: &ConvKey,
        record: &Record,
        owed: Vec<Obligation>,
    ) -> Standing {
        let cursor = self.cursors.get(key).cloned().unwrap_or_default();
        // Main's own flow is the screen the user is sitting in front of.
        if key.is_main() {
            return Standing {
                unread: 0,
                mentions: 0,
                read_cursor: cursor.read.clone(),
                obligations: owed,
            };
        }
        let tail = record.items.get(cursor.read_count..).unwrap_or_default();
        let unread = tail.iter().filter(|item| is_message(item)).count() as u32;
        let mentions = tail.iter().filter(|item| names_the_user(item)).count() as u32;
        Standing {
            unread,
            mentions,
            read_cursor: cursor.read,
            obligations: owed,
        }
    }
}

/// A message somebody would count. A tool call, a notice, and a permission
/// receipt are work, not conversation, and the terminal front end has never
/// counted them either (D99).
fn is_message(item: &Item) -> bool {
    matches!(
        item.body,
        ItemBody::UserMessage { .. }
            | ItemBody::AssistantMessage { .. }
            | ItemBody::PeerMessage { .. }
            | ItemBody::RoomMessage { .. }
    )
}

/// Something addressed to the user in particular: their name in a room, or a
/// colleague answering them in private.
fn names_the_user(item: &Item) -> bool {
    match &item.body {
        ItemBody::RoomMessage { mentions, from, .. } => {
            from != crate::channels::USER_NAME
                && mentions.iter().any(|name| {
                    name.eq_ignore_ascii_case(crate::channels::USER_NAME)
                        || name.eq_ignore_ascii_case(crate::channels::ALL_NAME)
                })
        }
        // The DM lane's rule: it wants you when the other side answers, and not
        // when you speak (D99).
        ItemBody::PeerMessage { from, .. } => from != crate::channels::USER_NAME,
        _ => false,
    }
}

/// Something the user themselves put there. Their own words are never unread,
/// and neither is their own arrival: the domain says the same thing when a post
/// advances the sender's own cursor and an invite seats a late joiner at the
/// head.
pub(crate) fn authored_by_user(item: &Item) -> bool {
    match &item.body {
        ItemBody::UserMessage { .. } => true,
        ItemBody::RoomMessage { from, .. } | ItemBody::PeerMessage { from, .. } => {
            from == crate::channels::USER_NAME
        }
        _ => false,
    }
}

fn last_room_seq(items: &[Item]) -> u64 {
    items
        .iter()
        .filter_map(|item| match &item.body {
            ItemBody::RoomMessage { room_seq, .. } => Some(*room_seq),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// The user was named in a room and has not spoken since.
pub(crate) fn mention_debt(from: &str, since: UnixMillis) -> Obligation {
    Obligation {
        kind: ObligationKind::MentionDebt,
        from: Some(from.to_string()),
        item_id: None,
        since,
    }
}

/// A message the user sent that nobody has answered.
pub(crate) fn unanswered(to: &str, since: UnixMillis) -> Obligation {
    Obligation {
        kind: ObligationKind::UnansweredMessage,
        from: Some(to.to_string()),
        item_id: None,
        since,
    }
}

/// A prompt is open on the user.
pub(crate) fn awaiting_user(since: UnixMillis) -> Obligation {
    Obligation {
        kind: ObligationKind::AwaitingUser,
        from: None,
        item_id: None,
        since,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::conversation::Conversations;
    use crate::app::ids::{EpochId, IdMint, RoomId};
    use crate::app::snapshot::ItemStatus;

    fn room_item(id: &str, seq: u64, from: &str, mentions: &[&str]) -> Item {
        Item {
            id: ItemId::new(id),
            status: ItemStatus::Completed,
            turn_id: None,
            started_at: Some(1),
            completed_at: Some(1),
            body: ItemBody::RoomMessage {
                room_id: RoomId::new("room_1"),
                from: from.to_string(),
                text: "hello".to_string(),
                room_seq: seq,
                mentions: mentions.iter().map(|name| name.to_string()).collect(),
            },
        }
    }

    /// Reading has no side effect. Only `markRead` moves the cursor, and it
    /// moves it exactly as far as the client said it had looked.
    #[test]
    fn only_marking_read_clears_what_is_unread() {
        let mut mint = IdMint::new(EpochId::mint());
        let mut conversations = Conversations::new(&mut mint);
        let key = ConvKey::Room("build".to_string());
        let mut attention = Attention::default();
        conversations.append(&mut mint, &key, room_item("item_1", 1, "scout", &[]));
        attention.seed(
            &key,
            conversations
                .record(&key)
                .unwrap_or_else(|| panic!("the room exists")),
        );

        conversations.append(&mut mint, &key, room_item("item_2", 2, "scout", &["user"]));
        conversations.append(&mut mint, &key, room_item("item_3", 3, "scout", &[]));
        let record = conversations
            .record(&key)
            .unwrap_or_else(|| panic!("the room exists"));
        let standing = attention.standing(&key, record, Vec::new());
        assert_eq!(standing.unread, 2, "the two posts since the cursor");
        assert_eq!(standing.mentions, 1, "one of them said the user's name");

        // Reading the page again changes nothing: the snapshot is a read.
        let again = attention.standing(&key, record, Vec::new());
        assert_eq!(again, standing, "reading is not marking");

        attention.mark_read(&key, record, Some(&ItemId::new("item_2")), Some(2));
        let record = conversations
            .record(&key)
            .unwrap_or_else(|| panic!("the room exists"));
        let standing = attention.standing(&key, record, Vec::new());
        assert_eq!(standing.unread, 1, "only what the client said it showed");
        assert_eq!(standing.mentions, 0);
        assert_eq!(standing.read_cursor, Some(ItemId::new("item_2")));
        assert_eq!(attention.read_room_seq(&key), 2);
    }

    /// A conversation the user has never opened starts read: its past is not
    /// news, and a badge for it would be the registry inventing one.
    #[test]
    fn a_conversation_seen_for_the_first_time_starts_read() {
        let mut mint = IdMint::new(EpochId::mint());
        let mut conversations = Conversations::new(&mut mint);
        let key = ConvKey::Room("build".to_string());
        for n in 1..=3 {
            conversations.append(
                &mut mint,
                &key,
                room_item(&format!("item_{n}"), n, "qa", &[]),
            );
        }
        let mut attention = Attention::default();
        let record = conversations
            .record(&key)
            .unwrap_or_else(|| panic!("the room exists"));
        attention.seed(&key, record);
        assert_eq!(attention.standing(&key, record, Vec::new()).unread, 0);
    }

    /// A cursor never goes backwards, and never marks ahead of the log.
    #[test]
    fn a_cursor_only_moves_forward_and_only_over_what_exists() {
        let mut mint = IdMint::new(EpochId::mint());
        let mut conversations = Conversations::new(&mut mint);
        let key = ConvKey::Room("build".to_string());
        for n in 1..=3 {
            conversations.append(
                &mut mint,
                &key,
                room_item(&format!("item_{n}"), n, "qa", &[]),
            );
        }
        let mut attention = Attention::default();
        let record = conversations
            .record(&key)
            .unwrap_or_else(|| panic!("the room exists"));
        attention.mark_read(&key, record, Some(&ItemId::new("item_3")), None);
        attention.mark_read(&key, record, Some(&ItemId::new("item_1")), None);
        assert_eq!(
            attention.standing(&key, record, Vec::new()).read_cursor,
            Some(ItemId::new("item_3")),
            "a later cursor is not undone by an earlier one"
        );
        attention.mark_read(&key, record, Some(&ItemId::new("item_99")), None);
        assert_eq!(
            attention.standing(&key, record, Vec::new()).read_cursor,
            Some(ItemId::new("item_3")),
            "an identifier the log does not hold marks nothing"
        );
    }

    /// Main's flow is the screen the user is already reading (D103).
    #[test]
    fn main_never_counts_its_own_flow() {
        let mut mint = IdMint::new(EpochId::mint());
        let mut conversations = Conversations::new(&mut mint);
        let key = ConvKey::Main;
        conversations.append(&mut mint, &key, room_item("item_1", 1, "scout", &["user"]));
        let attention = Attention::default();
        let record = conversations
            .record(&key)
            .unwrap_or_else(|| panic!("main exists"));
        let standing = attention.standing(&key, record, vec![awaiting_user(7)]);
        assert_eq!((standing.unread, standing.mentions), (0, 0));
        assert_eq!(
            standing.obligations.len(),
            1,
            "an obligation is still an obligation on main"
        );
    }
}

//! Which conversations there are, what each one holds, and the identifier each
//! one answers to.
//!
//! [`ConvKey`] is how the process has always named a conversation — main, an
//! instance, a room — and it stays that way: it is what a page, an event sink and
//! a queue entry are keyed by. [`ConversationId`] is what a client sees, opaque
//! and minted by the actor. [`Conversations`] is the one place the two meet, so a
//! name that appears twice is the same conversation both times.
//!
//! B4 gave it the content. A conversation owns an ordered log of completed items
//! — a room post, a colleague's message, the prompt a run opened with — and the
//! two numbers a client pages it by: `revision`, which every published summary
//! bumps, and `history_generation`, which only a structural rewrite does. An item
//! cursor is bound to the generation it was issued under, so a continuation
//! across a rewind is refused rather than silently mixing two histories.

use std::collections::HashMap;

use crate::app::ids::{ConversationId, IdMint, ItemId, UnixMillis, now_millis};
use crate::app::snapshot::{Item, ItemCursor, Page};

/// Which conversation an event, or a page, belongs to.
///
/// Main is a key like any other (D134). It differs in exactly one way — it
/// talks to the user by default — and that difference lives in the composer,
/// not in the store.
///
/// `Room` never appears on an `Addressed` event: a room is a log, not a turn
/// loop, so nothing streams into it. It is a key because the console keeps a
/// store per *page*, and a room is one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ConvKey {
    Main,
    Agent(String),
    Room(String),
}

impl ConvKey {
    pub fn is_main(&self) -> bool {
        matches!(self, ConvKey::Main)
    }

    /// The instance whose stream this addresses, if any.
    pub fn agent(&self) -> Option<&str> {
        match self {
            ConvKey::Agent(name) => Some(name),
            _ => None,
        }
    }

    /// The room this names, if any.
    pub fn room(&self) -> Option<&str> {
        match self {
            ConvKey::Room(name) => Some(name),
            _ => None,
        }
    }

    /// What a frontend calls this conversation before anybody renames it.
    pub fn title(&self) -> String {
        match self {
            ConvKey::Main => format!("@{}", crate::channels::MAIN_NAME),
            ConvKey::Agent(name) => format!("@{name}"),
            ConvKey::Room(name) => format!("#{name}"),
        }
    }
}

/// One conversation's own state: what is in it, and how far it has moved.
#[derive(Debug)]
pub(crate) struct Record {
    pub id: ConversationId,
    /// Bumped by every *published* summary, and carried by `markRead` so a stale
    /// client cannot clear attention it never saw.
    pub revision: u64,
    /// Bumped only by a structural rewrite — a rewind, a compaction. Ordinary
    /// appends leave it alone, which is what keeps an item cursor valid.
    pub history_generation: u64,
    /// The completed items, in order. Live items belong to the turn producing
    /// them until it finishes with them.
    pub items: Vec<Item>,
    pub last_activity_at: Option<UnixMillis>,
    /// The summary has been published at least once, so the next one is an
    /// update rather than a creation.
    pub announced: bool,
}

impl Record {
    fn new(id: ConversationId) -> Self {
        Self {
            id,
            revision: 0,
            history_generation: 1,
            items: Vec::new(),
            last_activity_at: None,
            announced: false,
        }
    }

    pub fn last_item_id(&self) -> Option<ItemId> {
        self.items.last().map(|item| item.id.clone())
    }

    /// How far into the log this identifier is, or `None` if it names nothing
    /// here.
    pub fn index_of(&self, item: &ItemId) -> Option<usize> {
        self.items.iter().position(|held| &held.id == item)
    }
}

/// The identifier every conversation the session has touched answers to, and
/// what each one holds.
///
/// Identifiers are minted on first mention and kept for the epoch: a client that
/// saw `conv_3` twice saw the same conversation twice. Main is minted at startup
/// because it exists before anything happens in it.
#[derive(Debug)]
pub(crate) struct Conversations {
    ids: HashMap<ConvKey, ConversationId>,
    keys: HashMap<ConversationId, ConvKey>,
    records: HashMap<ConvKey, Record>,
    /// The order conversations were first named in, which is the order
    /// `conversation/list` pages them in.
    order: Vec<ConvKey>,
}

impl Conversations {
    /// Start with main, which is the one conversation a session always has.
    pub(crate) fn new(mint: &mut IdMint) -> Self {
        let mut conversations = Self {
            ids: HashMap::new(),
            keys: HashMap::new(),
            records: HashMap::new(),
            order: Vec::new(),
        };
        conversations.id(mint, &ConvKey::Main);
        conversations
    }

    /// This conversation's identifier, minting one the first time it is asked
    /// for.
    pub(crate) fn id(&mut self, mint: &mut IdMint, key: &ConvKey) -> ConversationId {
        if let Some(id) = self.ids.get(key) {
            return id.clone();
        }
        let id: ConversationId = mint.mint();
        self.ids.insert(key.clone(), id.clone());
        self.keys.insert(id.clone(), key.clone());
        self.records.insert(key.clone(), Record::new(id.clone()));
        self.order.push(key.clone());
        id
    }

    /// Which conversation an identifier names, or `None` if it names none.
    pub(crate) fn key(&self, id: &ConversationId) -> Option<&ConvKey> {
        self.keys.get(id)
    }

    pub(crate) fn record(&self, key: &ConvKey) -> Option<&Record> {
        self.records.get(key)
    }

    pub(crate) fn record_mut(&mut self, mint: &mut IdMint, key: &ConvKey) -> &mut Record {
        self.id(mint, key);
        self.records
            .get_mut(key)
            .unwrap_or_else(|| unreachable!("the record was just minted"))
    }

    /// Every conversation, in the order it was first named.
    pub(crate) fn keys(&self) -> &[ConvKey] {
        &self.order
    }

    /// Append one completed item and say what identifier it landed under.
    pub(crate) fn append(&mut self, mint: &mut IdMint, key: &ConvKey, item: Item) {
        let record = self.record_mut(mint, key);
        record.last_activity_at = Some(item.completed_at.unwrap_or_else(now_millis));
        record.items.push(item);
    }

    /// Replace the whole log, which is what a rewrite is: the generation moves
    /// and every cursor issued against the old one is refused from here on.
    pub(crate) fn rewrite(&mut self, mint: &mut IdMint, key: &ConvKey, items: Vec<Item>) {
        let record = self.record_mut(mint, key);
        record.items = items;
        record.history_generation = record.history_generation.saturating_add(1);
        record.last_activity_at = Some(now_millis());
    }

    /// One page of history, oldest first, starting after `cursor`.
    ///
    /// A cursor from a stale generation is refused rather than answered from a
    /// history it was not issued against (spec "Client request taxonomy").
    pub(crate) fn page(
        &self,
        key: &ConvKey,
        cursor: Option<&ItemCursor>,
        limit: usize,
    ) -> Result<Page<Item>, StalePage> {
        let Some(record) = self.records.get(key) else {
            return Ok(Page {
                items: Vec::new(),
                revision: 0,
                next_cursor: None,
            });
        };
        let start = match cursor {
            None => 0,
            Some(cursor) => {
                if cursor.history_generation != record.history_generation {
                    return Err(StalePage);
                }
                match record.index_of(&cursor.after) {
                    Some(index) => index.saturating_add(1),
                    // The generation matches but the item is gone: an explicit
                    // retry checkpoint removed it, and the page after it is the
                    // rest of the log rather than nothing.
                    None => 0,
                }
            }
        };
        let end = start.saturating_add(limit).min(record.items.len());
        let items = record.items.get(start..end).unwrap_or_default().to_vec();
        let next_cursor = if end < record.items.len() {
            items.last().map(|item| item.id.to_string())
        } else {
            None
        };
        Ok(Page {
            items,
            revision: record.revision,
            next_cursor,
        })
    }

    /// How many items each conversation holds, for a snapshot that reports
    /// collection sizes without carrying them.
    pub(crate) fn count(&self, key: &ConvKey) -> u32 {
        self.records
            .get(key)
            .map_or(0, |record| record.items.len() as u32)
    }
}

/// A continuation from a generation that no longer exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StalePage;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ids::EpochId;
    use crate::app::snapshot::{ItemBody, ItemStatus};

    fn item(id: &str, text: &str) -> Item {
        Item {
            id: ItemId::new(id),
            status: ItemStatus::Completed,
            turn_id: None,
            started_at: Some(1),
            completed_at: Some(1),
            body: ItemBody::AssistantMessage {
                text: text.to_string(),
            },
        }
    }

    #[test]
    fn a_name_keeps_the_identifier_it_was_first_given() {
        let mut mint = IdMint::new(EpochId::mint());
        let mut conversations = Conversations::new(&mut mint);
        let main = conversations.id(&mut mint, &ConvKey::Main);
        assert_eq!(
            main,
            ConversationId::new("conv_1"),
            "main exists before anything happens in it"
        );
        let scout = conversations.id(&mut mint, &ConvKey::Agent("scout".to_string()));
        assert_eq!(
            conversations.id(&mut mint, &ConvKey::Agent("scout".to_string())),
            scout,
            "a second mention of one name is the same conversation"
        );
        assert_ne!(scout, main);
        assert_eq!(
            conversations.key(&scout),
            Some(&ConvKey::Agent("scout".to_string()))
        );
        assert_eq!(conversations.key(&ConversationId::new("conv_99")), None);
    }

    #[test]
    fn an_instance_and_a_room_of_the_same_name_are_two_conversations() {
        let mut mint = IdMint::new(EpochId::mint());
        let mut conversations = Conversations::new(&mut mint);
        let agent = conversations.id(&mut mint, &ConvKey::Agent("build".to_string()));
        let room = conversations.id(&mut mint, &ConvKey::Room("build".to_string()));
        assert_ne!(agent, room, "@build and #build are not each other");
    }

    /// A page is bound to the generation it was issued under. A rewrite moves
    /// the generation, and the cursor from before it is refused rather than
    /// answered out of a history it never saw.
    #[test]
    fn a_cursor_from_a_rewritten_history_is_refused() {
        let mut mint = IdMint::new(EpochId::mint());
        let mut conversations = Conversations::new(&mut mint);
        let room = ConvKey::Room("build".to_string());
        for n in 1..=5 {
            conversations.append(&mut mint, &room, item(&format!("item_{n}"), "hello"));
        }
        let first = conversations
            .page(&room, None, 2)
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(first.items.len(), 2);
        assert_eq!(first.next_cursor.as_deref(), Some("item_2"));

        let generation = conversations
            .record(&room)
            .map(|record| record.history_generation)
            .unwrap_or_default();
        let cursor = ItemCursor {
            history_generation: generation,
            after: ItemId::new("item_2"),
        };
        let second = conversations
            .page(&room, Some(&cursor), 10)
            .unwrap_or_else(|error| panic!("{error:?}"));
        assert_eq!(second.items.len(), 3, "the rest of the log follows");
        assert_eq!(second.next_cursor, None);

        conversations.rewrite(&mut mint, &room, vec![item("item_9", "summary")]);
        assert_eq!(
            conversations.page(&room, Some(&cursor), 10),
            Err(StalePage),
            "a continuation across a rewrite is refused, never mixed"
        );
    }
}

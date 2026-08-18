//! Which conversations there are, and the identifier each one answers to.
//!
//! [`ConvKey`] is how the process has always named a conversation — main, an
//! instance, a room — and it stays that way: it is what a page, an event sink and
//! a queue entry are keyed by. [`ConversationId`] is what a client sees, opaque
//! and minted by the actor. [`Conversations`] is the one place the two meet, so a
//! name that appears twice is the same conversation both times.
//!
//! The full conversation resource — summaries, attention cursors, obligations —
//! is B4's. What lands here is identity, which turns and queue entries need
//! before any of that exists.

use std::collections::HashMap;

use crate::app::ids::{ConversationId, IdMint};

/// Which conversation an event, or a page, belongs to.
///
/// Main is a key like any other (D134). It differs in exactly one way — it
/// talks to the user by default — and that difference lives in the composer,
/// not in the store.
///
/// `Room` never appears on an `Addressed` event: a room is a log, not a turn
/// loop, so nothing streams into it. It is a key because the console keeps a
/// store per *page*, and a room is one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
}

/// The identifier every conversation the session has touched answers to.
///
/// Identifiers are minted on first mention and kept for the epoch: a client that
/// saw `conv_3` twice saw the same conversation twice. Main is minted at startup
/// because it exists before anything happens in it.
#[derive(Debug)]
pub(crate) struct Conversations {
    ids: HashMap<ConvKey, ConversationId>,
    keys: HashMap<ConversationId, ConvKey>,
}

impl Conversations {
    /// Start with main, which is the one conversation a session always has.
    pub(crate) fn new(mint: &mut IdMint) -> Self {
        let mut conversations = Self {
            ids: HashMap::new(),
            keys: HashMap::new(),
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
        id
    }

    /// Which conversation an identifier names, or `None` if it names none.
    pub(crate) fn key(&self, id: &ConversationId) -> Option<&ConvKey> {
        self.keys.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ids::EpochId;

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
}

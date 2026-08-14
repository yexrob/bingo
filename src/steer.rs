//! Mid-turn steering: the composer's queue reaching a turn that is still working.
//!
//! A message typed while a turn is busy is queued, and until now the queue was only
//! drained at TurnEnd — the model finished the work the correction was about before it
//! ever read the correction. Claude Code injects queued messages at the running turn's
//! next *tool barrier* (its `next` priority): the moment tool results are assembled and
//! the following request has not gone out yet. Nothing is cancelled, nothing is
//! re-prompted; the text simply rides along in the same user message as the tool
//! results, and the model reads it before deciding what to do next.
//!
//! This module is only the channel between the two sides. The composer offers items,
//! the turn takes them, and both need the take to be atomic: whatever the turn took is
//! already on its way to the model, so the composer must neither show it as pending nor
//! let `↑` pull it back into the input. [`SteerQueue::take`] is that instant, and
//! [`SteerQueue::reclaim`] is how the composer asks whether it lost the race.

use std::sync::{Arc, Mutex};

/// The line a steered message arrives under in the model's context.
///
/// It reaches the model inside the same user message as the tool results, where an
/// unlabelled paragraph would read as tool output rather than as the user speaking.
/// The wording follows the marker family already in use — `[DM from user]` (D64),
/// `[Request interrupted by user]` (D76): a bracketed statement of fact, no
/// instruction attached.
pub const STEER_MARKER: &str = "[Message from user, sent while you were working]";

/// Prefix of the line a steered message leaves in the transcript flow.
///
/// The glyph is what tells a reader this message was injected mid-turn rather than
/// typed at an idle prompt — the reply above it was written without it, the reply
/// below it with it.
pub const STEER_FLOW_PREFIX: &str = "↪ ";

/// One queued plain message offered to the running turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SteerItem {
    /// The composer's own id for the queued entry, so an absorbed item can be taken
    /// out of the queue by identity rather than by matching its text — two identical
    /// messages are two messages.
    pub id: u64,
    /// What the user typed.
    pub text: String,
}

impl SteerItem {
    /// The user content block this item contributes at the barrier.
    pub fn block_text(&self) -> String {
        format!("{STEER_MARKER}\n{}", self.text)
    }
}

/// What happened when the composer tried to take a queued item back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reclaim {
    /// It was still pending and is now out of the channel: the composer owns it.
    Pulled,
    /// The turn already took it. The composer must treat the pull-back as a no-op —
    /// the text is in the request, and the absorption event will remove it from the
    /// queue on its own.
    Absorbed,
    /// It was never offered (a slash command, an item carrying images, or one queued
    /// behind either): the channel has no claim on it and the composer proceeds.
    Untracked,
}

/// The channel between the chat composer and the foreground turn.
///
/// Cloning shares one channel. It belongs to exactly one turn: [`SteerQueue::reset`] at
/// every turn boundary is what keeps a message the previous turn declined from being
/// injected into a turn the user never typed it at.
#[derive(Clone, Default)]
pub struct SteerQueue(Arc<Mutex<Inner>>);

#[derive(Default)]
struct Inner {
    /// Offered and not yet taken, in the order the user typed them.
    pending: Vec<SteerItem>,
    /// Ids this turn already took. Kept so a re-arm cannot offer the same message
    /// twice: the composer re-arms from its queue, and the queue still holds an
    /// absorbed item until the absorption event reaches it.
    taken: Vec<u64>,
}

impl SteerQueue {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Replaces what is on offer with `items`, minus anything this turn already took.
    ///
    /// The composer calls this whenever its queue changes, so the channel is a
    /// projection of the queue rather than a second copy that can drift from it.
    pub fn rearm(&self, items: Vec<SteerItem>) {
        let mut inner = self.lock();
        let taken = std::mem::take(&mut inner.taken);
        inner.pending = items
            .into_iter()
            .filter(|item| !taken.contains(&item.id))
            .collect();
        inner.taken = taken;
    }

    /// Clears the channel for a new turn: nothing pending, nothing claimed.
    pub fn reset(&self) {
        let mut inner = self.lock();
        inner.pending.clear();
        inner.taken.clear();
    }

    /// The barrier's atomic take: everything pending at this instant, in order.
    ///
    /// The caller is committing to append what it gets, so from here on the items
    /// count as absorbed even before the composer has been told.
    pub fn take(&self) -> Vec<SteerItem> {
        let mut inner = self.lock();
        let items = std::mem::take(&mut inner.pending);
        inner.taken.extend(items.iter().map(|item| item.id));
        items
    }

    /// The composer taking an item back (`↑` pull-back). See [`Reclaim`].
    pub fn reclaim(&self, id: u64) -> Reclaim {
        let mut inner = self.lock();
        if let Some(pos) = inner.pending.iter().position(|item| item.id == id) {
            inner.pending.remove(pos);
            return Reclaim::Pulled;
        }
        if inner.taken.contains(&id) {
            return Reclaim::Absorbed;
        }
        Reclaim::Untracked
    }

    /// Whether anything is on offer (tests and assertions; the barrier just takes).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_empty(&self) -> bool {
        self.lock().pending.is_empty()
    }
}

impl std::fmt::Debug for SteerQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.lock();
        f.debug_struct("SteerQueue")
            .field("pending", &inner.pending.len())
            .field("taken", &inner.taken.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u64, text: &str) -> SteerItem {
        SteerItem {
            id,
            text: text.to_string(),
        }
    }

    #[test]
    fn take_is_atomic_and_empties_the_channel() {
        let queue = SteerQueue::new();
        queue.rearm(vec![item(1, "a"), item(2, "b")]);
        assert_eq!(queue.take(), vec![item(1, "a"), item(2, "b")]);
        assert!(queue.is_empty(), "a second barrier finds nothing left");
        assert!(queue.take().is_empty());
    }

    /// The composer re-arms from its own queue, which still holds an absorbed item
    /// until the absorption event lands. Without the taken-id ledger that re-arm
    /// would offer the same message to the next barrier.
    #[test]
    fn rearm_never_re_offers_what_the_turn_already_took() {
        let queue = SteerQueue::new();
        queue.rearm(vec![item(1, "a")]);
        assert_eq!(queue.take(), vec![item(1, "a")]);
        queue.rearm(vec![item(1, "a"), item(2, "b")]);
        assert_eq!(
            queue.take(),
            vec![item(2, "b")],
            "only the message the turn has not seen is offered again"
        );
    }

    #[test]
    fn reclaim_reports_who_won_the_race() {
        let queue = SteerQueue::new();
        queue.rearm(vec![item(1, "a"), item(2, "b")]);
        assert_eq!(queue.reclaim(2), Reclaim::Pulled);
        assert_eq!(queue.take(), vec![item(1, "a")]);
        assert_eq!(
            queue.reclaim(1),
            Reclaim::Absorbed,
            "the turn took it first: the pull-back must be a no-op"
        );
        assert_eq!(
            queue.reclaim(9),
            Reclaim::Untracked,
            "an item that was never offered is the composer's alone"
        );
    }

    #[test]
    fn reset_hands_a_clean_channel_to_the_next_turn() {
        let queue = SteerQueue::new();
        queue.rearm(vec![item(1, "a")]);
        let _ = queue.take();
        queue.reset();
        queue.rearm(vec![item(1, "a")]);
        assert_eq!(
            queue.take(),
            vec![item(1, "a")],
            "the ledger belongs to the turn that filled it"
        );
    }

    /// The marker is what lets the model tell the user's interjection from the tool
    /// output it arrives beside.
    #[test]
    fn block_text_carries_the_marker_above_the_message() {
        assert_eq!(
            item(1, "use tabs").block_text(),
            format!("{STEER_MARKER}\nuse tabs")
        );
    }
}

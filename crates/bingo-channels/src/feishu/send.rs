//! Five sends a second, per chat (ADR-0016 §6).
//!
//! The five are shared with every other bot in a group, so the queue is per
//! chat rather than per app: two conversations never wait for each other, and
//! two messages in one conversation always do. The same gap throttles a
//! stream's element updates, which are exempt from the quota but not from
//! good manners.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;

/// The gap between two sends in one chat: five a second, with a little room.
pub const GAP: Duration = Duration::from_millis(200);

/// When a chat may next be spoken to.
type Slot = Arc<tokio::sync::Mutex<Instant>>;

#[derive(Debug, Default)]
pub struct Queue {
    chats: Mutex<HashMap<String, Slot>>,
}

impl Queue {
    /// Wait for this chat's turn, and take it.
    pub async fn turn(&self, chat: &str) {
        let slot = self.slot(chat);
        let mut next = slot.lock().await;
        tokio::time::sleep_until(*next).await;
        *next = Instant::now() + GAP;
    }

    fn slot(&self, chat: &str) -> Slot {
        let mut chats = self
            .chats
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        Arc::clone(
            chats
                .entry(chat.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(Instant::now()))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn two_messages_in_one_chat_are_spaced_and_two_chats_are_not() {
        let queue = Queue::default();
        let started = Instant::now();
        queue.turn("oc_1").await;
        queue.turn("oc_2").await;
        assert_eq!(Instant::now(), started, "different chats do not queue");
        queue.turn("oc_1").await;
        assert_eq!(
            Instant::now().duration_since(started),
            GAP,
            "the same chat waits its turn"
        );
    }
}

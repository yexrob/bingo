//! Which chat a session is sitting in, while the surface is running.
//!
//! The tool that posts a file (ADR-0051 §3) is handed a session id and nothing
//! else: the kernel knows no chats, and a conversation is this plugin's own
//! fact. This is where a session id becomes a chat again — one map, written by
//! the runner that owns the conversation and read by the tool.
//!
//! It also carries whether the surface is running at all, because that is what
//! decides whether the tool exists (ADR-0009 §1: answering with nothing is
//! never wrong). A directory nobody is serving from lends out no seat.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use bingo_sdk::SessionId;

use crate::adapter::ChannelAdapter;
use crate::conversation::{Conversation, Posted};

/// Where one session's chat is, and what to post there through.
#[derive(Clone)]
pub struct Seat {
    pub adapter: Arc<dyn ChannelAdapter>,
    pub conversation: Conversation,
    /// The message a reply hangs under, from whoever spoke last. The runner's
    /// only copy of it: a file goes out under the same message the answer
    /// does, and two copies would be two answers to "under what".
    pub parent: Option<Posted>,
}

impl std::fmt::Debug for Seat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Seat")
            .field("adapter", &self.adapter.id())
            .field("conversation", &self.conversation)
            .field("parent", &self.parent)
            .finish()
    }
}

/// Every conversation with a session, shared between the surface that runs
/// them and the tool that has to find one again.
#[derive(Clone, Debug, Default)]
pub struct Directory {
    seats: Arc<Mutex<BTreeMap<SessionId, Seat>>>,
    serving: Arc<AtomicBool>,
}

impl Directory {
    /// This session's conversation is `seat`, from now until it leaves.
    pub fn sit(&self, session: SessionId, seat: Seat) {
        locked(&self.seats).insert(session, seat);
    }

    /// The conversation is over: a tool call after this is refused in words
    /// rather than posting into a chat nobody is reading.
    pub fn leave(&self, session: &SessionId) {
        locked(&self.seats).remove(session);
    }

    pub fn seat(&self, session: &SessionId) -> Option<Seat> {
        locked(&self.seats).get(session).cloned()
    }

    /// What a reply in this conversation now hangs under.
    pub fn under(&self, session: &SessionId, parent: Option<Posted>) {
        if let Some(seat) = locked(&self.seats).get_mut(session) {
            seat.parent = parent;
        }
    }

    pub fn parent(&self, session: &SessionId) -> Option<Posted> {
        locked(&self.seats)
            .get(session)
            .and_then(|seat| seat.parent.clone())
    }

    /// The surface is running, for as long as the guard lives. Dropping it is
    /// what clears the flag, so an early refusal leaves nothing behind.
    pub fn serving(&self) -> Serving {
        self.serving.store(true, Ordering::Release);
        Serving(Arc::clone(&self.serving))
    }

    pub fn is_serving(&self) -> bool {
        self.serving.load(Ordering::Acquire)
    }
}

/// The surface's run, as something that ends by itself.
pub struct Serving(Arc<AtomicBool>);

impl Drop for Serving {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn locked<T>(slot: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loopback::{Config, Loopback};

    fn seat() -> Seat {
        Seat {
            adapter: Arc::new(Loopback::new(Config::default())) as Arc<dyn ChannelAdapter>,
            conversation: Conversation::direct("oc_1"),
            parent: None,
        }
    }

    fn session() -> SessionId {
        SessionId::from_raw("ses_1")
    }

    #[test]
    fn a_session_that_sat_down_is_found_again_and_one_that_left_is_not() {
        let directory = Directory::default();
        assert!(directory.seat(&session()).is_none());
        directory.sit(session(), seat());
        let found = directory.seat(&session()).expect("a seat");
        assert_eq!(found.conversation, Conversation::direct("oc_1"));
        assert_eq!(found.adapter.id(), "loopback");
        directory.leave(&session());
        assert!(directory.seat(&session()).is_none());
    }

    #[test]
    fn the_parent_is_the_seats_and_a_session_with_no_seat_keeps_none() {
        let directory = Directory::default();
        directory.sit(session(), seat());
        directory.under(&session(), Some(Posted::new("om_1")));
        assert_eq!(directory.parent(&session()), Some(Posted::new("om_1")));
        directory.under(&SessionId::from_raw("ses_other"), Some(Posted::new("om_2")));
        assert_eq!(
            directory.parent(&session()),
            Some(Posted::new("om_1")),
            "another session's parent is not this one's"
        );
        directory.under(&session(), None);
        assert_eq!(directory.parent(&session()), None);
    }

    #[test]
    fn the_directory_serves_only_while_the_guard_lives() {
        let directory = Directory::default();
        assert!(!directory.is_serving());
        let serving = directory.serving();
        assert!(directory.is_serving());
        drop(serving);
        assert!(
            !directory.is_serving(),
            "a run that returned serves nothing"
        );
    }
}

//! Starting an actor: fresh from a summary, or back from its journal.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use bingo_sdk::*;
use tokio::sync::mpsc;

use super::Actor;
use super::mailbox::Mailbox;
use super::subscribers::Subscribers;
use crate::turn::TurnConfig;

/// Start an actor. The turn config is built second because its tool host
/// talks back through the mailbox.
pub fn spawn(
    summary: SessionSummary,
    store: Option<Arc<dyn SessionStore>>,
    config: impl FnOnce(&Mailbox) -> Arc<TurnConfig>,
) -> Mailbox {
    spawn_with(summary, Vec::new(), store, config)
}

/// A session read back from its journal (ADR-0005): the frames are the
/// actor's own history, the state is their fold by the one reducer, and the
/// seq goes on from the last frame.
pub fn resume(
    frames: Vec<Frame>,
    store: Option<Arc<dyn SessionStore>>,
    config: impl FnOnce(&Mailbox) -> Arc<TurnConfig>,
) -> Result<Mailbox, KernelError> {
    let head = head_summary(&frames)?;
    Ok(spawn_with(head, frames, store, config))
}

/// The first frame of every journal is the session saying what it is.
pub fn head_summary(frames: &[Frame]) -> Result<SessionSummary, KernelError> {
    match frames.first().map(|f| &f.event) {
        Some(Event::SessionUpdated { summary }) => Ok(summary.clone()),
        _ => Err(KernelError::new(
            ErrorCode::Storage,
            "the journal does not start with the session's summary",
        )),
    }
}

fn spawn_with(
    head: SessionSummary,
    journal: Vec<Frame>,
    store: Option<Arc<dyn SessionStore>>,
    config: impl FnOnce(&Mailbox) -> Arc<TurnConfig>,
) -> Mailbox {
    let (tx, rx) = mpsc::unbounded_channel();
    let mailbox = Mailbox::new(head.id.clone(), tx);
    let config = config(&mailbox);
    let mut state = SessionState::new(head.clone());
    for frame in &journal {
        state.apply(frame);
    }
    // A `SessionClosed` in the journal ended the last process's segment, not
    // the session: it is open again by being here.
    state.closed = false;
    let actor = Actor {
        id: head.id,
        mailbox: mailbox.clone(),
        rx,
        seq: journal.last().map_or(Seq::ZERO, |f| f.seq),
        generation: state.history_generation,
        state,
        journal,
        store,
        config,
        subscribers: Subscribers::default(),
        running: None,
        queue: VecDeque::new(),
        queue_revision: 0,
        pending: HashMap::new(),
        closing: None,
        progress_n: 0,
    };
    tokio::spawn(actor.run());
    mailbox
}

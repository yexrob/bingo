//! The turn in flight: the task it runs on, the token that stops it, and the
//! config and tool set it is judged by for as long as it lasts. The actor
//! holds at most one, and everything the rest of the session may ask about
//! "the running turn" is asked of this.

use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use bingo_sdk::*;
use futures::FutureExt;
use tokio::task::JoinHandle;

use super::mailbox::{Msg, TurnMail};
use super::{Actor, TurnKind, panic_message};
use crate::turn::{TurnConfig, TurnRun, run_turn};

pub(super) struct Running {
    pub(super) turn: TurnId,
    pub(super) cancel: CancellationToken,
    task: JoinHandle<()>,
    /// The config this turn started under; the session's may have been
    /// rebuilt since, and a call is judged by the turn's own.
    pub(super) config: Arc<TurnConfig>,
    /// The tools it resolved, once it has said so (`Msg::Offered`). Empty
    /// until then, which is the fail-closed reading: a call naming a tool
    /// nothing has offered yet is refused.
    pub(super) tools: Vec<Arc<dyn Tool>>,
}

impl Running {
    /// The turn loop runs in its own task and reports back by mail; a panic in
    /// it becomes a failed turn rather than a lost session.
    pub(super) fn spawn(
        actor: &Actor,
        turn: TurnId,
        cancel: CancellationToken,
        kind: TurnKind,
    ) -> Self {
        let run = TurnRun {
            turn: turn.clone(),
            history: actor.journal.clone(),
            generation: actor.generation,
            cancel: cancel.clone(),
            kind,
        };
        let cfg = Arc::clone(&actor.config);
        let mailbox = actor.mailbox.clone();
        let host = TurnMail {
            mailbox: mailbox.clone(),
            turn: turn.clone(),
        };
        let config = Arc::clone(&cfg);
        let task = tokio::spawn(async move {
            let outcome = AssertUnwindSafe(run_turn(&cfg, run, &host))
                .catch_unwind()
                .await
                .map_err(panic_message);
            mailbox.send(Msg::TurnFinished {
                turn: host.turn.clone(),
                outcome,
            });
        });
        Self {
            turn,
            cancel,
            task,
            config,
            tools: Vec::new(),
        }
    }

    /// Drop the turn where it stands: the loop is told to stop and the task is
    /// not waited for. Used only when the actor itself is going.
    pub(super) fn abandon(self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

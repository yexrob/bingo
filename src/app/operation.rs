//! Accepted asynchronous work that is not naturally a turn.
//!
//! Bringing a team up, authenticating with a provider, reconnecting an MCP
//! server: each is something the user asked for that takes a while, reports
//! progress, and finishes exactly once — like a turn, and unlike a turn in that
//! no model is running and no conversation is being written.
//!
//! The registry is the actor's, like every other. What it guarantees is the
//! same thing the turn registry does: **exactly one terminal state**, whatever
//! ends the work.

use std::collections::HashMap;

use tokio::sync::{mpsc, oneshot};

use crate::app::answer::Answer;
use crate::app::controller::Control;
use crate::app::ids::{IdMint, OperationId, now_millis};
use crate::app::snapshot::{
    ItemBody, Operation, OperationKind, OperationProgress, OperationStatus, TurnError,
};

pub(crate) enum OperationMsg {
    Start {
        kind: OperationKind,
        /// The page it was asked on, when it was asked on one.
        conversation: Option<crate::app::ids::ConversationId>,
        reply: oneshot::Sender<OperationId>,
    },
    Progress {
        id: OperationId,
        progress: OperationProgress,
    },
    Finish {
        id: OperationId,
        status: OperationStatus,
        error: Option<TurnError>,
    },
}

/// What one operation's change asks the actor to publish.
pub(crate) enum OperationChange {
    Started(Box<Operation>),
    Progressed {
        id: OperationId,
        progress: OperationProgress,
    },
    Completed(Box<Operation>),
}

/// The operations of one session, owned by the actor.
#[derive(Default)]
pub(crate) struct OperationRegistry {
    open: HashMap<OperationId, Operation>,
}

/// How the rest of the process reaches them.
#[derive(Clone)]
pub struct OperationHandle {
    control: mpsc::UnboundedSender<Control>,
}

impl std::fmt::Debug for OperationHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OperationHandle")
    }
}

impl OperationHandle {
    pub(crate) fn new(control: mpsc::UnboundedSender<Control>) -> Self {
        Self { control }
    }

    /// Accept a piece of work and name it.
    pub fn start(
        &self,
        kind: OperationKind,
        conversation: Option<crate::app::ids::ConversationId>,
    ) -> Answer<OperationId> {
        let (reply, answer) = oneshot::channel();
        let _ = self.control.send(Control::Operation(OperationMsg::Start {
            kind,
            conversation,
            reply,
        }));
        Answer::new(answer, OperationId::new(""))
    }

    pub fn progress(&self, id: &OperationId, label: impl Into<String>, done: u32, total: u32) {
        let _ = self
            .control
            .send(Control::Operation(OperationMsg::Progress {
                id: id.clone(),
                progress: OperationProgress {
                    label: label.into(),
                    done: Some(done),
                    total: Some(total),
                },
            }));
    }

    /// Close it. The first terminal state wins, as it does for a turn.
    pub fn finish(&self, id: &OperationId, status: OperationStatus, error: Option<TurnError>) {
        let _ = self.control.send(Control::Operation(OperationMsg::Finish {
            id: id.clone(),
            status,
            error,
        }));
    }

    /// Write what the work produced into the page it was asked on.
    ///
    /// An operation says that something is happening and when it stopped; what
    /// it *did* is an item in a conversation's log, because that is where a
    /// client reads the history of a session rather than the state of its
    /// background work. The two together are one report.
    pub fn record(&self, conversation: crate::app::conversation::ConvKey, body: ItemBody) {
        let _ = self.control.send(Control::Record {
            conversation,
            body: Box::new(body),
        });
    }
}

impl OperationRegistry {
    pub(crate) fn handle(
        &mut self,
        message: OperationMsg,
        mint: &mut IdMint,
    ) -> Vec<OperationChange> {
        match message {
            OperationMsg::Start {
                kind,
                conversation,
                reply,
            } => {
                let id: OperationId = mint.mint();
                let operation = Operation {
                    id: id.clone(),
                    kind,
                    status: OperationStatus::Running,
                    // The page it was asked on, which is where its report lands:
                    // an action carries its origin conversation (D135a), and work
                    // nobody asked for on a page carries none.
                    conversation_id: conversation,
                    progress: None,
                    started_at: now_millis(),
                    completed_at: None,
                    error: None,
                };
                self.open.insert(id.clone(), operation.clone());
                let _ = reply.send(id);
                vec![OperationChange::Started(Box::new(operation))]
            }
            OperationMsg::Progress { id, progress } => {
                let Some(operation) = self.open.get_mut(&id) else {
                    return Vec::new();
                };
                if operation.status != OperationStatus::Running {
                    return Vec::new();
                }
                operation.progress = Some(progress.clone());
                vec![OperationChange::Progressed { id, progress }]
            }
            OperationMsg::Finish { id, status, error } => {
                let Some(operation) = self.open.get_mut(&id) else {
                    return Vec::new();
                };
                if operation.status != OperationStatus::Running {
                    return Vec::new();
                }
                operation.status = status;
                operation.completed_at = Some(now_millis());
                operation.error = error.filter(|_| status == OperationStatus::Failed);
                vec![OperationChange::Completed(Box::new(operation.clone()))]
            }
        }
    }

    /// Everything still running, for a snapshot.
    pub(crate) fn running(&self) -> Vec<Operation> {
        let mut open: Vec<Operation> = self
            .open
            .values()
            .filter(|operation| operation.status == OperationStatus::Running)
            .cloned()
            .collect();
        open.sort_by(|a, b| a.id.cmp(&b.id));
        open
    }

    /// Close everything still open, which is what a session ending does to work
    /// nobody is left to finish.
    pub(crate) fn close_all(&mut self) -> Vec<OperationChange> {
        let ids: Vec<OperationId> = self.running().into_iter().map(|open| open.id).collect();
        let mut changes = Vec::new();
        for id in ids {
            changes.extend(self.handle_finish(&id));
        }
        changes
    }

    fn handle_finish(&mut self, id: &OperationId) -> Vec<OperationChange> {
        let Some(operation) = self.open.get_mut(id) else {
            return Vec::new();
        };
        operation.status = OperationStatus::Cancelled;
        operation.completed_at = Some(now_millis());
        vec![OperationChange::Completed(Box::new(operation.clone()))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::ids::EpochId;

    fn registry() -> (OperationRegistry, IdMint) {
        (OperationRegistry::default(), IdMint::new(EpochId::mint()))
    }

    fn start(registry: &mut OperationRegistry, mint: &mut IdMint) -> OperationId {
        let (reply, answer) = oneshot::channel();
        registry.handle(
            OperationMsg::Start {
                kind: OperationKind::TeamStart,
                conversation: None,
                reply,
            },
            mint,
        );
        answer
            .blocking_recv()
            .unwrap_or_else(|error| panic!("{error}"))
    }

    /// One terminal state, whatever tries to close it twice.
    #[test]
    fn an_operation_reaches_exactly_one_terminal_state() {
        let (mut registry, mut mint) = registry();
        let id = start(&mut registry, &mut mint);
        assert_eq!(registry.running().len(), 1);

        let done = registry.handle(
            OperationMsg::Finish {
                id: id.clone(),
                status: OperationStatus::Completed,
                error: None,
            },
            &mut mint,
        );
        assert_eq!(done.len(), 1);
        assert!(registry.running().is_empty());

        let again = registry.handle(
            OperationMsg::Finish {
                id: id.clone(),
                status: OperationStatus::Failed,
                error: None,
            },
            &mut mint,
        );
        assert!(again.is_empty(), "the first terminal state wins");

        let late = registry.handle(
            OperationMsg::Progress {
                id,
                progress: OperationProgress {
                    label: "still going".to_string(),
                    done: Some(1),
                    total: Some(2),
                },
            },
            &mut mint,
        );
        assert!(late.is_empty(), "and nothing reports after it");
    }

    /// A session that ends cancels the work nobody is left to finish.
    #[test]
    fn closing_a_session_cancels_what_is_still_running() {
        let (mut registry, mut mint) = registry();
        let _ = start(&mut registry, &mut mint);
        let closed = registry.close_all();
        assert_eq!(closed.len(), 1);
        assert!(registry.running().is_empty());
    }
}

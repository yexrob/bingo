//! The engine a session actually runs on.
//!
//! [`crate::app::engine::Engine`] says what the core needs; this is the one
//! implementation that calls a provider. It holds two things the actor cannot:
//! a [`Session`], which is what a run reads its model, tools and transcript
//! from, and a runtime handle, because the actor's loop is a thread rather than
//! a task and has nothing to spawn on.
//!
//! Nothing is reported back through the return value. A run publishes the way
//! every run publishes — [`EngineEvent`] into the turn it was bound to — so the
//! core hears about the work through the same door whichever frontend asked for
//! it.

use std::sync::Arc;

use tokio::sync::watch;

use crate::app::engine::{Engine, Run};
use crate::app::ids::TurnId;
use crate::engine::events::{EngineEvents, EngineHost, EngineRequests};
use crate::query::Session;

/// One session's engine.
pub struct SessionEngine {
    session: Arc<Session>,
    runtime: tokio::runtime::Handle,
    /// Interruption, as the run watches for it. One channel for the session
    /// rather than one per run: `turn/interrupt` names a turn, and the run that
    /// owns it is the only one listening when it fires.
    cancel: watch::Sender<bool>,
}

impl SessionEngine {
    /// Build the engine for a session, on the runtime it will run its work on.
    ///
    /// `None` when there is no runtime to run on. That is not a failure to
    /// report — it is a caller that assembled a session without one, and a
    /// session with no engine is a session that can be read and configured but
    /// not run, which is a state the core already has an answer for.
    pub fn new(session: Arc<Session>) -> Option<Arc<Self>> {
        Some(Arc::new(Self {
            session,
            runtime: tokio::runtime::Handle::try_current().ok()?,
            cancel: watch::channel(false).0,
        }))
    }

    /// The history a turn starts from. A transcript that has not been written to
    /// yet is an empty history, not a failure.
    fn history(
        session: &Session,
        warn: &mut (dyn FnMut(String) + Send),
    ) -> Vec<crate::api::types::Message> {
        let Some(transcript) = session.runtime.transcript.borrow().clone() else {
            return Vec::new();
        };
        match transcript.load_messages() {
            Ok(messages) => messages,
            Err(crate::transcript::TranscriptError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                Vec::new()
            }
            Err(error) => {
                warn(format!("transcript load failed: {error}"));
                Vec::new()
            }
        }
    }

    /// One run, from the turn the core opened to the terminal state it reaches.
    ///
    /// The guard is what makes "exactly one terminal state" hold through an
    /// abort: a task that is cancelled executes none of its own code, and the
    /// guard's `Drop` closes the turn for it (spec invariant #5).
    fn spawn(&self, turn: TurnId, work: Work) {
        let session = self.session.clone();
        let cancel = self.cancel.subscribe();
        self.cancel.send_replace(false);
        self.runtime.spawn(async move {
            let host = host_for(&session, turn.clone());
            let guard = session
                .turns
                .guard(turn, crate::app::snapshot::TurnStatus::Failed);
            let history = Self::history(&session, &mut host.events.warn_sink());
            let outcome = match work {
                Work::Prompt(text) => {
                    crate::query::run_query(&session, history, &text, &[], &host, Some(cancel))
                        .await
                }
                Work::Shell(command) => {
                    crate::query::run_bash_command(&session, &command, history, &host, Some(cancel))
                        .await
                }
            };
            match outcome {
                Ok(outcome) if outcome.aborted => {
                    guard.finish(crate::app::snapshot::TurnStatus::Interrupted, None)
                }
                Ok(_) => guard.finish(crate::app::snapshot::TurnStatus::Completed, None),
                Err(error) => guard.finish(
                    crate::app::snapshot::TurnStatus::Failed,
                    Some(crate::app::snapshot::TurnError {
                        code: crate::error::map_error(&error).to_string(),
                        message: crate::error::sanitize_msg(&error.to_string()),
                    }),
                ),
            }
        });
    }
}

/// The host one run reports through: bound to its turn, asking the core's
/// prompts, steering from the core's queue, publishing its shell tail.
fn host_for(session: &Arc<Session>, turn: TurnId) -> EngineHost {
    let conversation = crate::app::conversation::ConvKey::Main;
    let mut host = EngineHost::new(
        // No frontend sink: everything this run says is the turn's, and the turn
        // is the actor's. A second sink here would be the shim the console still
        // has and this path never needed.
        EngineEvents::new(|_| {}),
        EngineRequests {
            ask: crate::app::interaction::permission_ask(
                session.interactions.clone(),
                conversation.clone(),
            ),
            ask_question: crate::app::interaction::question_ask(
                session.interactions.clone(),
                conversation,
            ),
            steer: steering(session, turn.clone()),
            live: crate::live::LiveBash::detached(),
        },
    );
    host = host.bound(session.turns.clone(), turn);
    // The tail sink needs the bound events, so it is fitted after: a sample
    // published by an unbound host would reach nobody.
    host.requests.live = crate::live::LiveBash::new(host.events.tail_sink());
    host
}

/// This turn's tool barrier: what was queued while it worked (D83).
///
/// The take is atomic because it happens inside the actor — whatever comes back
/// is off the queue and on its way to the model, so a pull-back racing it has
/// already lost. What the client sees is `queue/itemAbsorbed`, which the core
/// publishes on the way out.
fn steering(session: &Arc<Session>, turn: TurnId) -> Arc<crate::query::SteerFn> {
    let queue = session.queue.clone();
    Arc::new(move || {
        let queue = queue.clone();
        let turn = turn.clone();
        Box::pin(async move {
            queue
                .absorb(crate::app::conversation::ConvKey::Main, turn)
                .await
        })
    })
}

/// What a run is running.
enum Work {
    Prompt(String),
    Shell(String),
}

impl Engine for SessionEngine {
    fn run(&self, run: Run) {
        match run {
            Run::Turn { turn, text } => self.spawn(turn, Work::Prompt(text)),
            Run::Shell { turn, command } => self.spawn(turn, Work::Shell(command)),
            // Both of these touch the registries through their handles, which is
            // a message to the actor — so they cannot run on the actor's own
            // thread, and they go to the runtime like everything else here.
            Run::Wake => {
                let session = self.session.clone();
                self.runtime.spawn_blocking(move || {
                    crate::tool::agent::flush_agent_inbox(&session, &session.watch);
                });
            }
            Run::Posted(posted) => {
                let session = self.session.clone();
                self.runtime.spawn_blocking(move || {
                    crate::tool::channel::follow_through(&session, &session.watch, &posted);
                });
            }
            // The terminal event still comes from the run, so a client that saw
            // `turn/started` is owed exactly one end whether it was interrupted
            // or not. This only asks.
            Run::Interrupt { .. } => {
                self.cancel.send_replace(true);
            }
        }
    }
}

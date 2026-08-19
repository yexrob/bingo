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

use std::sync::{Arc, Mutex};

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
    /// The foreground handle of the run in flight. There is at most one — phase
    /// 2 runs unsafe tools serially — so this is the one thing `Run::Promote`
    /// could possibly be about.
    live: Arc<Mutex<Option<Arc<crate::live::LiveBash>>>>,
}

impl SessionEngine {
    /// Build the engine for a session, on the runtime it will run its work on.
    ///
    /// `None` when there is no runtime to run on. That is not a failure to
    /// report — it is a caller that assembled a session without one, and a
    /// session with no engine is a session that can be read and configured but
    /// not run, which is a state the core already has an answer for.
    pub fn new(session: Arc<Session>) -> Option<Arc<Self>> {
        let runtime = tokio::runtime::Handle::try_current().ok()?;
        runtime.spawn(mirror_config(session.clone()));
        Some(Arc::new(Self {
            session,
            runtime,
            cancel: watch::channel(false).0,
            live: Arc::new(Mutex::new(None)),
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
        let session = self.session_now();
        let cancel = self.cancel.subscribe();
        self.cancel.send_replace(false);
        // The host is assembled here rather than inside the task so the
        // foreground handle it carries is reachable the moment the run exists:
        // ctrl+b arrives as `Run::Promote` and has nowhere else to look.
        let host = host_for(&session, turn.clone());
        self.hold(Some(host.requests.live.clone()));
        let held = self.live.clone();
        let turn_id = turn.clone();
        self.runtime.spawn(async move {
            let guard = session
                .turns
                .guard(turn, crate::app::snapshot::TurnStatus::Failed);
            let history = Self::history(&session, &mut host.events.warn_sink());
            let outcome = match work {
                Work::Prompt(text) => {
                    // The markers are the text's, and the registry they name is
                    // the session's — so a picture the user pasted reaches the
                    // model whichever frontend submitted the line.
                    let images = session.attachments.resolve(&text);
                    crate::query::run_query(&session, history, &text, &images, &host, Some(cancel))
                        .await
                }
                Work::Shell(command) => {
                    crate::query::run_bash_command(&session, &command, history, &host, Some(cancel))
                        .await
                }
            };
            *held.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
            // Before the close, not after: a report into a turn that has already
            // reached its terminal state is dropped, and this one is about the
            // turn rather than about what came after it.
            if let Ok(outcome) = &outcome
                && !outcome.aborted
                && outcome.end_reason == crate::query::QueryEndReason::EmptyResponseRetried
            {
                host.events
                    .warn("model returned an empty response and was retried");
            }
            // The interrupt marker is committed before the close, in the order
            // it happened: the exact sentence the transcript recorded becomes an
            // item, so the screen and the model read the same one.
            if let Ok(outcome) = &outcome
                && let Some(marker) = outcome.interrupt_marker
            {
                session
                    .turns
                    .commit_item(
                        crate::app::conversation::ConvKey::Main,
                        Some(turn_id.clone()),
                        crate::app::snapshot::ItemBody::Interruption {
                            marker: marker.to_string(),
                        },
                    )
                    .forget();
            }
            match &outcome {
                Ok(outcome) if outcome.aborted => {
                    guard.finish(crate::app::snapshot::TurnStatus::Interrupted, None)
                }
                Ok(_) => guard.finish(crate::app::snapshot::TurnStatus::Completed, None),
                Err(error) => guard.finish(
                    crate::app::snapshot::TurnStatus::Failed,
                    Some(crate::app::snapshot::TurnError {
                        code: crate::error::map_error(error).to_string(),
                        message: crate::error::sanitize_msg(&error.to_string()),
                    }),
                ),
            }
            // After the turn is closed, never before: the memory pass is a
            // non-streaming model call and must not hold up an ending a client is
            // waiting on. It used to be the console's alone, which meant a
            // session driven over the wire learned nothing from its own turns.
            if let Ok(outcome) = &outcome {
                let cwd = session.cwd();
                crate::memory::extract_memory(&session, &outcome.messages, &session.home, &cwd)
                    .await;
            }
        });
    }

    /// The session this run reads its permission mode from.
    ///
    /// A `Session` is immutable inside its `Arc`, and the permission mode moves
    /// while a session is alive — shift+tab, `/permission-mode`, a client's
    /// `action/execute`. The core is the one authority for what it stands at
    /// (D154), so a run that needs a different one takes a copy carrying it;
    /// every other field is a shared handle and still points at the same state.
    fn session_now(&self) -> Arc<crate::query::Session> {
        let wanted = match self.session.core.config().borrow().permission_mode {
            crate::app::snapshot::PermissionMode::Default => {
                crate::permission::PermissionMode::Default
            }
            crate::app::snapshot::PermissionMode::AcceptEdits => {
                crate::permission::PermissionMode::AcceptEdits
            }
            crate::app::snapshot::PermissionMode::BypassPermissions => {
                crate::permission::PermissionMode::BypassPermissions
            }
            crate::app::snapshot::PermissionMode::DontAsk => {
                crate::permission::PermissionMode::DontAsk
            }
            crate::app::snapshot::PermissionMode::Plan => crate::permission::PermissionMode::Plan,
        };
        if wanted == self.session.permission_mode {
            return self.session.clone();
        }
        let mut session = (*self.session).clone();
        session.permission_mode = wanted;
        Arc::new(session)
    }

    /// Remember the run's foreground handle, or forget it.
    fn hold(&self, live: Option<Arc<crate::live::LiveBash>>) {
        *self.live.lock().unwrap_or_else(|held| held.into_inner()) = live;
    }

    /// The run in flight, as `Run::Promote` reaches it.
    fn foreground(&self) -> Option<Arc<crate::live::LiveBash>> {
        self.live
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
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

/// Keep what a run reads in step with what the core holds.
///
/// The model, the provider and the thinking level are the core's — an
/// `action/execute`, a slash line, or a `/model` drained off the queue all change
/// them there (D154). What a run *reads* is the session's runtime, so one task
/// carries the core's answer across rather than every caller remembering to
/// write both. It ends when the session does, because the sender does.
async fn mirror_config(session: Arc<Session>) {
    let mut config = session.core.config();
    // What moved, not what differs: a session assembles its runtime and its core
    // from the same settings, and anything else that revises the configuration —
    // an MCP report, a permission rule — must not carry a stale selection across
    // with it.
    let mut held = config.borrow().clone();
    while config.changed().await.is_ok() {
        let now = config.borrow_and_update().clone();
        if now.model != held.model {
            let _ = session.runtime.model_tx.send(now.model.clone());
        }
        if now.thinking != held.thinking {
            let _ = session.runtime.thinking_tx.send(match now.thinking {
                crate::app::snapshot::ThinkingLevel::Off => None,
                level => Some(level.as_str().to_string()),
            });
        }
        if now.provider != held.provider && session.client.set_provider(&now.provider).is_ok() {
            let _ = session.runtime.provider_tx.send(now.provider.clone());
        }
        held = now;
    }
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
            // Which item is in the foreground was settled before this was sent;
            // what is left is letting go of the command, and only the run that
            // armed it can.
            Run::Promote { .. } => {
                if let Some(live) = self.foreground() {
                    live.promote();
                }
            }
            Run::Act(act) => self.act(*act),
        }
    }
}

impl SessionEngine {
    /// One action's work, from the operation the core opened to the item it
    /// leaves in the page it was asked on.
    fn act(&self, act: crate::app::engine::Act) {
        let session = self.session.clone();
        self.runtime.spawn(async move {
            crate::engine::actions::perform(&session, act).await;
        });
    }
}

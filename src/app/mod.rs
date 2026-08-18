//! The application core: what it accepts, what it publishes, and how to reach it.
//!
//! [`AppCore`] owns application truth and ordering; the TUI and `bingo
//! app-server` are two projections of it. The modules below are the vocabulary
//! they share: the identifiers the core mints ([`ids`]), the mutations it
//! accepts ([`command`]), the events it publishes ([`event`]), and the snapshots
//! it hands out ([`snapshot`]). [`controller`] is the single actor that turns
//! one into the others.
//!
//! B2a landed the actor's skeleton: attachment, sequencing, the snapshot cut,
//! and the identifier mint. B2b gave it its first state — the three
//! collaboration registries ([`crate::watch`], [`crate::channels`],
//! [`crate::agents`]), which stopped being `Arc<Mutex<…>>` tables anyone could
//! reach and became this loop's own. Conversations, turns, and items arrive with
//! B3; a registry's place in a snapshot, and the attention cursors that go with
//! it, with B4.
//!
//! Design: `notes/design/gui-app-server.md` (with its amendments) and
//! `notes/design/gui-app-server-plan.md`.
// The core is reachable before it is reached: the TUI attaches in B7 and the
// stdio transport in B6. Remove this allow when they arrive.
#![allow(dead_code)]

pub mod action;
pub mod answer;
pub mod asset;
pub mod attention;
pub mod catalog;
#[cfg(test)]
mod collab_tests;
pub mod command;
pub mod controller;
pub mod conversation;
pub mod event;
pub mod ids;
pub mod interaction;
pub mod mail;
pub mod operation;
pub mod projection;
pub mod queue;
pub mod roomlog;
pub mod snapshot;
pub mod submit;
pub mod turn;

use std::path::PathBuf;

use tokio::sync::{mpsc, oneshot};

use crate::app::command::{AppCommand, AppQuery};
use crate::app::event::AppEvent;
use crate::app::ids::OperationId;
use crate::app::snapshot::{
    ConversationSnapshot, PermissionMode, ServerCapabilities, SessionLocator, SessionSnapshot,
    ShellDialect, ThemeChoice, ThinkingLevel,
};
use crate::app_server::protocol::error::ProtocolErrorKind;

/// How many frames an attachment may fall behind by before the core stops
/// carrying it. The core is the one ordering point in the process: it must never
/// block on a frontend, so a frontend that stops reading loses its attachment
/// and has to attach and read again. The transport's own backpressure and its
/// `CLIENT_TOO_SLOW` notice are B6's (spec "Errors, load, and security").
const FRAME_CAPACITY: usize = 1024;

/// How many unserved requests one attachment may have in flight.
const REQUEST_CAPACITY: usize = 64;

/// What a frontend says about itself when it attaches. It buys one ordered frame
/// channel and one request channel, nothing else: an attachment is a view, never
/// a second owner of state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachRequest {
    /// What this attachment is, for diagnostics: `tui`, `app-server`, `print`.
    pub label: String,
}

impl AttachRequest {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
        }
    }
}

/// One attached frontend's two channels.
///
/// Everything the core says arrives on `frames`, in the order it decided it:
/// replies, snapshots, and events on one channel rather than several, because
/// two channels cannot state which came first.
pub struct AppLink {
    pub requests: mpsc::Sender<AppRequest>,
    pub frames: mpsc::Receiver<AppFrame>,
}

/// A caller-chosen correlation number, echoed on the reply frame. The wire's
/// JSON-RPC id is the transport's own business (B6); this is how the core tells
/// two in-flight requests from the same attachment apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RequestId(pub u64);

/// What an attachment asks the core to do or to read.
#[derive(Debug, Clone, PartialEq)]
pub enum AppRequest {
    Command { id: RequestId, command: AppCommand },
    Query { id: RequestId, query: AppQuery },
}

/// What the core says, in the order it decided it.
#[derive(Debug, Clone, PartialEq)]
pub enum AppFrame {
    /// The answer to one request, written before any event that request caused
    /// (spec invariant #3).
    Reply {
        id: RequestId,
        result: Result<AppReply, AppError>,
    },
    /// Boxed because it is the common frame, not despite it: an event is far
    /// larger than a reply, and every attachment gets its own copy.
    Event(Box<AppEvent>),
}

/// The answer to an accepted request.
#[derive(Debug, Clone, PartialEq)]
pub enum AppReply {
    /// A mutation the core took. What it produced arrives as events.
    Accepted,
    /// A session cut, valid through its `event_cursor`.
    Session(Box<SessionSnapshot>),
    /// A conversation cut, valid through its `event_cursor`.
    Conversation(Box<ConversationSnapshot>),
    /// One page of the session's conversations.
    Conversations(crate::app::snapshot::Page<crate::app::snapshot::ConversationSummary>),
    /// What the core did with a submission. The caller never chose it.
    Submitted(crate::app::command::SubmitDisposition),
    /// One page of the sessions on disk.
    Sessions(crate::app::snapshot::Page<crate::app::snapshot::SessionListEntry>),
    /// One page of a conversation's queue, and how much is on it.
    Queue {
        entries: crate::app::snapshot::Page<crate::app::snapshot::QueueEntry>,
        count: u32,
    },
    /// What every action is called, and which of them can run right now.
    Actions {
        actions: Vec<crate::app::command::ActionInfo>,
        revision: u64,
    },
    /// The effective configuration.
    Config(Box<crate::app::snapshot::ConfigSnapshot>),
    /// One page of one catalog.
    Catalog(Box<crate::app::snapshot::Catalog>),
    /// One page of one runtime collection.
    Resource(Box<crate::app::snapshot::ResourcePage>),
    /// A registered asset.
    Asset(Box<crate::app::snapshot::AssetRecord>),
    /// One bounded chunk of an asset's bytes, base64.
    AssetChunk {
        data: String,
        next_offset: u64,
        eof: bool,
    },
    /// A turn was asked to stop. `accepted` is false when it had already ended.
    Interrupted {
        turn_id: crate::app::ids::TurnId,
        accepted: bool,
    },
    /// A prompt was answered, with the ordered item the resolution committed.
    Responded {
        item_id: Option<crate::app::ids::ItemId>,
    },
    /// What the pull-back found, and where the queue stands after it.
    Reclaimed {
        outcome: Box<crate::app::queue::Reclaim>,
        revision: u64,
    },
    /// A persisted session was asked for by name and is gone.
    Deleted {
        locator: SessionLocator,
        deleted: bool,
    },
}

/// Why the core did not do it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AppError {
    /// The core is gone: it stopped, or the process is shutting down. Nothing
    /// was mutated and nothing will be.
    #[error("the application core is not running")]
    Stopped,
    /// A well-formed request the core refused on its state or its arguments.
    #[error("{}", .0.message())]
    Refused(ProtocolErrorKind),
    /// The core has no handler for this yet. The skeleton says so rather than
    /// answering out of state it does not hold.
    // B3-B5 land the handlers; this variant leaves with the last of them.
    #[error("{0} is not served by this build yet")]
    Unserved(&'static str),
}

/// The session metadata the core is started with.
#[derive(Debug, Clone)]
pub struct SessionSetup {
    pub title: String,
    pub cwd: PathBuf,
    pub locator: SessionLocator,
    pub provider: String,
    pub model: String,
    pub thinking: ThinkingLevel,
    pub permission_mode: PermissionMode,
    pub theme: ThemeChoice,
    pub shell: String,
    pub shell_dialect: ShellDialect,
    /// The session resumed a persisted transcript rather than starting empty.
    pub resumed: bool,
    /// Room budgets, which come from settings and are the registry's from the
    /// moment the actor starts.
    pub channel_limits: crate::channels::ChannelLimits,
    pub capabilities: ServerCapabilities,
    /// Settings, the two directories, and the endpoint table: what the catalogs
    /// and the effective configuration are read from (B5).
    pub catalog: crate::app::catalog::CatalogSource,
}

impl Default for SessionSetup {
    fn default() -> Self {
        Self {
            title: String::new(),
            cwd: PathBuf::new(),
            locator: SessionLocator::Latest,
            provider: String::new(),
            model: String::new(),
            thinking: ThinkingLevel::Off,
            permission_mode: PermissionMode::Default,
            theme: ThemeChoice::Auto,
            shell: String::new(),
            shell_dialect: ShellDialect::Unknown,
            resumed: false,
            channel_limits: crate::channels::ChannelLimits::default(),
            catalog: crate::app::catalog::CatalogSource::default(),
            capabilities: ServerCapabilities {
                multi_conversation: true,
                reasoning: true,
                images: true,
                teams: true,
                rooms: true,
                shell: true,
            },
        }
    }
}

/// A handle on the running session actor.
///
/// The actor is the process's one ordering point: every mutation and every
/// sequence number happens inside it, and everything else — provider streams,
/// tool runs, agent loops — re-enters it before changing state. Dropping the
/// last handle and every link stops it.
#[derive(Clone)]
pub struct AppCore {
    control: mpsc::UnboundedSender<controller::Control>,
    /// The registries the actor owns, as the rest of the process reaches them.
    /// Each is a handle on the same inbox — cloning one keeps the actor alive,
    /// which is why a session outlives the `AppCore` value it was started from.
    watch: crate::watch::WatchHandle,
    channels: crate::channels::ChannelHandle,
    agents: crate::agents::AgentHandle,
    turns: crate::app::turn::TurnHandle,
    queue: crate::app::queue::QueueHandle,
    submit: crate::app::submit::SubmitHandle,
    interactions: crate::app::interaction::InteractionHandle,
    mail: crate::app::mail::MailHandle,
    operations: crate::app::operation::OperationHandle,
    /// Whether the actor's loop is still running. Weak on purpose: holding it
    /// must not be what keeps a session alive.
    alive: controller::Alive,
}

impl AppCore {
    /// Start the session actor.
    pub fn start(setup: SessionSetup) -> Self {
        let (control, registries, alive) = controller::spawn(setup);
        Self {
            control,
            watch: registries.watch,
            channels: registries.channels,
            agents: registries.agents,
            turns: registries.turns,
            queue: registries.queue,
            submit: registries.submit,
            interactions: registries.interactions,
            mail: registries.mail,
            operations: registries.operations,
            alive,
        }
    }

    /// Background work: commands, agent runs, room operations.
    pub fn watch(&self) -> crate::watch::WatchHandle {
        self.watch.clone()
    }

    /// The rooms, and the main agent's inbox.
    pub fn channels(&self) -> crate::channels::ChannelHandle {
        self.channels.clone()
    }

    /// The subagent instances.
    pub fn agents(&self) -> crate::agents::AgentHandle {
        self.agents.clone()
    }

    /// The turns in flight.
    pub fn turns(&self) -> crate::app::turn::TurnHandle {
        self.turns.clone()
    }

    /// The input queues.
    pub fn queue(&self) -> crate::app::queue::QueueHandle {
        self.queue.clone()
    }

    /// The one submission path.
    pub fn submit(&self) -> crate::app::submit::SubmitHandle {
        self.submit.clone()
    }

    /// The prompts a run is stopped on.
    pub fn interactions(&self) -> crate::app::interaction::InteractionHandle {
        self.interactions.clone()
    }

    /// The mail waiting for main, and whether it is time to read it.
    pub fn mail(&self) -> crate::app::mail::MailHandle {
        self.mail.clone()
    }

    /// The asynchronous work that is not a turn.
    pub fn operations(&self) -> crate::app::operation::OperationHandle {
        self.operations.clone()
    }

    /// Tell the core what the MCP manager stands at. Connection state belongs to
    /// the manager, which lives outside the actor; the catalogs answer
    /// "configured, not connected" until something says otherwise.
    pub fn report_mcp(&self, states: Vec<crate::app::snapshot::McpServerState>) {
        let _ = self.control.send(controller::Control::Mcp(states));
    }

    /// Whether the session actor's loop is still running.
    pub fn is_running(&self) -> bool {
        self.alive.upgrade().is_some()
    }

    /// Close the session and wait for the actor to finish settling.
    ///
    /// Everything open reaches a terminal state, the instances are released, and
    /// the loop ends. What is left of the session after this is the handles the
    /// process still holds — and they answer with their shutdown values rather
    /// than blocking, because there is nobody left to answer them.
    pub async fn close(&self) {
        let (reply, done) = oneshot::channel();
        if self
            .control
            .send(controller::Control::Close {
                reason: crate::app::snapshot::SessionCloseReason::Requested,
                reply,
            })
            .is_ok()
        {
            let _ = done.await;
        }
    }

    /// Attach a frontend. The attachment sees no event until it takes a
    /// snapshot cut: everything before the cut is in the snapshot, so replaying
    /// it would be telling the same fact twice (spec "Architecture").
    pub async fn attach(&self, request: AttachRequest) -> Result<AppLink, AppError> {
        let (reply, answer) = oneshot::channel();
        self.control
            .send(controller::Control::Attach {
                request,
                runtime: tokio::runtime::Handle::current(),
                reply,
            })
            .map_err(|_| AppError::Stopped)?;
        answer.await.map_err(|_| AppError::Stopped)?
    }

    /// The ingress engine work publishes through. A shim while the engine still
    /// runs outside the actor: B3 removes this as `EngineEvent` becomes the
    /// actor's own inbox. The registries no longer need it — they are the
    /// actor's state now, and the events they cause are stamped where they
    /// happen.
    pub fn publisher(&self) -> AppPublisher {
        AppPublisher {
            control: self.control.clone(),
        }
    }
}

/// The one way state changes from outside a request: an engine task hands the
/// actor what happened and the actor decides when it happened.
///
/// B2b and B3 feed this from `EngineEvent`, which is why nothing here takes a
/// sequence number or a timestamp — those are the actor's to stamp.
#[derive(Debug, Clone)]
pub struct AppPublisher {
    control: mpsc::UnboundedSender<controller::Control>,
}

impl AppPublisher {
    pub fn publish(
        &self,
        payload: crate::app::event::AppEventPayload,
        caused_by: Option<OperationId>,
    ) -> Result<(), AppError> {
        self.control
            .send(controller::Control::Publish {
                payload: Box::new(payload),
                caused_by,
            })
            .map_err(|_| AppError::Stopped)
    }
}

impl AppLink {
    /// The next thing the core said, or `None` once it has nothing more to say.
    pub async fn recv(&mut self) -> Option<AppFrame> {
        self.frames.recv().await
    }

    pub async fn request(&self, request: AppRequest) -> Result<(), AppError> {
        self.requests
            .send(request)
            .await
            .map_err(|_| AppError::Stopped)
    }
}

//! The application core: what it accepts, what it publishes, and how to reach it.
//!
//! [`AppCore`] owns application truth and ordering; the TUI and `bingo
//! app-server` are two projections of it. The modules below are the vocabulary
//! they share: the identifiers the core mints ([`ids`]), the mutations it
//! accepts ([`command`]), the events it publishes ([`event`]), and the snapshots
//! it hands out ([`snapshot`]). [`controller`] is the single actor that turns
//! one into the others.
//!
//! B2a lands the actor's skeleton: attachment, sequencing, the snapshot cut, and
//! the identifier mint. The state it sequences is still a session's own metadata
//! and empty collections — conversations, turns, and items arrive with B3, the
//! collaboration registries with B2b.
//!
//! Design: `notes/design/gui-app-server.md` (with its amendments) and
//! `notes/design/gui-app-server-plan.md`.
// The core is reachable before it is reached: the TUI attaches in B7 and the
// stdio transport in B6. Remove this allow when they arrive.
#![allow(dead_code)]

pub mod command;
pub mod controller;
pub mod event;
pub mod ids;
pub mod snapshot;

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
///
/// The skeleton is handed what it reports; B3 fills this from the real session
/// and its settings, which is also when the empty collections below stop being
/// empty.
#[derive(Debug, Clone, PartialEq)]
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
    pub capabilities: ServerCapabilities,
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
#[derive(Debug, Clone)]
pub struct AppCore {
    control: mpsc::Sender<controller::Control>,
}

impl AppCore {
    /// Start the session actor on the current runtime.
    pub fn start(setup: SessionSetup) -> Self {
        Self {
            control: controller::spawn(setup),
        }
    }

    /// Attach a frontend. The attachment sees no event until it takes a
    /// snapshot cut: everything before the cut is in the snapshot, so replaying
    /// it would be telling the same fact twice (spec "Architecture").
    pub async fn attach(&self, request: AttachRequest) -> Result<AppLink, AppError> {
        let (reply, answer) = oneshot::channel();
        self.control
            .send(controller::Control::Attach { request, reply })
            .await
            .map_err(|_| AppError::Stopped)?;
        answer.await.map_err(|_| AppError::Stopped)?
    }

    /// The ingress engine work publishes through.
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
    control: mpsc::Sender<controller::Control>,
}

impl AppPublisher {
    pub async fn publish(
        &self,
        payload: crate::app::event::AppEventPayload,
        caused_by: Option<OperationId>,
    ) -> Result<(), AppError> {
        self.control
            .send(controller::Control::Publish {
                payload: Box::new(payload),
                caused_by,
            })
            .await
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

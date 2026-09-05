//! The client contract. In-process surfaces call these traits directly; the
//! JSON-RPC surface exposes them one-to-one. Writes are synchronous and
//! return nothing; outcomes arrive as `IntentAck` frames.

use std::any::Any;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::Stream;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ErrorCode, KernelError};
use crate::event::*;
use crate::ids::{IntentId, InteractionId, ItemId, Seq, SessionId, TurnId};
use crate::model::{Effort, Image};
use crate::service::WireService;
use crate::state::SessionState;
use crate::tool::{ToolCall, ToolOutcome};

pub type FrameStream = Pin<Box<dyn Stream<Item = Frame> + Send>>;
pub type GatewayStream = Pin<Box<dyn Stream<Item = GatewayEvent> + Send>>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClientIdentity {
    pub name: String,
    pub surface: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSpec {
    pub cwd: PathBuf,
    /// Routing key, `owner/path`, unique across the store; the first segment is the minting plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ParentLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// `Log` needs no provider or model: nothing answers (ADR-0011 §1).
    #[serde(default)]
    pub driver: Driver,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_extra: Option<String>,
    /// Restrict the tool set by name; `None` means every registered tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// How hard the session thinks (ADR-0047 §1). Absent inherits — the
    /// parent's level as it stands, else the settings'; `null` is off;
    /// otherwise the level. The host resolves it once, where it resolves the
    /// model, and the spec is the one holder from then on.
    #[serde(
        default,
        deserialize_with = "read_thinking",
        skip_serializing_if = "Option::is_none"
    )]
    pub thinking: Option<Option<Effort>>,
}

/// `null` is a level said, not a level unsaid: without this serde folds it
/// into the absent case and a spec could never ask for thinking off.
fn read_thinking<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<Effort>>, D::Error> {
    Option::<Effort>::deserialize(deserializer).map(Some)
}

/// What may be changed about a running session between turns (ADR-0047 §3).
/// It lands on the next turn: the running one keeps the config it started on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionChange {
    Model {
        /// Stays as it was when absent.
        provider: Option<String>,
        model: String,
    },
    Thinking(Option<Effort>),
    /// What the session is called from now on.
    Title(String),
}

/// What an attachment carries beyond the session itself (ADR-0010 §3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct OpenOptions {
    /// The frames of every live descendant too, each stamped with its own
    /// `session`; the handle answers an interaction wherever in the tree it
    /// was opened.
    pub children: bool,
}

impl OpenOptions {
    pub fn with_children() -> Self {
        Self { children: true }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SessionSelector {
    Create { spec: SessionSpec },
    ById { id: SessionId },
    ByKey { key: String },
    Latest { cwd: PathBuf },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

/// A typed action a client asks for (GUI buttons, hosts); the kernel dispatches it to a command.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Action {
    pub name: String,
    #[serde(default)]
    pub args: Value,
}

/// The one submission entry. The kernel parses `/`, `!` and `@` in text and
/// decides turn, queue, steer or deliver; a client never chooses.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Input {
    Text {
        text: String,
        /// The pictures the ask carries, in the order they reach the journal
        /// (ADR-0040): exactly as they will be journaled, because a surface
        /// resolves a picture and the kernel does no file I/O for input.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<Image>,
        origin: Origin,
        /// Whether a running turn may be steered with it (ADR-0008 §2,
        /// amended M68). `Wake` is the line every client has always sent, so
        /// it is the default and nothing is written for it.
        #[serde(default, skip_serializing_if = "Delivery::is_wake")]
        delivery: Delivery,
    },
    Action {
        action: Action,
    },
}

impl Input {
    pub fn text(text: impl Into<String>, origin: Origin) -> Self {
        Input::Text {
            text: text.into(),
            images: Vec::new(),
            origin,
            delivery: Delivery::Wake,
        }
    }

    /// Whether the line asked to wait for the running turn to end rather than
    /// steer it. A barrier absorbs the lines that steer and leaves this one
    /// to open the next turn.
    pub fn is_held(&self) -> bool {
        matches!(
            self,
            Input::Text {
                delivery: Delivery::Hold,
                ..
            }
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum InterruptScope {
    Turn {
        turn: TurnId,
    },
    /// Whatever is running now.
    Head,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<ItemId>,
    #[serde(default)]
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryChunk {
    pub items: Vec<Item>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<ItemId>,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CatalogKind {
    Models,
    Providers,
    Tools,
    Commands,
    Skills,
    Plugins,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub meta: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub kind: CatalogKind,
    pub entries: Vec<CatalogEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum GatewayEvent {
    SessionCreated { summary: Box<SessionSummary> },
    SessionRemoved { session: SessionId },
    CatalogChanged { kind: CatalogKind },
}

/// Ask a person something through the interaction registry.
#[async_trait]
pub trait Prompter: Send + Sync {
    async fn ask(
        &self,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError>;
}

/// The actor's mailbox as a client sees it.
#[async_trait]
pub trait SessionPort: Send + Sync {
    fn submit(&self, intent: IntentId, input: Input);
    fn interrupt(&self, intent: IntentId, scope: InterruptScope);
    fn answer(
        &self,
        intent: IntentId,
        interaction: InteractionId,
        answer: Answer,
        activation: Activation,
    );
    async fn history(&self, page: HistoryPage) -> Result<HistoryChunk, KernelError>;
    /// Frames with `seq > since`, then live.
    async fn events_since(&self, since: Seq) -> Result<FrameStream, KernelError>;
}

#[derive(Clone)]
pub struct SessionHandle(pub Arc<dyn SessionPort>);

impl std::fmt::Debug for SessionHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionHandle")
    }
}

impl SessionHandle {
    pub fn submit(&self, intent: IntentId, input: Input) {
        self.0.submit(intent, input)
    }

    pub fn interrupt(&self, intent: IntentId, scope: InterruptScope) {
        self.0.interrupt(intent, scope)
    }

    pub fn answer(
        &self,
        intent: IntentId,
        interaction: InteractionId,
        answer: Answer,
        activation: Activation,
    ) {
        self.0.answer(intent, interaction, answer, activation)
    }

    pub async fn history(&self, page: HistoryPage) -> Result<HistoryChunk, KernelError> {
        self.0.history(page).await
    }

    pub async fn events_since(&self, since: Seq) -> Result<FrameStream, KernelError> {
        self.0.events_since(since).await
    }
}

/// What `open` returns: a snapshot cut and every frame after it.
pub struct Attachment {
    pub session: SessionId,
    pub snapshot: SessionState,
    pub events: FrameStream,
    pub handle: SessionHandle,
}

impl std::fmt::Debug for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Attachment")
            .field("session", &self.session)
            .field("seq", &self.snapshot.seq)
            .finish_non_exhaustive()
    }
}

/// How a line reaches a session's queue: a peer's message (ADR-0010 §1) and
/// a person's own line are the same question, so they are the same word.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Delivery {
    /// An idle target opens a turn on it; a busy one absorbs it at the next barrier.
    #[default]
    Wake,
    /// It waits in the queue for whatever opens the next turn.
    Hold,
}

impl Delivery {
    /// The default, which is written nowhere: a frame from before there was a
    /// field for it reads as `Wake` and goes back on the wire byte-identical.
    fn is_wake(&self) -> bool {
        matches!(self, Delivery::Wake)
    }
}

#[async_trait]
pub trait HostApi: Send + Sync {
    async fn sessions(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError>;

    async fn open(
        &self,
        selector: SessionSelector,
        who: ClientIdentity,
        options: OpenOptions,
    ) -> Result<Attachment, KernelError>;

    /// Detach this client; the session keeps running.
    async fn close(&self, session: &SessionId, reason: CloseReason) -> Result<(), KernelError>;

    async fn delete(&self, session: &SessionId) -> Result<(), KernelError>;

    /// The peer-messaging primitive (ADR-0010 §1, ADR-0011 §3): the target's
    /// queue is its inbox. A target that is persisted but not live is
    /// reopened first; the outcome is the target's ack.
    async fn deliver(
        &self,
        to: &SessionId,
        intent: IntentId,
        input: Input,
        delivery: Delivery,
    ) -> Result<(), KernelError>;

    /// Publish a plugin's state into a session's journal (ADR-0011 §2): a
    /// durable `Event::Extension` whose payload is the whole of `kind`.
    async fn extend(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError>;

    /// Publish a plugin's live state onto a session's stream (ADR-0013 §2):
    /// an ephemeral `Event::Signal`, never journaled; `Null` removes `kind`.
    async fn signal(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError>;

    /// Hand a tool call to a session's running turn and wait for what it comes
    /// to (ADR-0036 §2). The turn's own machinery serves it: the same gate, a
    /// real tool item journaled under the turn, a cancel token that is a child
    /// of the turn's — so one `esc` drops the call where it stands and the
    /// answer says so.
    ///
    /// It is served while the turn's stream is open, because whoever handed
    /// the call in is blocked on the answer before it can go on. The outcome
    /// goes back here and nowhere else: it never joins the provider's
    /// messages, because the caller already holds it and a copy would be a
    /// second representation of one call.
    ///
    /// Refused, fail closed, when no turn is in flight, and when the call
    /// names a tool the running turn was not given — the turn's own offer is
    /// the whole of what may be called.
    async fn invoke(
        &self,
        _session: &SessionId,
        _call: ToolCall,
    ) -> Result<ToolOutcome, KernelError> {
        Err(KernelError::new(
            ErrorCode::Internal,
            "this host runs no turns",
        ))
    }

    /// Put a question to whoever is at this session (ADR-0039 §1): one door
    /// onto the interaction machinery the gate already asks through, for a
    /// question no tool defines. `answers` are the answers the kernel will
    /// take, as a gate question states them.
    ///
    /// The session answers first, through its policy's one `stance`: a
    /// session that lets everything happen answers the question's allowing
    /// option and a session with nobody at it answers its refusing one —
    /// both at once, with no interaction opened and nothing journaled, as a
    /// call the gate allows leaves no receipt. Otherwise the interaction is
    /// opened and whatever surface is attached renders it, exactly as it
    /// renders a gate question; the person's answer comes back here.
    ///
    /// Unlike `invoke`, a question need not arrive mid-turn: an interaction
    /// is the session's, not a turn's, so an asker between turns is served.
    ///
    /// A question that could not be put to anybody after all — the turn was
    /// interrupted under it, the session closed — comes back as the refusing
    /// option rather than as an error, so no caller has to read a refusal out
    /// of one. A question that names no option for the role its session needs
    /// is refused outright, and so is a session this host does not run: the
    /// door never answers "allowed" for a question nobody could have asked.
    ///
    /// Offer `Cancel` (or `Deny`) among the `answers`: it is how a surface
    /// with nobody at the keyboard declines what it was handed, and a
    /// question no surface can decline is a question that waits — nothing
    /// here expires.
    ///
    /// It does not join the JSON-RPC wire.
    async fn ask(
        &self,
        _session: &SessionId,
        _kind: InteractionKind,
        _answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        Err(KernelError::new(
            ErrorCode::Internal,
            "this host runs no sessions",
        ))
    }

    /// Undo a turn and everything after it (ADR-0045 §1). The kernel appends
    /// `ItemBody::Rewind { to_turn, dropped }` as its own item and publishes
    /// `Event::Rewound`, so the items from that turn's first onward leave the
    /// model's view (`ContextView::items`) and every client's fold
    /// (`SessionState::apply`) at once. The journal is never rewritten: what
    /// happened stays, and the item that undid it is the record of the undoing
    /// (ADR-0002 §3).
    ///
    /// Answers how many items were dropped. Refused while a turn is running —
    /// a rewind under a turn would cut the ground from under it, and a child
    /// agent runs inside its parent's turn — and refused for a turn this
    /// session does not have.
    ///
    /// The files a turn wrote are nobody's here: the kernel snapshots nothing
    /// and restores nothing, which is why the verb takes no paths. Whoever put
    /// the files back says so in its own reply.
    ///
    /// It does not join the JSON-RPC wire: a client rewinds by submitting the
    /// command that owns the snapshots.
    async fn rewind(&self, _session: &SessionId, _to_turn: &TurnId) -> Result<u32, KernelError> {
        Err(KernelError::new(
            ErrorCode::Internal,
            "this host keeps no journal",
        ))
    }

    /// Take a queued line back out of a session's queue and hand it to
    /// whoever put it there (ADR-0008 §2, amended M68). A line that waits is
    /// still the person's: this is how it reaches an editor again.
    ///
    /// The entry must still be queued — one a turn has already taken is
    /// `NOT_FOUND`, and so is an intent this session never held — and it must
    /// be the caller's own: `who.surface` is the surface that submitted it,
    /// and another surface's line is `PERMISSION_DENIED`. `QueueChanged`
    /// follows, so every client's fold loses the row at once.
    ///
    /// It does not join the JSON-RPC wire.
    async fn withdraw(
        &self,
        _session: &SessionId,
        _intent: &IntentId,
        _who: ClientIdentity,
    ) -> Result<Input, KernelError> {
        Err(KernelError::new(
            ErrorCode::Internal,
            "this host keeps no queue",
        ))
    }

    /// Move one of a session's knobs (ADR-0047 §3): the model it runs on,
    /// how hard it thinks, what it is called. The host re-resolves the model
    /// and hands the actor the config its next turn runs on.
    ///
    /// It answers nothing. What the next turn will actually ask for is read
    /// back — a level a model does not reason at reaches no request — and
    /// every client learns of the change from the `SessionUpdated` and
    /// `ConfigChanged` that follow.
    ///
    /// Between turns, never inside one: a turn already running keeps the
    /// config it started with, so a change made mid-turn lands on the next.
    ///
    /// It joins neither the JSON-RPC wire — a client has the commands — nor
    /// the plugin bridge.
    async fn reconfigure(
        &self,
        _session: &SessionId,
        _change: SessionChange,
    ) -> Result<(), KernelError> {
        Err(KernelError::new(
            ErrorCode::Internal,
            "this host runs no sessions",
        ))
    }

    async fn catalog(&self, kind: CatalogKind) -> Result<Catalog, KernelError>;

    /// Say one line to the person, wherever they are: a transcript notice on
    /// every session that is open right now. It belongs to no session and no
    /// call — a plugin process that died, a hook that never decided, a plugin
    /// saying something of its own — which is why it names none.
    ///
    /// A host with nobody listening refuses, so a caller can keep the line and
    /// say it when somebody is there to read it.
    async fn notice(&self, _level: Level, _code: &str, _text: &str) -> Result<(), KernelError> {
        Err(KernelError::new(
            ErrorCode::Internal,
            "this host has nobody to say it to",
        ))
    }

    fn gateway_events(&self) -> GatewayStream;

    fn service_any(&self, key: &str) -> Option<Arc<dyn Any + Send + Sync>>;

    /// The other face of the same entry: what a process's `service/call` is
    /// served by, when the service's owner opened one (ADR-0031 §3). A
    /// service with no wire face does not exist across a process line, and a
    /// host that keeps no services has none at all.
    fn service_wire(&self, _key: &str) -> Option<Arc<dyn WireService>> {
        None
    }

    /// Put a service in the registry that could not be there when the plugins
    /// registered — one an external process declared, which nothing knows
    /// until its handshake has answered (ADR-0009 §1, ADR-0031 §4). The two
    /// faces are built from the one object: `service::<ServiceHandle>(key)`
    /// reaches it from in here, `service/call` from another process. A key
    /// that is taken stays its first owner's, and says so.
    fn open_service(&self, key: &str, _wire: Arc<dyn WireService>) -> Result<(), KernelError> {
        Err(KernelError::new(
            ErrorCode::Internal,
            format!("this host keeps no services: {key}"),
        ))
    }
}

#[derive(Clone)]
pub struct HostHandle(pub Arc<dyn HostApi>);

impl std::fmt::Debug for HostHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostHandle")
    }
}

impl HostHandle {
    pub fn service<T: Any + Send + Sync>(&self, key: &str) -> Option<Arc<T>> {
        self.0.service_any(key).and_then(|v| v.downcast::<T>().ok())
    }
}

impl std::ops::Deref for HostHandle {
    type Target = dyn HostApi;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The field is new (M68) and the line that never asks for it is the line
    /// every client has always sent: it reads without one and writes none.
    #[test]
    fn a_line_that_says_nothing_about_delivery_is_on_the_wire_as_it_was() {
        let bare = serde_json::json!({
            "kind": "text",
            "text": "fix the docs",
            "origin": { "surface": "tui" }
        });
        let input: Input = serde_json::from_value(bare.clone()).expect("an old line");
        assert_eq!(input, Input::text("fix the docs", Origin::surface("tui")));
        assert!(!input.is_held(), "a line with no word for it steers");
        assert_eq!(serde_json::to_value(&input).expect("json"), bare);
    }

    /// A line that asked to wait says so, and says it in one place.
    #[test]
    fn a_held_line_carries_the_word_and_answers_to_it() {
        let held = Input::Text {
            text: "and then the docs".into(),
            images: Vec::new(),
            origin: Origin::surface("tui"),
            delivery: Delivery::Hold,
        };
        assert!(held.is_held());
        assert_eq!(
            serde_json::to_value(&held).expect("json"),
            serde_json::json!({
                "kind": "text",
                "text": "and then the docs",
                "origin": { "surface": "tui" },
                "delivery": "hold"
            })
        );
        let read: Input = serde_json::from_value(serde_json::to_value(&held).expect("json"))
            .expect("a held line");
        assert_eq!(read, held);
    }

    /// Three answers, not two (ADR-0047 §1): a spec that says nothing about
    /// thinking inherits, one that says `null` is off, one that names a level
    /// asks for it. Serde folds `null` into absent unless told otherwise, so
    /// the middle case is the one this pins.
    #[test]
    fn a_spec_tells_inheriting_a_level_apart_from_asking_for_none() {
        let read = |value: serde_json::Value| -> SessionSpec {
            serde_json::from_value(value).expect("a spec")
        };
        // `driver` is written whatever it is, so a round trip carries it.
        let cwd = serde_json::json!({ "cwd": "/work", "driver": "model" });
        assert_eq!(read(cwd.clone()).thinking, None, "absent inherits");

        let mut off = cwd.clone();
        off["thinking"] = Value::Null;
        assert_eq!(read(off.clone()).thinking, Some(None), "null is off");

        let mut low = cwd.clone();
        low["thinking"] = Value::from("low");
        assert_eq!(read(low.clone()).thinking, Some(Some(Effort::Low)));

        for value in [cwd, off, low] {
            let spec = read(value.clone());
            assert_eq!(serde_json::to_value(&spec).expect("json"), value);
        }
    }

    /// A spec written before the field is a spec that inherits: an older
    /// journal's head frame reads without one and goes back on the wire as
    /// it came.
    #[test]
    fn a_spec_from_before_the_field_reads_as_absent() {
        let before = serde_json::json!({
            "cwd": "/work",
            "key": "agent/ses_1/reviewer",
            "title": "reviewer",
            "driver": "model",
            "model": "m",
        });
        let spec: SessionSpec = serde_json::from_value(before.clone()).expect("an old spec");
        assert_eq!(spec.thinking, None);
        assert_eq!(serde_json::to_value(&spec).expect("json"), before);
    }

    /// An action carries no words to steer with, so it is never a held line.
    #[test]
    fn an_action_is_never_held() {
        let action = Input::Action {
            action: Action {
                name: "x".into(),
                args: Value::Null,
            },
        };
        assert!(!action.is_held());
    }
}

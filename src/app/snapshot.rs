//! The resources the application core owns and the snapshots it hands out.
//!
//! Two readers share these shapes: the TUI projection and the app-server wire
//! (`crate::app_server::protocol`). They are the same types on purpose — a
//! second copy of a conversation summary is a second answer to "is this unread".
//!
//! Serialization is the wire contract: camelCase keys, tagged unions on `type`,
//! absent rather than null for optional fields, and never a credential. Provider
//! state carries presence, source, and status only (spec "Errors, load, and
//! security").
// The contract lands before its caller: the actor that fills these in (B2) is
// the first consumer. Remove this allow when it arrives.
#![allow(dead_code)]

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::app::ids::{
    AgentId, AssetId, CommandId, ConversationId, DeliveryId, EpochId, FeedbackId, InteractionId,
    ItemId, OperationId, QueueId, RoomId, ScopeId, SessionId, TaskId, TurnId, UnixMillis,
};

/// A bounded slice of an otherwise unbounded collection: the revision it was
/// read at, how many entries exist, and the ones worth carrying in a snapshot.
/// The remainder is read through the paginated resource methods.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Collection<T> {
    pub revision: u64,
    pub count: u32,
    pub active: Vec<T>,
}

/// One page of a paginated read. `next_cursor` is absent at the end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    pub items: Vec<T>,
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// Which revisioned resource a precondition or change refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RevisionScope {
    Session,
    Conversation,
    Queue,
    Config,
    Catalog,
    Agents,
    Rooms,
    Tasks,
    Team,
}

/// An expected revision carried by a mutation, so a stale write fails instead of
/// overwriting a concurrently refreshed view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceRevision {
    pub scope: RevisionScope,
    pub revision: u64,
}

// ---------------------------------------------------------------------------
// Selection vocabulary shared with the CLI
// ---------------------------------------------------------------------------

/// Thinking budget. `off` is a level rather than an absent value so a snapshot
/// always answers the question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ThinkingLevel {
    Off,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    /// The wire form, which is also the CLI's `/think` argument.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub const ALL: [Self; 6] = [
        Self::Off,
        Self::Low,
        Self::Medium,
        Self::High,
        Self::Xhigh,
        Self::Max,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    BypassPermissions,
    DontAsk,
    Plan,
}

impl PermissionMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::AcceptEdits => "acceptEdits",
            Self::BypassPermissions => "bypassPermissions",
            Self::DontAsk => "dontAsk",
            Self::Plan => "plan",
        }
    }

    pub const ALL: [Self; 5] = [
        Self::Default,
        Self::AcceptEdits,
        Self::BypassPermissions,
        Self::DontAsk,
        Self::Plan,
    ];
}

/// A persisted theme preference. Which colours a frontend actually paints is its
/// own business; that this choice was saved is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ThemeChoice {
    Auto,
    Dark,
    Light,
}

impl ThemeChoice {
    /// The wire form, which is also the CLI's `/theme` argument.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub const ALL: [Self; 3] = [Self::Auto, Self::Dark, Self::Light];
}

/// Syntax family of the resolved shell, so a frontend can render (and a model can
/// be told) what a command line will be parsed as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ShellDialect {
    Posix,
    Powershell,
    Cmd,
    Unknown,
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// How a persisted session is named on the wire. Opaque session IDs die with
/// their epoch, so selection uses an explicit transcript locator instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SessionLocator {
    /// The most recently active session, which is what `/resume` with no
    /// argument and `--continue` both mean.
    Latest,
    /// Exact transcript stem (`{slug}-{ts}`).
    #[serde(rename_all = "camelCase")]
    Stem { stem: String },
    /// Exact transcript path.
    #[serde(rename_all = "camelCase")]
    Path { path: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionState {
    Active,
    Closed,
}

/// The session's own bounded state. Everything unbounded it owns is a collection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: SessionId,
    pub epoch: EpochId,
    pub title: String,
    pub state: SessionState,
    pub cwd: PathBuf,
    pub locator: SessionLocator,
    pub provider: String,
    pub model: String,
    pub thinking: ThinkingLevel,
    pub permission_mode: PermissionMode,
    pub created_at: UnixMillis,
    pub updated_at: UnixMillis,
    /// The session resumed a persisted transcript rather than starting empty.
    pub resumed: bool,
}

/// One persisted session as `session/list` reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionListEntry {
    pub locator: SessionLocator,
    pub title: String,
    pub cwd: PathBuf,
    pub updated_at: UnixMillis,
    pub message_count: u32,
    /// This session is the one currently open on this connection.
    pub open: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionCloseReason {
    /// The client asked for it.
    Requested,
    /// The transport reached EOF and the shutdown policy ran.
    Disconnected,
    /// The session was replaced by a reset or a resume.
    Replaced,
}

// ---------------------------------------------------------------------------
// Conversation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ConversationKind {
    /// The console: session actions, shell input, and the turn the user's prose
    /// starts. Rendered through the same projection as every other conversation.
    Main,
    #[serde(rename_all = "camelCase")]
    Agent { agent_id: AgentId },
    #[serde(rename_all = "camelCase")]
    Room { room_id: RoomId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConversationRunState {
    Idle,
    Running,
    /// An agent conversation whose instance was stopped; its history stays and a
    /// direct message revives it.
    Stopped,
    /// A room, which receives posts and never owns a turn.
    Passive,
}

/// Something the conversation is waiting on the user for, stated rather than
/// inferred from prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ObligationKind {
    /// The user was mentioned and has not replied.
    MentionDebt,
    /// A question is open on the user.
    AwaitingUser,
    /// A message the user sent has not been answered.
    UnansweredMessage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Obligation {
    pub kind: ObligationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<ItemId>,
    pub since: UnixMillis,
}

/// The bounded state of one conversation, including the attention state a
/// frontend cannot safely infer from text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSummary {
    pub id: ConversationId,
    pub kind: ConversationKind,
    pub title: String,
    /// Bumped by every change to this summary; carried by `markRead` so a stale
    /// client cannot clear attention it never saw.
    pub revision: u64,
    /// Bumped only by structural rewrites (rewind, compaction). Ordinary
    /// streaming and appends leave it alone, so an item cursor stays valid.
    pub history_generation: u64,
    pub run_state: ConversationRunState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn_id: Option<TurnId>,
    pub unread: u32,
    pub mentions: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_cursor: Option<ItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_item_id: Option<ItemId>,
    pub obligations: Vec<Obligation>,
    /// The user may speak here. False for a room they have not joined.
    pub is_member: bool,
    pub queue_revision: u64,
    pub queue_count: u32,
    pub pending_interactions: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<UnixMillis>,
}

/// A cursor into one conversation's history. Bound to the generation it was
/// issued under; a continuation from a stale one is refused rather than mixing
/// pre- and post-rewrite history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemCursor {
    pub history_generation: u64,
    pub after: ItemId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PageDirection {
    /// Older items, which is how a transcript is paged backwards from the tail.
    Backward,
    Forward,
}

// ---------------------------------------------------------------------------
// Turn
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
}

impl TurnStatus {
    /// Exactly one of these closes a turn; an error never substitutes for it.
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// What started this turn. The console starts turns the user never typed —
/// a mail digest wakes main on every frontend (`auto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TurnOrigin {
    /// Prose the user submitted.
    User,
    /// The next entry of the input queue draining.
    Queue,
    /// A message from main or a peer that woke this conversation.
    Peer,
    /// The core decided to run without an input of its own (mail digest wake).
    Auto,
    /// A standalone shell run.
    Shell,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// This is the end-of-round accounting total rather than freshly streamed
    /// output: a rate sampler must not read the jump as a burst.
    pub authoritative: bool,
}

/// Context window occupancy, with the point at which the core compacts on its
/// own — the same three numbers the CLI's `/context` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub used: u64,
    pub window: u64,
    pub trigger: u64,
}

/// One model or standalone shell run attached to a runnable conversation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: TurnId,
    pub conversation_id: ConversationId,
    pub status: TurnStatus,
    pub origin: TurnOrigin,
    /// Model requests inside the tool loop so far; a retry re-runs the same one.
    pub round: u32,
    /// The completed items this turn consumed. They are ordered before
    /// `turn/started` and carry no `turnId` of their own.
    pub input_item_ids: Vec<ItemId>,
    pub started_at: UnixMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<UnixMillis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TurnUsage>,
    /// Why a failed turn failed. Present only with `status: failed`; the terminal
    /// event is still the turn's, not an error frame's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TurnError>,
}

/// A turn's failure, in the same stable-code vocabulary as a request error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TurnError {
    pub code: String,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Item
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ItemStatus {
    Pending,
    Streaming,
    Completed,
    Failed,
    Cancelled,
}

/// A foreground command's live tail: bounded lines after carriage-return
/// handling and escape removal, plus how many lines have completed. It replaces
/// the previous sample and never appends to final output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandTail {
    pub lines: Vec<String>,
    pub total_lines: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RewindMode {
    Preview,
    Applied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum NoticeLevel {
    Info,
    Warning,
    Error,
}

/// What the user chose at a permission prompt, and what the server advertises as
/// choosable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PermissionDecisionKind {
    AllowOnce,
    AllowSession,
    Deny,
}

/// Ordered semantic content. Not a terminal row and not a provider event.
///
/// An item belongs to a conversation and may carry a `turnId`; the input item
/// that opens a turn carries none. `item/completed` is authoritative over every
/// delta that preceded it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: ItemId,
    pub status: ItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<UnixMillis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<UnixMillis>,
    #[serde(flatten)]
    pub body: ItemBody,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ItemBody {
    /// Prose the user submitted.
    #[serde(rename_all = "camelCase")]
    UserMessage {
        text: String,
        attachments: Vec<AssetId>,
    },
    /// Prose the model produced.
    #[serde(rename_all = "camelCase")]
    AssistantMessage { text: String },
    /// A message that arrived from main or from a colleague. `to` is the private
    /// lane's target when it is not this conversation's own owner.
    #[serde(rename_all = "camelCase")]
    PeerMessage {
        from: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery_id: Option<DeliveryId>,
    },
    /// A room post. Completed on arrival, with no turn of its own.
    #[serde(rename_all = "camelCase")]
    RoomMessage {
        room_id: RoomId,
        from: String,
        text: String,
        room_seq: u64,
        mentions: Vec<String>,
    },
    /// Model reasoning, appended by `item/reasoningDelta`.
    #[serde(rename_all = "camelCase")]
    Reasoning { text: String },
    /// A tool call and its authoritative result. Whether it succeeded is the
    /// item's own `status` — a tool-specific status beside it would be a second
    /// answer to the same question, and two answers can disagree.
    #[serde(rename_all = "camelCase")]
    ToolCall {
        tool_call_id: String,
        name: String,
        /// The resolved input, exactly as the tool received it.
        input: serde_json::Value,
        summary: String,
        output: String,
        duration_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        diff: Option<String>,
        /// Output too large for the item; read through `asset/readChunk`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact: Option<AssetId>,
    },
    /// A standalone shell run: the `!` command and its promoted background form.
    #[serde(rename_all = "camelCase")]
    Command {
        command: String,
        dialect: ShellDialect,
        output: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tail: Option<CommandTail>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        duration_ms: u64,
        /// The run was promoted to the background; it is watch state from here.
        background: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command_id: Option<CommandId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact: Option<AssetId>,
    },
    /// History was summarized. The numbers are the compactor's own outcome.
    #[serde(rename_all = "camelCase")]
    Compaction {
        before_tokens: u64,
        after_tokens: u64,
        replaced_messages: u32,
        duration_ms: u64,
    },
    /// History was cut back to an earlier point.
    #[serde(rename_all = "camelCase")]
    Rewind {
        mode: RewindMode,
        removed_items: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_item_id: Option<ItemId>,
    },
    /// The user interrupted. `marker` is the exact string the transcript
    /// recorded, so the screen and the model read the same sentence.
    #[serde(rename_all = "camelCase")]
    Interruption { marker: String },
    /// Structured notice committed into the flow rather than raised as transient
    /// feedback.
    #[serde(rename_all = "camelCase")]
    Notice {
        code: String,
        level: NoticeLevel,
        text: String,
    },
    /// A question the user answered. This one does enter the model's context.
    #[serde(rename_all = "camelCase")]
    QuestionAnswer {
        interaction_id: InteractionId,
        question: String,
        answer: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option_id: Option<String>,
    },
    /// A permission decision, for display and audit. Explicitly excluded from
    /// model input.
    #[serde(rename_all = "camelCase")]
    PermissionReceipt {
        interaction_id: InteractionId,
        tool: String,
        decision: PermissionDecisionKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope_id: Option<ScopeId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
    /// An image or other asset, by opaque reference. No terminal cell geometry
    /// travels with it.
    #[serde(rename_all = "camelCase")]
    Asset {
        asset_id: AssetId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
}

impl ItemBody {
    /// The item categories, in the order the spec lists them. The wire tag is
    /// the camelCase of the variant name.
    pub const KINDS: [&'static str; 14] = [
        "userMessage",
        "assistantMessage",
        "peerMessage",
        "roomMessage",
        "reasoning",
        "toolCall",
        "command",
        "compaction",
        "rewind",
        "interruption",
        "notice",
        "questionAnswer",
        "permissionReceipt",
        "asset",
    ];
}

// ---------------------------------------------------------------------------
// Input queue
// ---------------------------------------------------------------------------

/// One entry of a conversation's input queue. The page it was submitted on is
/// immutable: a queued command runs where it was typed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub id: QueueId,
    pub origin_conversation_id: ConversationId,
    pub text: String,
    pub attachments: Vec<AssetId>,
    /// Plain, attachment-free input in the eligible FIFO prefix may be absorbed
    /// at a tool barrier. A command or an attachment blocks everything behind it.
    pub steer_eligible: bool,
    pub queued_at: UnixMillis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum QueueRemovalReason {
    /// Drained from the front into a new turn.
    Drained,
    /// Pulled back by the user.
    Reclaimed,
    /// Dropped with the conversation or by a reset.
    Cleared,
}

// ---------------------------------------------------------------------------
// Interaction
// ---------------------------------------------------------------------------

/// A permission rule the server derived and verified, offered as the scope of an
/// `allowSession` decision. Session-lived: never written to settings or the
/// transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionScope {
    pub id: ScopeId,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InteractionPreview {
    #[serde(rename_all = "camelCase")]
    Command { command: String },
    #[serde(rename_all = "camelCase")]
    Diff { diff: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolRequest {
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// What the server is asking. Exactly the decisions it advertises are valid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InteractionPrompt {
    #[serde(rename_all = "camelCase")]
    Permission {
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        tool: ToolRequest,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        preview: Option<InteractionPreview>,
        decisions: Vec<PermissionDecisionKind>,
        /// Absent when no verified session rule exists; `allowSession` is then
        /// rejected rather than invented.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_scope: Option<SessionScope>,
        allows_feedback: bool,
    },
    #[serde(rename_all = "camelCase")]
    Question {
        title: String,
        question: String,
        options: Vec<QuestionOption>,
        allows_free_text: bool,
    },
    /// An explicit destructive confirmation (session delete, rewind apply).
    #[serde(rename_all = "camelCase")]
    Confirmation {
        title: String,
        detail: String,
        confirm_label: String,
    },
}

/// A pending application request. It outlives the JSON-RPC call that announced
/// it: a prompt recovered from a snapshot stays answerable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Interaction {
    pub id: InteractionId,
    pub conversation_id: ConversationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<ItemId>,
    pub opened_at: UnixMillis,
    /// Milliseconds left on the confirmation guard, recomputed for every event
    /// and snapshot. Only keyboard approval is held back by it.
    pub remaining_guard_ms: u64,
    pub prompt: InteractionPrompt,
}

/// How the client's response was activated. Only keyboard approval inside the
/// guard is premature; denial, cancellation, and pointer approval are immediate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ActivationKind {
    Pointer,
    Keyboard,
    /// Neither — a programmatic client with no human activation to report.
    Programmatic,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum InteractionDecision {
    AllowOnce,
    #[serde(rename_all = "camelCase")]
    AllowSession {
        scope_id: ScopeId,
    },
    #[serde(rename_all = "camelCase")]
    Deny {
        /// What the user wanted instead; it travels to the model through the
        /// permission-error path so a denial carries a direction.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        feedback: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Answer {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        option_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    Confirm,
    /// The user dismissed the prompt. Fails closed.
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum InteractionCancelReason {
    Interrupted,
    SessionClosed,
    TurnEnded,
}

// ---------------------------------------------------------------------------
// Operation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum OperationKind {
    ProviderLogin,
    ProviderLogout,
    Share,
    McpReconnect,
    TeamStart,
    GarbageCollect,
    Compact,
    Rewind,
    SessionDelete,
    SkillInvoke,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum OperationStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperationProgress {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u32>,
}

/// Accepted asynchronous work that is not naturally a turn. Exactly one terminal
/// state, like a turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub id: OperationId,
    pub kind: OperationKind,
    pub status: OperationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<OperationProgress>,
    pub started_at: UnixMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<UnixMillis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TurnError>,
}

// ---------------------------------------------------------------------------
// Collaboration resources
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentState {
    Running,
    Idle,
    Stopped,
}

/// A standing member of the project's crew, or someone hired for one task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AgentKind {
    Crew,
    Hire,
}

/// One agent instance. Each is its own session with its own engine, so the
/// engine it runs on is reported rather than assumed to be the session's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentResource {
    pub id: AgentId,
    pub name: String,
    /// The definition it was spawned from, when it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub def: Option<String>,
    pub description: String,
    /// The task this instance is working on, or last worked on. A roster's task
    /// line is drawn from it, and a client that had only the description would
    /// be showing what the instance *is* where it means to show what it is
    /// *doing*.
    pub prompt: String,
    pub kind: AgentKind,
    pub state: AgentState,
    pub model: String,
    pub provider: String,
    pub thinking: ThinkingLevel,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationId>,
    /// Messages waiting in the inbox for this instance to claim.
    pub pending: u32,
    /// Messages sent to it that nobody has answered yet.
    pub unacked: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
    pub output_tokens: u64,
    pub tool_uses: u32,
    /// The tool lines this run has produced, oldest first and bounded. What a
    /// `Running` row says it is doing right now is the last of them; without it
    /// a client can only report that something is running.
    pub recent_activity: Vec<String>,
    pub last_active_at: UnixMillis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RoomMode {
    /// Everyone in the roster reads every post.
    Broadcast,
    /// Posts reach the room's members when they next look.
    Relay,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoomResource {
    pub id: RoomId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    pub mode: RoomMode,
    pub members: Vec<String>,
    /// The user joined, and may therefore speak.
    pub user_is_member: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationId>,
    pub message_count: u64,
    pub last_seq: u64,
    pub unread: u32,
    pub mentions: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskResource {
    pub id: TaskId,
    pub subject: String,
    pub description: String,
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    pub blocks: Vec<TaskId>,
    pub blocked_by: Vec<TaskId>,
}

/// Where a direct message got to. A peer's turn prose does not settle this;
/// only an observable message back does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DeliveryState {
    Queued,
    Delivered,
    Read,
    Answered,
    Dropped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryResource {
    pub id: DeliveryId,
    pub from: String,
    pub to: String,
    /// The private lane rather than a room.
    pub private: bool,
    pub state: DeliveryState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_item_id: Option<ItemId>,
    /// Follow-ups the runtime has chased with so far, and the bound on them.
    pub follow_ups: u32,
    pub max_follow_ups: u32,
    /// Why a dropped message will never arrive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub updated_at: UnixMillis,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum BackgroundCommandState {
    Running,
    /// One round finished; a periodic command may run again.
    Idle,
    Done,
    Failed,
    Cancelled,
}

/// A command running outside the foreground slot. Watch state only: the protocol
/// invents no cancel or re-foreground capability the CLI does not have.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundCommandResource {
    pub id: CommandId,
    pub label: String,
    pub command: String,
    pub state: BackgroundCommandState,
    pub started_at: UnixMillis,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<ItemId>,
}

// ---------------------------------------------------------------------------
// Configuration and catalogs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum McpStatus {
    Connecting,
    Connected,
    Disconnected,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpServerState {
    pub name: String,
    pub enabled: bool,
    pub status: McpStatus,
    pub tools: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Where a credential came from. Presence, source, and status only — no key
/// material reaches a snapshot or an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CredentialSource {
    Environment,
    Settings,
    OauthStore,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CredentialStatus {
    Present,
    Missing,
    Expired,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CredentialState {
    pub configured: bool,
    pub source: CredentialSource,
    pub status: CredentialStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub name: String,
    pub protocol: String,
    pub api_base_url: String,
    pub builtin: bool,
    pub supports_images: bool,
    pub credential: CredentialState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    pub supports_images: bool,
    pub supports_thinking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SkillSource {
    Bundled,
    User,
    Project,
    Plugin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SkillInfo {
    pub name: String,
    pub description: String,
    pub source: SkillSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImageInfo {
    pub asset_id: AssetId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub created_at: UnixMillis,
}

/// Which catalog a read asks for. Every one of them is answerable before a
/// session exists: a GUI picks a provider and a model on the way in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum CatalogKind {
    Models,
    Providers,
    Skills,
    McpServers,
    Images,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "catalog", rename_all = "camelCase")]
pub enum Catalog {
    Models(Page<ModelInfo>),
    Providers(Page<ProviderInfo>),
    Skills(Page<SkillInfo>),
    McpServers(Page<McpServerState>),
    Images(Page<ImageInfo>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ResourceKind {
    Agents,
    Rooms,
    Tasks,
    Deliveries,
    BackgroundCommands,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "resource", rename_all = "camelCase")]
pub enum ResourcePage {
    Agents(Page<AgentResource>),
    Rooms(Page<RoomResource>),
    Tasks(Page<TaskResource>),
    Deliveries(Page<DeliveryResource>),
    BackgroundCommands(Page<BackgroundCommandResource>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PermissionRuleDecision {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRule {
    pub decision: PermissionRuleDecision,
    pub rule: String,
    /// Installed for this session only (an `allowSession` grant), rather than
    /// read from settings.
    pub session_scoped: bool,
}

/// One settings layer that contributed to the effective configuration, so a
/// frontend can show where a value came from without reading files itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigLayer {
    pub path: PathBuf,
    pub keys: Vec<String>,
}

/// The effective configuration. No credential values, only where they come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigSnapshot {
    pub revision: u64,
    pub model: String,
    pub provider: String,
    pub thinking: ThinkingLevel,
    pub permission_mode: PermissionMode,
    pub theme: ThemeChoice,
    pub cwd: PathBuf,
    pub shell: String,
    pub shell_dialect: ShellDialect,
    pub permissions: Vec<PermissionRule>,
    pub layers: Vec<ConfigLayer>,
    pub mcp_servers: Vec<McpServerState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ConfigSection {
    Selection,
    Permissions,
    Layers,
    Mcp,
}

// ---------------------------------------------------------------------------
// Assets and feedback
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AssetKind {
    Image,
    Text,
    Binary,
}

/// Where an asset's bytes live. Session assets are cleaned up with the session;
/// transcript assets stay reconstructable from durable content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AssetOrigin {
    Session,
    Transcript,
}

/// A registered asset. Registration snapshots the bytes into server-owned
/// storage; the caller's path is never borrowed or deleted afterwards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetRecord {
    pub id: AssetId,
    pub kind: AssetKind,
    pub origin: AssetOrigin,
    pub mime: String,
    pub bytes: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    pub created_at: UnixMillis,
}

/// A warning, notice, or error raised outside any single turn. It carries a
/// stable code so a frontend branches on the code, never on the text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Feedback {
    pub id: FeedbackId,
    pub level: NoticeLevel,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationId>,
    pub raised_at: UnixMillis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<UnixMillis>,
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// What this build can do, reported rather than inferred from model or provider
/// names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerCapabilities {
    pub multi_conversation: bool,
    pub reasoning: bool,
    pub images: bool,
    pub teams: bool,
    pub rooms: bool,
    pub shell: bool,
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

/// The unbounded collections a session owns, each as a revision plus the entries
/// worth carrying inline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeCollections {
    pub agents: Collection<AgentResource>,
    pub rooms: Collection<RoomResource>,
    pub tasks: Collection<TaskResource>,
    pub deliveries: Collection<DeliveryResource>,
    pub background_commands: Collection<BackgroundCommandResource>,
    pub mcp_servers: Vec<McpServerState>,
}

/// An atomic read of the session. Valid through `event_cursor`: the first event
/// a client sees afterwards has a strictly greater `seq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub session: SessionSummary,
    pub capabilities: ServerCapabilities,
    pub conversations: Collection<ConversationSummary>,
    pub active_turns: Vec<Turn>,
    pub interactions: Vec<Interaction>,
    pub operations: Vec<Operation>,
    pub collections: RuntimeCollections,
    pub feedback: Vec<Feedback>,
    pub config: ConfigSnapshot,
    pub event_cursor: u64,
}

/// An atomic read of one conversation. Reading never marks it read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSnapshot {
    pub conversation: ConversationSummary,
    pub items: Page<Item>,
    /// The generation the item page was cut at; a continuation from a stale one
    /// is refused.
    pub history_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_turn: Option<Turn>,
    pub queue: Page<QueueEntry>,
    pub interactions: Vec<Interaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<ContextUsage>,
    pub event_cursor: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contract's selection vocabulary is the CLI's. If a permission mode is
    /// renamed in the domain, this fails rather than letting the wire drift.
    #[test]
    fn permission_modes_match_the_domain() {
        use std::str::FromStr;
        for mode in PermissionMode::ALL {
            let domain = crate::permission::PermissionMode::from_str(mode.as_str())
                .unwrap_or_else(|error| panic!("{}: {error}", mode.as_str()));
            let expected = match mode {
                PermissionMode::Default => crate::permission::PermissionMode::Default,
                PermissionMode::AcceptEdits => crate::permission::PermissionMode::AcceptEdits,
                PermissionMode::BypassPermissions => {
                    crate::permission::PermissionMode::BypassPermissions
                }
                PermissionMode::DontAsk => crate::permission::PermissionMode::DontAsk,
                PermissionMode::Plan => crate::permission::PermissionMode::Plan,
            };
            assert_eq!(domain, expected);
        }
        assert_eq!(
            serde_json::to_value(PermissionMode::AcceptEdits)
                .unwrap_or_else(|error| panic!("{error}")),
            serde_json::json!("acceptEdits")
        );
    }

    #[test]
    fn thinking_levels_match_the_domain() {
        for level in ThinkingLevel::ALL {
            if level == ThinkingLevel::Off {
                assert!(!crate::api::contract::THINKING_LEVELS.contains(&level.as_str()));
                continue;
            }
            assert!(
                crate::api::contract::THINKING_LEVELS.contains(&level.as_str()),
                "{} is not a thinking level the engine knows",
                level.as_str()
            );
        }
        assert_eq!(
            ThinkingLevel::ALL.len(),
            crate::api::contract::THINKING_LEVELS.len() + 1,
            "off plus every engine level, and nothing else"
        );
    }

    #[test]
    fn item_kinds_are_the_tags_serde_writes() {
        let bodies = [
            ItemBody::UserMessage {
                text: String::new(),
                attachments: Vec::new(),
            },
            ItemBody::AssistantMessage {
                text: String::new(),
            },
            ItemBody::PeerMessage {
                from: String::new(),
                to: None,
                text: String::new(),
                delivery_id: None,
            },
            ItemBody::RoomMessage {
                room_id: RoomId::new("room_1"),
                from: String::new(),
                text: String::new(),
                room_seq: 0,
                mentions: Vec::new(),
            },
            ItemBody::Reasoning {
                text: String::new(),
            },
            ItemBody::ToolCall {
                tool_call_id: String::new(),
                name: String::new(),
                input: serde_json::Value::Null,
                summary: String::new(),
                output: String::new(),
                duration_ms: 0,
                diff: None,
                artifact: None,
            },
            ItemBody::Command {
                command: String::new(),
                dialect: ShellDialect::Posix,
                output: String::new(),
                tail: None,
                exit_code: None,
                duration_ms: 0,
                background: false,
                command_id: None,
                artifact: None,
            },
            ItemBody::Compaction {
                before_tokens: 0,
                after_tokens: 0,
                replaced_messages: 0,
                duration_ms: 0,
            },
            ItemBody::Rewind {
                mode: RewindMode::Preview,
                removed_items: 0,
                target_item_id: None,
            },
            ItemBody::Interruption {
                marker: String::new(),
            },
            ItemBody::Notice {
                code: String::new(),
                level: NoticeLevel::Info,
                text: String::new(),
            },
            ItemBody::QuestionAnswer {
                interaction_id: InteractionId::new("int_1"),
                question: String::new(),
                answer: String::new(),
                option_id: None,
            },
            ItemBody::PermissionReceipt {
                interaction_id: InteractionId::new("int_1"),
                tool: String::new(),
                decision: PermissionDecisionKind::AllowOnce,
                scope_id: None,
                feedback: None,
            },
            ItemBody::Asset {
                asset_id: AssetId::new("asset_1"),
                label: None,
            },
        ];
        assert_eq!(bodies.len(), ItemBody::KINDS.len());
        for (body, expected) in bodies.iter().zip(ItemBody::KINDS) {
            let value = serde_json::to_value(body).unwrap_or_else(|error| panic!("{error}"));
            assert_eq!(
                value.get("type").and_then(|tag| tag.as_str()),
                Some(expected),
                "item body tag drifted from the declared kind list"
            );
        }
    }

    #[test]
    fn an_item_flattens_its_body_beside_its_envelope() {
        let item = Item {
            id: ItemId::new("item_12"),
            status: ItemStatus::Streaming,
            turn_id: Some(TurnId::new("turn_9")),
            started_at: Some(1),
            completed_at: None,
            body: ItemBody::AssistantMessage {
                text: String::new(),
            },
        };
        let value = serde_json::to_value(&item).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            value,
            serde_json::json!({
                "id": "item_12",
                "status": "streaming",
                "turnId": "turn_9",
                "startedAt": 1,
                "type": "assistantMessage",
                "text": ""
            })
        );
        let decoded: Item = serde_json::from_value(value).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(decoded, item);
    }
}

//! The semantic events the application core publishes.
//!
//! One layer of three: `EngineEvent` is private query/tool progress entering the
//! core, [`AppEvent`] is application state both frontends consume, and `TuiEvent`
//! is terminal-only effect. A page break, a pinned panel, and a cell measurement
//! are not events here and never will be.
//!
//! Every variant maps to exactly one app-server notification method
//! (`crate::app_server::protocol::ServerNotification`); the mapping is total in
//! both directions and a test keeps it that way.
// The contract lands before its caller: the actor that stamps and publishes
// these (B2) is the first consumer. Remove this allow when it arrives.
#![allow(dead_code)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::app::ids::{
    AgentId, ConversationId, FeedbackId, InteractionId, ItemId, OperationId, QueueId, SessionId,
    TaskId, TurnId, UnixMillis,
};
use crate::app::snapshot::{
    AgentResource, AssetRecord, BackgroundCommandResource, CatalogKind, CommandTail,
    ConfigSnapshot, ContextUsage, ConversationSummary, DeliveryResource, Feedback, Interaction,
    InteractionCancelReason, InteractionDecision, Item, Operation, OperationProgress, QueueEntry,
    QueueRemovalReason, RoomResource, SessionCloseReason, SessionLocator, SessionSummary,
    TaskResource, Turn, TurnUsage,
};

/// The header every event carries.
///
/// `seq` is assigned when the actor mutates state, not when a transport
/// serializes the event: it is strictly increasing and gapless within one server
/// epoch. `ts` is stamped at the same instant. `caused_by` names the operation
/// that asked for this, when one did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventMeta {
    pub seq: u64,
    pub ts: UnixMillis,
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<OperationId>,
    /// Set only when a transport merged a run of adjacent append deltas for one
    /// item into this frame: the `seq` that run began at. Every event from
    /// `coalescedFrom` through `seq` is represented here, so a client checking
    /// for gaps still reads a gapless stream. The core never sets it — merging
    /// is the transport's, and this is how it stays honest about what it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesced_from: Option<u64>,
}

/// One sequenced application event.
///
/// The kernel event has no serialization of its own: the wire form is the
/// notification its payload maps to, and a second encoding of the same fact
/// would be a second contract to keep true.
#[derive(Debug, Clone, PartialEq)]
pub struct AppEvent {
    pub meta: EventMeta,
    pub payload: AppEventPayload,
}

impl AppEvent {
    /// The app-server notification method this event is published as.
    pub fn method(&self) -> &'static str {
        self.payload.method()
    }
}

// --- Session ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionUpdated {
    pub session: SessionSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionClosed {
    pub session_id: SessionId,
    pub reason: SessionCloseReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleted {
    pub locator: SessionLocator,
}

// --- Conversation ----------------------------------------------------------

/// The summary replaces its predecessor whole, attention state included.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationChanged {
    pub conversation: ConversationSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationRemoved {
    pub conversation_id: ConversationId,
}

// --- Turn ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TurnChanged {
    pub conversation_id: ConversationId,
    pub turn: Turn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TurnRoundStarted {
    pub conversation_id: ConversationId,
    pub turn_id: TurnId,
    pub round: u32,
}

/// A failed stream attempt, with the authoritative replacement of the turn's
/// live tail at its pre-attempt checkpoint. The discarded items are gone; the
/// next attempt uses new item IDs, and a client never guesses which text to roll
/// back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TurnRetrying {
    pub conversation_id: ConversationId,
    pub turn_id: TurnId,
    pub round: u32,
    /// Attempts made so far, including the one that just failed.
    pub attempt: u32,
    pub max_attempts: u32,
    /// How long the core waits before the next attempt.
    pub delay_ms: u64,
    pub removed_item_ids: Vec<ItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TurnRoundCompleted {
    pub conversation_id: ConversationId,
    pub turn_id: TurnId,
    pub round: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TurnUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TurnUsageUpdated {
    pub conversation_id: ConversationId,
    pub turn_id: TurnId,
    pub usage: TurnUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<ContextUsage>,
}

// --- Item ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemChanged {
    pub conversation_id: ConversationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub item: Item,
}

/// An append. `delta_seq` is contiguous per item, so a client can tell a gap from
/// a coalesced frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemDelta {
    pub conversation_id: ConversationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub item_id: ItemId,
    pub delta_seq: u64,
    pub delta: String,
}

/// A replacement, not an append: the bounded tail of the one foreground command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemCommandTailUpdated {
    pub conversation_id: ConversationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    pub item_id: ItemId,
    pub tail: CommandTail,
}

// --- Input queue -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueItemAdded {
    pub conversation_id: ConversationId,
    pub revision: u64,
    pub position: u32,
    pub entry: QueueEntry,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueItemRemoved {
    pub conversation_id: ConversationId,
    pub revision: u64,
    pub queue_id: QueueId,
    pub reason: QueueRemovalReason,
}

/// A tool barrier took this entry into the running turn's own context. One event
/// per absorbed entry, in contiguous sequence order; a pull-back racing this
/// loses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueItemAbsorbed {
    pub conversation_id: ConversationId,
    pub revision: u64,
    pub queue_id: QueueId,
    pub turn_id: TurnId,
    /// The conversation item the absorbed input became.
    pub item_id: ItemId,
}

// --- Interaction -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InteractionOpened {
    pub interaction: Interaction,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InteractionResolved {
    pub interaction_id: InteractionId,
    pub conversation_id: ConversationId,
    pub decision: InteractionDecision,
    /// The ordered item the resolution committed, before execution continued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<ItemId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InteractionCancelled {
    pub interaction_id: InteractionId,
    pub conversation_id: ConversationId,
    pub reason: InteractionCancelReason,
}

// --- Collaboration ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentChanged {
    pub agent: AgentResource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoomChanged {
    pub room: RoomResource,
}

/// An instance left the roster: stopped is not gone, but removed is. Without
/// this a client's roster would keep a name the session no longer has until it
/// re-read a snapshot — the removal half the spec asks of every keyed
/// collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentRemoved {
    pub agent_id: AgentId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskChanged {
    pub task: TaskResource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskRemoved {
    pub task_id: TaskId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryChanged {
    pub delivery: DeliveryResource,
}

/// A background command's state moved. Watch state only: the protocol invents no
/// cancel or re-foreground capability the CLI does not have, and a label-only
/// string is not a resource update (parity ledger, "agent/task/command watch
/// transitions").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandChanged {
    pub command: BackgroundCommandResource,
}

// --- Operation -------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperationChanged {
    pub operation: Operation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperationProgressed {
    pub operation_id: OperationId,
    pub progress: OperationProgress,
}

// --- Runtime state ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigChanged {
    pub config: ConfigSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogChanged {
    pub catalog: CatalogKind,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetAvailable {
    pub asset: AssetRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackRaised {
    pub feedback: Feedback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackCleared {
    pub feedback_id: FeedbackId,
}

/// What happened. One variant per app-server notification method.
#[derive(Debug, Clone, PartialEq)]
pub enum AppEventPayload {
    SessionUpdated(SessionUpdated),
    SessionClosed(SessionClosed),
    SessionDeleted(SessionDeleted),
    ConversationCreated(ConversationChanged),
    ConversationUpdated(ConversationChanged),
    ConversationRemoved(ConversationRemoved),
    TurnStarted(TurnChanged),
    TurnRoundStarted(TurnRoundStarted),
    TurnRetrying(TurnRetrying),
    TurnRoundCompleted(TurnRoundCompleted),
    TurnUsageUpdated(TurnUsageUpdated),
    TurnCompleted(TurnChanged),
    ItemStarted(ItemChanged),
    ItemTextDelta(ItemDelta),
    ItemReasoningDelta(ItemDelta),
    ItemCommandTailUpdated(ItemCommandTailUpdated),
    ItemUpdated(ItemChanged),
    ItemCompleted(ItemChanged),
    QueueItemAdded(QueueItemAdded),
    QueueItemRemoved(QueueItemRemoved),
    QueueItemAbsorbed(QueueItemAbsorbed),
    InteractionOpened(InteractionOpened),
    InteractionResolved(InteractionResolved),
    InteractionCancelled(InteractionCancelled),
    AgentChanged(AgentChanged),
    AgentRemoved(AgentRemoved),
    RoomChanged(RoomChanged),
    TaskChanged(TaskChanged),
    TaskRemoved(TaskRemoved),
    DeliveryChanged(DeliveryChanged),
    CommandChanged(CommandChanged),
    OperationStarted(OperationChanged),
    OperationProgress(OperationProgressed),
    OperationCompleted(OperationChanged),
    ConfigChanged(ConfigChanged),
    CatalogChanged(CatalogChanged),
    AssetAvailable(AssetAvailable),
    FeedbackRaised(FeedbackRaised),
    FeedbackCleared(FeedbackCleared),
}

impl AppEventPayload {
    /// The app-server notification method. The wire name and the semantic
    /// variant are decided in one place, so neither can drift alone.
    pub fn method(&self) -> &'static str {
        match self {
            Self::SessionUpdated(_) => "session/updated",
            Self::SessionClosed(_) => "session/closed",
            Self::SessionDeleted(_) => "session/deleted",
            Self::ConversationCreated(_) => "conversation/created",
            Self::ConversationUpdated(_) => "conversation/updated",
            Self::ConversationRemoved(_) => "conversation/removed",
            Self::TurnStarted(_) => "turn/started",
            Self::TurnRoundStarted(_) => "turn/roundStarted",
            Self::TurnRetrying(_) => "turn/retrying",
            Self::TurnRoundCompleted(_) => "turn/roundCompleted",
            Self::TurnUsageUpdated(_) => "turn/usageUpdated",
            Self::TurnCompleted(_) => "turn/completed",
            Self::ItemStarted(_) => "item/started",
            Self::ItemTextDelta(_) => "item/textDelta",
            Self::ItemReasoningDelta(_) => "item/reasoningDelta",
            Self::ItemCommandTailUpdated(_) => "item/commandTailUpdated",
            Self::ItemUpdated(_) => "item/updated",
            Self::ItemCompleted(_) => "item/completed",
            Self::QueueItemAdded(_) => "queue/itemAdded",
            Self::QueueItemRemoved(_) => "queue/itemRemoved",
            Self::QueueItemAbsorbed(_) => "queue/itemAbsorbed",
            Self::InteractionOpened(_) => "interaction/opened",
            Self::InteractionResolved(_) => "interaction/resolved",
            Self::InteractionCancelled(_) => "interaction/cancelled",
            Self::AgentChanged(_) => "agent/changed",
            Self::AgentRemoved(_) => "agent/removed",
            Self::RoomChanged(_) => "room/changed",
            Self::TaskChanged(_) => "task/changed",
            Self::TaskRemoved(_) => "task/removed",
            Self::DeliveryChanged(_) => "delivery/changed",
            Self::CommandChanged(_) => "command/changed",
            Self::OperationStarted(_) => "operation/started",
            Self::OperationProgress(_) => "operation/progress",
            Self::OperationCompleted(_) => "operation/completed",
            Self::ConfigChanged(_) => "config/changed",
            Self::CatalogChanged(_) => "catalog/changed",
            Self::AssetAvailable(_) => "asset/available",
            Self::FeedbackRaised(_) => "feedback/raised",
            Self::FeedbackCleared(_) => "feedback/cleared",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_meta_is_the_shared_header() {
        let meta = EventMeta {
            seq: 101,
            ts: 1_760_000_000_000,
            session_id: SessionId::new("sess_1"),
            caused_by: None,
            coalesced_from: None,
        };
        assert_eq!(
            serde_json::to_value(&meta).unwrap_or_else(|error| panic!("{error}")),
            serde_json::json!({"seq": 101, "ts": 1_760_000_000_000u64, "sessionId": "sess_1"})
        );
        let decoded: EventMeta = serde_json::from_value(
            serde_json::json!({"seq": 101, "ts": 1_760_000_000_000u64, "sessionId": "sess_1"}),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(decoded, meta);
    }

    /// A merged frame says which run it stands for, so "seq 5 then seq 8" is a
    /// coalesced append rather than three events a client lost.
    #[test]
    fn a_coalesced_frame_names_the_run_it_stands_for() {
        let meta = EventMeta {
            seq: 108,
            ts: 1_760_000_000_000,
            session_id: SessionId::new("sess_1"),
            caused_by: None,
            coalesced_from: Some(105),
        };
        assert_eq!(
            serde_json::to_value(&meta).unwrap_or_else(|error| panic!("{error}")),
            serde_json::json!({
                "seq": 108,
                "ts": 1_760_000_000_000u64,
                "sessionId": "sess_1",
                "coalescedFrom": 105
            })
        );
    }
}

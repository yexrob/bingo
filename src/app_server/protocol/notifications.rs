//! Server notifications: one explicit method per semantic change.
//!
//! There is no generic `item/delta`, because a client should never have to infer
//! whether a payload appends, patches, or replaces. Text and reasoning deltas
//! append; command tails, item snapshots, and bounded resource summaries
//! replace; unbounded collections are paged and changed by keyed events.
//!
//! Every notification carries the shared header under `event` and its own body
//! flattened beside it. The bodies are the core's own event payloads
//! (`crate::app::event`) rather than a second set of wire structs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::app::event::{
    AgentChanged, AppEvent, AppEventPayload, AssetAvailable, CatalogChanged, CommandChanged,
    ConfigChanged, ConversationChanged, ConversationRemoved, DeliveryChanged, EventMeta,
    FeedbackCleared, FeedbackRaised, InteractionCancelled, InteractionOpened, InteractionResolved,
    ItemChanged, ItemCommandTailUpdated, ItemDelta, OperationChanged, OperationProgressed,
    QueueItemAbsorbed, QueueItemAdded, QueueItemRemoved, RoomChanged, SessionClosed,
    SessionDeleted, SessionUpdated, TaskChanged, TaskRemoved, TurnChanged, TurnRetrying,
    TurnRoundCompleted, TurnRoundStarted, TurnUsageUpdated,
};

/// One notification's params: the event header, then the body's own fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NotificationParams<T> {
    pub event: EventMeta,
    #[serde(flatten)]
    pub body: T,
}

impl<T> NotificationParams<T> {
    pub fn new(event: EventMeta, body: T) -> Self {
        Self { event, body }
    }
}

macro_rules! server_notifications {
    ($( $variant:ident ( $body:ty ) => $method:literal ),+ $(,)?) => {
        /// A server notification, tagged the way JSON-RPC tags one.
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
        #[serde(tag = "method", content = "params")]
        pub enum ServerNotification {
            $(
                #[serde(rename = $method)]
                $variant(NotificationParams<$body>),
            )+
        }

        impl ServerNotification {
            pub fn method(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => $method,)+
                }
            }

            /// Every notification method, in the order the spec's table lists
            /// them.
            pub const METHODS: &'static [&'static str] = &[$($method),+];
        }

        /// Every notification's params schema, in table order.
        pub fn notification_schemas() -> Vec<(&'static str, schemars::schema::RootSchema)> {
            vec![$(($method, schemars::schema_for!(NotificationParams<$body>))),+]
        }
    };
}

server_notifications! {
    SessionUpdated(SessionUpdated) => "session/updated",
    SessionClosed(SessionClosed) => "session/closed",
    SessionDeleted(SessionDeleted) => "session/deleted",
    ConversationCreated(ConversationChanged) => "conversation/created",
    ConversationUpdated(ConversationChanged) => "conversation/updated",
    ConversationRemoved(ConversationRemoved) => "conversation/removed",
    TurnStarted(TurnChanged) => "turn/started",
    TurnRoundStarted(TurnRoundStarted) => "turn/roundStarted",
    TurnRetrying(TurnRetrying) => "turn/retrying",
    TurnRoundCompleted(TurnRoundCompleted) => "turn/roundCompleted",
    TurnUsageUpdated(TurnUsageUpdated) => "turn/usageUpdated",
    TurnCompleted(TurnChanged) => "turn/completed",
    ItemStarted(ItemChanged) => "item/started",
    ItemTextDelta(ItemDelta) => "item/textDelta",
    ItemReasoningDelta(ItemDelta) => "item/reasoningDelta",
    ItemCommandTailUpdated(ItemCommandTailUpdated) => "item/commandTailUpdated",
    ItemUpdated(ItemChanged) => "item/updated",
    ItemCompleted(ItemChanged) => "item/completed",
    QueueItemAdded(QueueItemAdded) => "queue/itemAdded",
    QueueItemRemoved(QueueItemRemoved) => "queue/itemRemoved",
    QueueItemAbsorbed(QueueItemAbsorbed) => "queue/itemAbsorbed",
    InteractionOpened(InteractionOpened) => "interaction/opened",
    InteractionResolved(InteractionResolved) => "interaction/resolved",
    InteractionCancelled(InteractionCancelled) => "interaction/cancelled",
    AgentChanged(AgentChanged) => "agent/changed",
    RoomChanged(RoomChanged) => "room/changed",
    TaskChanged(TaskChanged) => "task/changed",
    TaskRemoved(TaskRemoved) => "task/removed",
    DeliveryChanged(DeliveryChanged) => "delivery/changed",
    CommandChanged(CommandChanged) => "command/changed",
    OperationStarted(OperationChanged) => "operation/started",
    OperationProgress(OperationProgressed) => "operation/progress",
    OperationCompleted(OperationChanged) => "operation/completed",
    ConfigChanged(ConfigChanged) => "config/changed",
    CatalogChanged(CatalogChanged) => "catalog/changed",
    AssetAvailable(AssetAvailable) => "asset/available",
    FeedbackRaised(FeedbackRaised) => "feedback/raised",
    FeedbackCleared(FeedbackCleared) => "feedback/cleared",
}

impl From<AppEvent> for ServerNotification {
    /// The core's event becomes its wire form without a translation table: the
    /// bodies are the same structs, so this match is the whole adapter.
    fn from(event: AppEvent) -> Self {
        let meta = event.meta;
        match event.payload {
            AppEventPayload::SessionUpdated(body) => {
                Self::SessionUpdated(NotificationParams::new(meta, body))
            }
            AppEventPayload::SessionClosed(body) => {
                Self::SessionClosed(NotificationParams::new(meta, body))
            }
            AppEventPayload::SessionDeleted(body) => {
                Self::SessionDeleted(NotificationParams::new(meta, body))
            }
            AppEventPayload::ConversationCreated(body) => {
                Self::ConversationCreated(NotificationParams::new(meta, body))
            }
            AppEventPayload::ConversationUpdated(body) => {
                Self::ConversationUpdated(NotificationParams::new(meta, body))
            }
            AppEventPayload::ConversationRemoved(body) => {
                Self::ConversationRemoved(NotificationParams::new(meta, body))
            }
            AppEventPayload::TurnStarted(body) => {
                Self::TurnStarted(NotificationParams::new(meta, body))
            }
            AppEventPayload::TurnRoundStarted(body) => {
                Self::TurnRoundStarted(NotificationParams::new(meta, body))
            }
            AppEventPayload::TurnRetrying(body) => {
                Self::TurnRetrying(NotificationParams::new(meta, body))
            }
            AppEventPayload::TurnRoundCompleted(body) => {
                Self::TurnRoundCompleted(NotificationParams::new(meta, body))
            }
            AppEventPayload::TurnUsageUpdated(body) => {
                Self::TurnUsageUpdated(NotificationParams::new(meta, body))
            }
            AppEventPayload::TurnCompleted(body) => {
                Self::TurnCompleted(NotificationParams::new(meta, body))
            }
            AppEventPayload::ItemStarted(body) => {
                Self::ItemStarted(NotificationParams::new(meta, body))
            }
            AppEventPayload::ItemTextDelta(body) => {
                Self::ItemTextDelta(NotificationParams::new(meta, body))
            }
            AppEventPayload::ItemReasoningDelta(body) => {
                Self::ItemReasoningDelta(NotificationParams::new(meta, body))
            }
            AppEventPayload::ItemCommandTailUpdated(body) => {
                Self::ItemCommandTailUpdated(NotificationParams::new(meta, body))
            }
            AppEventPayload::ItemUpdated(body) => {
                Self::ItemUpdated(NotificationParams::new(meta, body))
            }
            AppEventPayload::ItemCompleted(body) => {
                Self::ItemCompleted(NotificationParams::new(meta, body))
            }
            AppEventPayload::QueueItemAdded(body) => {
                Self::QueueItemAdded(NotificationParams::new(meta, body))
            }
            AppEventPayload::QueueItemRemoved(body) => {
                Self::QueueItemRemoved(NotificationParams::new(meta, body))
            }
            AppEventPayload::QueueItemAbsorbed(body) => {
                Self::QueueItemAbsorbed(NotificationParams::new(meta, body))
            }
            AppEventPayload::InteractionOpened(body) => {
                Self::InteractionOpened(NotificationParams::new(meta, body))
            }
            AppEventPayload::InteractionResolved(body) => {
                Self::InteractionResolved(NotificationParams::new(meta, body))
            }
            AppEventPayload::InteractionCancelled(body) => {
                Self::InteractionCancelled(NotificationParams::new(meta, body))
            }
            AppEventPayload::AgentChanged(body) => {
                Self::AgentChanged(NotificationParams::new(meta, body))
            }
            AppEventPayload::RoomChanged(body) => {
                Self::RoomChanged(NotificationParams::new(meta, body))
            }
            AppEventPayload::TaskChanged(body) => {
                Self::TaskChanged(NotificationParams::new(meta, body))
            }
            AppEventPayload::TaskRemoved(body) => {
                Self::TaskRemoved(NotificationParams::new(meta, body))
            }
            AppEventPayload::DeliveryChanged(body) => {
                Self::DeliveryChanged(NotificationParams::new(meta, body))
            }
            AppEventPayload::CommandChanged(body) => {
                Self::CommandChanged(NotificationParams::new(meta, body))
            }
            AppEventPayload::OperationStarted(body) => {
                Self::OperationStarted(NotificationParams::new(meta, body))
            }
            AppEventPayload::OperationProgress(body) => {
                Self::OperationProgress(NotificationParams::new(meta, body))
            }
            AppEventPayload::OperationCompleted(body) => {
                Self::OperationCompleted(NotificationParams::new(meta, body))
            }
            AppEventPayload::ConfigChanged(body) => {
                Self::ConfigChanged(NotificationParams::new(meta, body))
            }
            AppEventPayload::CatalogChanged(body) => {
                Self::CatalogChanged(NotificationParams::new(meta, body))
            }
            AppEventPayload::AssetAvailable(body) => {
                Self::AssetAvailable(NotificationParams::new(meta, body))
            }
            AppEventPayload::FeedbackRaised(body) => {
                Self::FeedbackRaised(NotificationParams::new(meta, body))
            }
            AppEventPayload::FeedbackCleared(body) => {
                Self::FeedbackCleared(NotificationParams::new(meta, body))
            }
        }
    }
}

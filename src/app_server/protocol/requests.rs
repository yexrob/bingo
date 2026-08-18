//! The client request taxonomy: every method, its params, and its result.
//!
//! The surface stays smaller than the CLI's command list on purpose. One deep
//! submission path carries routing, queueing, and steering; a typed action union
//! carries the uncommon operations; everything else is a lifecycle call or an
//! authoritative read.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::app::command::{Action, ActionInfo, Submission, SubmitDisposition};
use crate::app::ids::{
    AssetId, ConversationId, EpochId, InteractionId, ItemId, QueueId, SessionId, TurnId,
};
use crate::app::snapshot::{
    ActivationKind, AssetRecord, Catalog, CatalogKind, ConfigSection, ConfigSnapshot,
    ConversationSnapshot, ConversationSummary, InteractionDecision, ItemCursor, Page,
    PermissionMode, QueueEntry, ResourceKind, ResourcePage, ResourceRevision, ServerCapabilities,
    SessionListEntry, SessionLocator, SessionSnapshot, ThinkingLevel,
};
use crate::app_server::protocol::error::ProtocolErrorKind;

// ---------------------------------------------------------------------------
// Connection
// ---------------------------------------------------------------------------

/// The major the client speaks and the minor window it accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolRange {
    pub major: u32,
    pub min_minor: u32,
    pub max_minor: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolVersion {
    pub major: u32,
    pub minor: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// What the client can do. A controlling client must be able to answer
/// interactions; initialization fails otherwise rather than silently
/// auto-denying a prompt that has not happened yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    pub interaction_response: bool,
    /// Experimental methods the client opts into by name.
    #[serde(default)]
    pub experimental: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    /// This process's run. Every resource ID belongs to it and dies with it.
    pub epoch: EpochId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FrameLimits {
    pub max_client_frame_bytes: u64,
    pub max_server_frame_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol: ProtocolRange,
    pub client: ClientInfo,
    pub capabilities: ClientCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol: ProtocolVersion,
    pub server: ServerInfo,
    pub limits: FrameLimits,
    pub capabilities: ServerCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct InitializedParams {}

/// EOF does the same thing. The shutdown policy interrupts active work and
/// resolves interactions fail-closed before persistence and exit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShutdownParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShutdownResult {
    /// Work the shutdown interrupted on its way out.
    pub interrupted_turns: u32,
    /// Interactions that were open and were resolved closed.
    pub denied_interactions: u32,
}

// ---------------------------------------------------------------------------
// Session lifecycle
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionListResult {
    pub sessions: Page<SessionListEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<ThinkingLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionStartResult {
    pub snapshot: SessionSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumeParams {
    pub locator: SessionLocator,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionResumeResult {
    pub snapshot: SessionSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadParams {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadResult {
    pub snapshot: SessionSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionCloseParams {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionCloseResult {
    pub session_id: SessionId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleteParams {
    pub locator: SessionLocator,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDeleteResult {
    pub locator: SessionLocator,
    pub deleted: bool,
}

// ---------------------------------------------------------------------------
// Conversation state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConversationListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationListResult {
    pub conversations: Page<ConversationSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationReadParams {
    pub conversation_id: ConversationId,
    /// Absent starts at the tail. A cursor from a stale `historyGeneration` is
    /// refused rather than mixing pre- and post-rewrite history.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<ItemCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationReadResult {
    pub snapshot: ConversationSnapshot,
}

/// Called only once content is actually visible. Prefetch and resynchronization
/// cannot clear unread or mention state, because reading never does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMarkReadParams {
    pub conversation_id: ConversationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_item_id: Option<ItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_room_seq: Option<u64>,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMarkReadResult {
    pub conversation: ConversationSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSubmitParams {
    pub conversation_id: ConversationId,
    pub input: Submission,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSubmitResult {
    pub disposition: SubmitDisposition,
}

// ---------------------------------------------------------------------------
// Turn and queue
// ---------------------------------------------------------------------------

/// Idempotent, and aimed at a server-issued turn ID: a late interrupt cannot
/// cancel the next turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams {
    pub conversation_id: ConversationId,
    pub turn_id: TurnId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptResult {
    pub turn_id: TurnId,
    /// False when the turn had already reached its terminal state.
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueReadParams {
    pub conversation_id: ConversationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueReadResult {
    pub entries: Page<QueueEntry>,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueReclaimTailParams {
    pub conversation_id: ConversationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}

/// Reclaim and absorption are one race with one winner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ReclaimOutcome {
    #[serde(rename_all = "camelCase")]
    Reclaimed {
        entry: QueueEntry,
        revision: u64,
    },
    /// A tool barrier took it first; it is in the running turn's context now.
    #[serde(rename_all = "camelCase")]
    AlreadyAbsorbed {
        queue_id: QueueId,
    },
    Empty,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueReclaimTailResult {
    pub outcome: ReclaimOutcome,
}

// ---------------------------------------------------------------------------
// Interaction
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InteractionRespondParams {
    pub interaction_id: InteractionId,
    pub activation: ActivationKind,
    pub decision: InteractionDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum RespondStatus {
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InteractionRespondResult {
    pub status: RespondStatus,
    /// The ordered item the resolution committed, when it commits one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<ItemId>,
}

// ---------------------------------------------------------------------------
// Actions and state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ActionListParams {
    /// Availability is answered for this page; absent answers for main.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_conversation_id: Option<ConversationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionListResult {
    pub actions: Vec<ActionInfo>,
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionExecuteParams {
    /// The page the action was submitted on. It stays the action's origin even
    /// if the user switches pages before it drains.
    pub origin_conversation_id: ConversationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precondition: Option<ResourceRevision>,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionExecuteResult {
    /// The same dispositions a submission gets: an action asked for while a turn
    /// is busy queues like any other input.
    pub disposition: SubmitDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ConfigReadParams {
    /// Absent reads every section.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sections: Option<Vec<ConfigSection>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConfigReadResult {
    pub config: ConfigSnapshot,
}

/// Answerable before `session/start`: a GUI picks its provider and model on the
/// way in, which is the job `--inspect` used to have.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogReadParams {
    pub catalog: CatalogKind,
    /// Which provider's models to list. Ignored by the other catalogs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogReadResult {
    pub catalog: Catalog,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceReadParams {
    pub resource: ResourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceReadResult {
    pub resource: ResourcePage,
}

// ---------------------------------------------------------------------------
// Assets
// ---------------------------------------------------------------------------

/// Registration validates the file and snapshots its bytes into server-owned
/// storage. The caller's path is never borrowed later and never deleted; a GUI
/// holding clipboard bytes writes its own temporary file and may remove it once
/// this succeeds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetRegisterPathParams {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_mime: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetRegisterPathResult {
    pub asset: AssetRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetReadChunkParams {
    pub asset_id: AssetId,
    pub offset: u64,
    pub length: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetReadChunkResult {
    /// Base64, bounded well below the server frame ceiling.
    pub data: String,
    pub next_offset: u64,
    pub eof: bool,
}

// ---------------------------------------------------------------------------
// The method table
// ---------------------------------------------------------------------------

/// One method's generated JSON Schemas, for the bundle.
pub struct MethodSchemas {
    pub method: RequestMethod,
    pub params: schemars::schema::RootSchema,
    pub result: schemars::schema::RootSchema,
}

/// The taxonomy, once. The method enum, the request union, the result union,
/// the decoder, the declared errors, and the schema bundle all read this table,
/// so a method cannot exist in one of them and not the others.
macro_rules! client_methods {
    ($(
        $variant:ident => $method:literal, $params:ty => $result:ty,
            errors: [$($error:ident),* $(,)?]
    );+ $(;)?) => {
        /// Every method name, in the order the taxonomy lists them.
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
        )]
        pub enum RequestMethod {
            $(#[serde(rename = $method)] $variant,)+
        }

        impl RequestMethod {
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $method,)+
                }
            }

            /// The application errors this method may return. Transport-level
            /// failures (oversized frames, a client that stopped reading) belong
            /// to the connection, not to a method.
            pub fn declared_errors(self) -> &'static [ProtocolErrorKind] {
                match self {
                    $(Self::$variant => &[$(ProtocolErrorKind::$error),*],)+
                }
            }
        }

        /// A client call, tagged the way JSON-RPC tags one: `method` plus
        /// `params`.
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
        #[serde(tag = "method", content = "params")]
        pub enum ClientRequest {
            $(#[serde(rename = $method)] $variant($params),)+
        }

        impl ClientRequest {
            pub fn method(&self) -> RequestMethod {
                match self {
                    $(Self::$variant(_) => RequestMethod::$variant,)+
                }
            }
        }

        /// A successful reply's payload. Its shape is decided by the method that
        /// was called, and the frame does not repeat the method — so decoding
        /// takes one, exactly as a client holding its own request does.
        #[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
        #[serde(untagged)]
        pub enum ResponseResult {
            $($variant($result),)+
        }

        impl ResponseResult {
            pub fn method(&self) -> RequestMethod {
                match self {
                    $(Self::$variant(_) => RequestMethod::$variant,)+
                }
            }

            pub fn from_value(
                method: RequestMethod,
                value: serde_json::Value,
            ) -> Result<Self, serde_json::Error> {
                Ok(match method {
                    $(RequestMethod::$variant => Self::$variant(serde_json::from_value(value)?),)+
                })
            }
        }

        /// Every method's params and result schema, in table order.
        pub fn method_schemas() -> Vec<MethodSchemas> {
            vec![$(MethodSchemas {
                method: RequestMethod::$variant,
                params: schemars::schema_for!($params),
                result: schemars::schema_for!($result),
            }),+]
        }
    };
}

client_methods! {
    Initialize => "initialize", InitializeParams => InitializeResult,
        errors: [ProtocolUnsupported, CapabilityRequired, AlreadyInitialized, BadArgument];
    Shutdown => "shutdown", ShutdownParams => ShutdownResult,
        errors: [NotInitialized];
    SessionList => "session/list", SessionListParams => SessionListResult,
        errors: [NotInitialized, BadArgument];
    SessionStart => "session/start", SessionStartParams => SessionStartResult,
        errors: [NotInitialized, BadArgument];
    SessionResume => "session/resume", SessionResumeParams => SessionResumeResult,
        errors: [NotInitialized, SessionNotFound, BadArgument];
    SessionRead => "session/read", SessionReadParams => SessionReadResult,
        errors: [NotInitialized, NoActiveSession];
    SessionClose => "session/close", SessionCloseParams => SessionCloseResult,
        errors: [NotInitialized, NoActiveSession];
    SessionDelete => "session/delete", SessionDeleteParams => SessionDeleteResult,
        errors: [NotInitialized, SessionNotFound, BadArgument];
    ConversationList => "conversation/list", ConversationListParams => ConversationListResult,
        errors: [NotInitialized, NoActiveSession, BadArgument];
    ConversationRead => "conversation/read", ConversationReadParams => ConversationReadResult,
        errors: [NotInitialized, NoActiveSession, ConversationNotFound, StalePage, BadArgument];
    ConversationMarkRead => "conversation/markRead",
        ConversationMarkReadParams => ConversationMarkReadResult,
        errors: [NotInitialized, NoActiveSession, ConversationNotFound, StaleRevision];
    ConversationSubmit => "conversation/submit",
        ConversationSubmitParams => ConversationSubmitResult,
        errors: [
            NotInitialized, NoActiveSession, ConversationNotFound, ActionUnavailable,
            AssetNotFound, BadArgument,
        ];
    TurnInterrupt => "turn/interrupt", TurnInterruptParams => TurnInterruptResult,
        errors: [NotInitialized, NoActiveSession, ConversationNotFound, TurnClosed];
    QueueRead => "queue/read", QueueReadParams => QueueReadResult,
        errors: [NotInitialized, NoActiveSession, ConversationNotFound, BadArgument];
    QueueReclaimTail => "queue/reclaimTail", QueueReclaimTailParams => QueueReclaimTailResult,
        errors: [NotInitialized, NoActiveSession, ConversationNotFound, StaleRevision];
    InteractionRespond => "interaction/respond",
        InteractionRespondParams => InteractionRespondResult,
        errors: [
            NotInitialized, NoActiveSession, InteractionClosed, InteractionNotReady,
            InteractionInvalidDecision,
        ];
    ActionList => "action/list", ActionListParams => ActionListResult,
        errors: [NotInitialized, NoActiveSession, ConversationNotFound];
    ActionExecute => "action/execute", ActionExecuteParams => ActionExecuteResult,
        errors: [
            NotInitialized, NoActiveSession, ConversationNotFound, ActionUnavailable,
            StaleRevision, BadArgument,
        ];
    ConfigRead => "config/read", ConfigReadParams => ConfigReadResult,
        errors: [NotInitialized, NoActiveSession];
    CatalogRead => "catalog/read", CatalogReadParams => CatalogReadResult,
        errors: [NotInitialized, BadArgument];
    ResourceRead => "resource/read", ResourceReadParams => ResourceReadResult,
        errors: [NotInitialized, NoActiveSession, BadArgument];
    AssetRegisterPath => "asset/registerPath",
        AssetRegisterPathParams => AssetRegisterPathResult,
        errors: [NotInitialized, NoActiveSession, AssetRejected, BadArgument];
    AssetReadChunk => "asset/readChunk", AssetReadChunkParams => AssetReadChunkResult,
        errors: [NotInitialized, AssetNotFound, BadArgument];
}

/// The only client notification: initialization is finished, and session events
/// may start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "method", content = "params")]
pub enum ClientNotification {
    #[serde(rename = "initialized")]
    Initialized(InitializedParams),
}

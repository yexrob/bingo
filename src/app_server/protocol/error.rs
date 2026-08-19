//! Errors on the wire: a JSON-RPC code, an English sanitized message, and
//! structured data whose `bingoCode` is the thing a client branches on.
//!
//! The codes are not a second registry — they are `crate::error`'s, which the
//! CLI exit already reads. This module only says which JSON-RPC number, scope,
//! and recoverability each one travels with.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::app::ids::{
    AssetId, ConversationId, InteractionId, ItemId, OperationId, QueueId, SessionId, TurnId,
};
use crate::error;

/// Invalid JSON. The line is refused and nothing is mutated.
pub const PARSE_ERROR: i32 = -32700;
/// A frame that is JSON but not a JSON-RPC request.
pub const INVALID_REQUEST: i32 = -32600;
pub const METHOD_NOT_FOUND: i32 = -32601;
pub const INVALID_PARAMS: i32 = -32602;
pub const INTERNAL_ERROR: i32 = -32603;

/// What the error is about, so a client knows what to re-read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub enum ErrorScope {
    /// The request itself was malformed or arrived out of order.
    #[default]
    Request,
    Connection,
    Session,
    Conversation,
    Turn,
    Item,
    Queue,
    Interaction,
    Operation,
    Asset,
    Action,
}

/// What would plausibly make it work. Advice, not a promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SuggestedAction {
    Retry,
    Reinitialize,
    RefreshSession,
    RefreshConversation,
    RefreshQueue,
    RefreshActions,
    Reauthenticate,
    Resubmit,
}

/// The structured half of an error. Human-readable text is English and
/// sanitized; clients never branch on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct ErrorData {
    pub bingo_code: String,
    pub recoverable: bool,
    pub scope: ErrorScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<ItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<QueueId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interaction_id: Option<InteractionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<OperationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<AssetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_action: Option<SuggestedAction>,
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ErrorData>,
}

impl RpcError {
    /// A standard JSON-RPC failure, which carries no application data: nothing
    /// application-level happened.
    pub fn standard(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: error::sanitize_msg(&message.into()),
            data: None,
        }
    }

    /// An application failure, with its declared code, scope, and advice.
    pub fn application(kind: ProtocolErrorKind) -> Self {
        Self {
            code: kind.code(),
            message: kind.message().to_string(),
            data: Some(ErrorData {
                bingo_code: kind.bingo_code().to_string(),
                recoverable: kind.recoverable(),
                scope: kind.scope(),
                suggested_action: kind.suggested_action(),
                ..ErrorData::default()
            }),
        }
    }
}

/// Every application error the protocol declares. The manifest lists these per
/// method; a client that has handled all of them has handled the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProtocolErrorKind {
    ProtocolUnsupported,
    CapabilityRequired,
    NotInitialized,
    AlreadyInitialized,
    NoActiveSession,
    SessionNotFound,
    ConversationNotFound,
    TurnClosed,
    StalePage,
    StaleRevision,
    InteractionNotReady,
    InteractionClosed,
    InteractionInvalidDecision,
    ActionUnavailable,
    AssetNotFound,
    AssetRejected,
    FrameTooLarge,
    ClientTooSlow,
    /// A well-formed request the core refused on its arguments.
    BadArgument,
}

impl ProtocolErrorKind {
    pub const ALL: [Self; 19] = [
        Self::ProtocolUnsupported,
        Self::CapabilityRequired,
        Self::NotInitialized,
        Self::AlreadyInitialized,
        Self::NoActiveSession,
        Self::SessionNotFound,
        Self::ConversationNotFound,
        Self::TurnClosed,
        Self::StalePage,
        Self::StaleRevision,
        Self::InteractionNotReady,
        Self::InteractionClosed,
        Self::InteractionInvalidDecision,
        Self::ActionUnavailable,
        Self::AssetNotFound,
        Self::AssetRejected,
        Self::FrameTooLarge,
        Self::ClientTooSlow,
        Self::BadArgument,
    ];

    /// The JSON-RPC number. Application numbers live in the implementation-defined
    /// range; -32008 and -32009 are unassigned so `TURN_CLOSED` keeps the -32010
    /// the spec printed.
    pub fn code(self) -> i32 {
        match self {
            Self::ProtocolUnsupported => -32001,
            Self::CapabilityRequired => -32002,
            Self::NotInitialized => -32003,
            Self::AlreadyInitialized => -32004,
            Self::NoActiveSession => -32005,
            Self::SessionNotFound => -32006,
            Self::ConversationNotFound => -32007,
            Self::TurnClosed => -32010,
            Self::StalePage => -32011,
            Self::StaleRevision => -32012,
            Self::InteractionNotReady => -32013,
            Self::InteractionClosed => -32014,
            Self::InteractionInvalidDecision => -32015,
            Self::ActionUnavailable => -32016,
            Self::AssetNotFound => -32017,
            Self::AssetRejected => -32018,
            Self::FrameTooLarge => -32019,
            Self::ClientTooSlow => -32020,
            Self::BadArgument => INVALID_PARAMS,
        }
    }

    pub fn bingo_code(self) -> &'static str {
        match self {
            Self::ProtocolUnsupported => error::PROTOCOL_UNSUPPORTED,
            Self::CapabilityRequired => error::CAPABILITY_REQUIRED,
            Self::NotInitialized => error::NOT_INITIALIZED,
            Self::AlreadyInitialized => error::ALREADY_INITIALIZED,
            Self::NoActiveSession => error::NO_ACTIVE_SESSION,
            Self::SessionNotFound => error::SESSION_NOT_FOUND,
            Self::ConversationNotFound => error::CONVERSATION_NOT_FOUND,
            Self::TurnClosed => error::TURN_CLOSED,
            Self::StalePage => error::STALE_PAGE,
            Self::StaleRevision => error::STALE_REVISION,
            Self::InteractionNotReady => error::INTERACTION_NOT_READY,
            Self::InteractionClosed => error::INTERACTION_CLOSED,
            Self::InteractionInvalidDecision => error::INTERACTION_INVALID_DECISION,
            Self::ActionUnavailable => error::ACTION_UNAVAILABLE,
            Self::AssetNotFound => error::ASSET_NOT_FOUND,
            Self::AssetRejected => error::ASSET_REJECTED,
            Self::FrameTooLarge => error::FRAME_TOO_LARGE,
            Self::ClientTooSlow => error::CLIENT_TOO_SLOW,
            Self::BadArgument => error::SLASH_ERROR_BAD_ARGUMENT,
        }
    }

    /// Whether the same client can usefully continue. Only initialization and
    /// transport failures are not recoverable — they end the connection.
    pub fn recoverable(self) -> bool {
        !matches!(
            self,
            Self::ProtocolUnsupported
                | Self::CapabilityRequired
                | Self::FrameTooLarge
                | Self::ClientTooSlow
        )
    }

    pub fn scope(self) -> ErrorScope {
        match self {
            Self::ProtocolUnsupported
            | Self::CapabilityRequired
            | Self::NotInitialized
            | Self::AlreadyInitialized
            | Self::FrameTooLarge
            | Self::ClientTooSlow => ErrorScope::Connection,
            Self::NoActiveSession | Self::SessionNotFound => ErrorScope::Session,
            Self::ConversationNotFound | Self::StalePage => ErrorScope::Conversation,
            Self::TurnClosed => ErrorScope::Turn,
            Self::StaleRevision => ErrorScope::Session,
            Self::InteractionNotReady
            | Self::InteractionClosed
            | Self::InteractionInvalidDecision => ErrorScope::Interaction,
            Self::ActionUnavailable => ErrorScope::Action,
            Self::AssetNotFound | Self::AssetRejected => ErrorScope::Asset,
            Self::BadArgument => ErrorScope::Request,
        }
    }

    pub fn suggested_action(self) -> Option<SuggestedAction> {
        match self {
            Self::ProtocolUnsupported | Self::CapabilityRequired => None,
            Self::NotInitialized | Self::AlreadyInitialized => Some(SuggestedAction::Reinitialize),
            Self::NoActiveSession | Self::SessionNotFound | Self::StaleRevision => {
                Some(SuggestedAction::RefreshSession)
            }
            Self::ConversationNotFound | Self::StalePage | Self::TurnClosed => {
                Some(SuggestedAction::RefreshConversation)
            }
            Self::InteractionNotReady => Some(SuggestedAction::Retry),
            Self::InteractionClosed | Self::InteractionInvalidDecision => {
                Some(SuggestedAction::RefreshConversation)
            }
            Self::ActionUnavailable => Some(SuggestedAction::RefreshActions),
            Self::AssetNotFound | Self::AssetRejected | Self::BadArgument => {
                Some(SuggestedAction::Resubmit)
            }
            Self::FrameTooLarge | Self::ClientTooSlow => None,
        }
    }

    /// The English sentence that travels with the code. Sanitized by
    /// construction: one line, no interpolated application data.
    pub fn message(self) -> &'static str {
        match self {
            Self::ProtocolUnsupported => "This server does not speak the requested protocol major.",
            Self::CapabilityRequired => "A controlling client must support interaction/respond.",
            Self::NotInitialized => "The connection is not initialized yet.",
            Self::AlreadyInitialized => "The connection is already initialized.",
            Self::NoActiveSession => "No session is open on this connection.",
            Self::SessionNotFound => "No session matches that locator.",
            Self::ConversationNotFound => "The conversation does not exist.",
            Self::TurnClosed => "The turn is no longer active.",
            Self::StalePage => "The page cursor belongs to an older history generation.",
            Self::StaleRevision => "The resource changed since that revision.",
            Self::InteractionNotReady => "The confirmation guard has not elapsed.",
            Self::InteractionClosed => "The interaction is already resolved.",
            Self::InteractionInvalidDecision => "That decision was not offered for this prompt.",
            Self::ActionUnavailable => "The action is not available in this state.",
            Self::AssetNotFound => "The asset does not exist.",
            Self::AssetRejected => "The file could not be registered as an asset.",
            Self::FrameTooLarge => "The frame exceeds the negotiated size limit.",
            Self::ClientTooSlow => "The client is not reading fast enough to stay attached.",
            Self::BadArgument => "The request arguments are invalid.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_declared_error_has_a_distinct_code_and_number() {
        let mut codes: Vec<&str> = Vec::new();
        let mut numbers: Vec<i32> = Vec::new();
        for kind in ProtocolErrorKind::ALL {
            let code = kind.bingo_code();
            assert!(
                !code.is_empty()
                    && code.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                    && code
                        .chars()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
                "{code:?} must match ^[A-Z][A-Z0-9_]*$"
            );
            assert!(!codes.contains(&code), "{code} is declared twice");
            codes.push(code);
            let number = kind.code();
            if number != INVALID_PARAMS {
                assert!(
                    (-32099..=-32000).contains(&number),
                    "{code}: {number} is outside the implementation-defined range"
                );
                assert!(
                    !numbers.contains(&number),
                    "{code}: {number} is already taken"
                );
            }
            numbers.push(number);
        }
        assert_eq!(ProtocolErrorKind::ALL.len(), 19);
    }

    /// The one error number the spec printed by hand.
    #[test]
    fn turn_closed_keeps_the_number_the_spec_published() {
        assert_eq!(ProtocolErrorKind::TurnClosed.code(), -32010);
        assert_eq!(ProtocolErrorKind::TurnClosed.bingo_code(), "TURN_CLOSED");
        assert_eq!(
            ProtocolErrorKind::TurnClosed.message(),
            "The turn is no longer active."
        );
    }

    #[test]
    fn messages_are_single_line_english_sentences() {
        for kind in ProtocolErrorKind::ALL {
            let message = kind.message();
            assert_eq!(
                message,
                error::sanitize_msg(message),
                "{}: the message must already be sanitized",
                kind.bingo_code()
            );
            assert!(
                message.ends_with('.') && message.is_ascii(),
                "{}: {message:?} must be one ascii sentence",
                kind.bingo_code()
            );
        }
    }

    #[test]
    fn an_application_error_carries_its_structured_half() {
        let error = RpcError::application(ProtocolErrorKind::TurnClosed);
        let value = serde_json::to_value(&error).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            value,
            serde_json::json!({
                "code": -32010,
                "message": "The turn is no longer active.",
                "data": {
                    "bingoCode": "TURN_CLOSED",
                    "recoverable": true,
                    "scope": "turn",
                    "suggestedAction": "refreshConversation"
                }
            })
        );
        let decoded: RpcError =
            serde_json::from_value(value).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(decoded, error);
    }

    #[test]
    fn a_standard_error_carries_no_application_data() {
        let error = RpcError::standard(PARSE_ERROR, "Invalid JSON was received.");
        assert_eq!(
            serde_json::to_value(&error).unwrap_or_else(|error| panic!("{error}")),
            serde_json::json!({"code": -32700, "message": "Invalid JSON was received."})
        );
    }
}

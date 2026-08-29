//! Stable error codes. Every error a client can see carries one of these; the
//! text is for people, the code is the contract.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    SessionNotFound,
    SessionLocked,
    SessionClosed,
    InteractionClosed,
    NotReady,
    StaleGeneration,
    NotInitialized,
    InvalidInput,
    PermissionDenied,
    ToolNotFound,
    ToolFailed,
    ProviderUnavailable,
    AuthRequired,
    RateLimited,
    ContextOverflow,
    Timeout,
    Offline,
    ServerError,
    TurnLost,
    TurnBudgetExhausted,
    /// The store could not read or write; the disk, not the kernel.
    Storage,
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::SessionNotFound => "SESSION_NOT_FOUND",
            ErrorCode::SessionLocked => "SESSION_LOCKED",
            ErrorCode::SessionClosed => "SESSION_CLOSED",
            ErrorCode::InteractionClosed => "INTERACTION_CLOSED",
            ErrorCode::NotReady => "NOT_READY",
            ErrorCode::StaleGeneration => "STALE_GENERATION",
            ErrorCode::NotInitialized => "NOT_INITIALIZED",
            ErrorCode::InvalidInput => "INVALID_INPUT",
            ErrorCode::PermissionDenied => "PERMISSION_DENIED",
            ErrorCode::ToolNotFound => "TOOL_NOT_FOUND",
            ErrorCode::ToolFailed => "TOOL_FAILED",
            ErrorCode::ProviderUnavailable => "PROVIDER_UNAVAILABLE",
            ErrorCode::AuthRequired => "AUTH_REQUIRED",
            ErrorCode::RateLimited => "RATE_LIMITED",
            ErrorCode::ContextOverflow => "CONTEXT_OVERFLOW",
            ErrorCode::Timeout => "TIMEOUT",
            ErrorCode::Offline => "OFFLINE",
            ErrorCode::ServerError => "SERVER_ERROR",
            ErrorCode::TurnLost => "TURN_LOST",
            ErrorCode::TurnBudgetExhausted => "TURN_BUDGET_EXHAUSTED",
            ErrorCode::Storage => "STORAGE",
            ErrorCode::Internal => "INTERNAL",
        }
    }
}

/// An error as it appears on the wire and in the journal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[error("[{}] {message}", code.as_str())]
pub struct KernelError {
    pub code: ErrorCode,
    pub message: String,
}

impl KernelError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_form_and_the_str_form_agree() {
        for code in [
            ErrorCode::SessionNotFound,
            ErrorCode::InteractionClosed,
            ErrorCode::TurnBudgetExhausted,
            ErrorCode::Storage,
            ErrorCode::Internal,
        ] {
            let json = serde_json::to_string(&code).unwrap();
            assert_eq!(json, format!("\"{}\"", code.as_str()));
        }
    }
}

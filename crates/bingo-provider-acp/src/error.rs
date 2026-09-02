//! What can go wrong between here and an adapter, and how a person hears it.
//!
//! An ACP adapter owns its own login, so what bingo can say about a failure is
//! mostly *where* it happened: our side of the pipe, their side of the pipe, or
//! the agent refusing in words of its own. The last is worth passing through
//! whole — `claude-agent-acp` says "run `claude login`", and no wording this
//! plugin invents would be better.

use agent_client_protocol_schema::v1::{Error as RpcError, ErrorCode};
use bingo_sdk::ProviderError;

#[derive(Debug, thiserror::Error)]
pub enum AcpError {
    #[error("{0}")]
    Spawn(String),
    #[error("{0}")]
    Transport(String),
    /// The agent answered, and the answer was not the shape its method
    /// promises.
    #[error("{0}")]
    Protocol(String),
    /// The agent refused, in its own words.
    #[error("{}", .0.message)]
    Refused(RpcError),
}

impl AcpError {
    pub fn protocol(what: impl std::fmt::Display) -> Self {
        Self::Protocol(what.to_string())
    }

    pub fn transport(what: impl std::fmt::Display) -> Self {
        Self::Transport(what.to_string())
    }
}

/// A refusal keeps the agent's own wording, and only its code decides which
/// kind of trouble bingo calls it. `AUTH_REQUIRED` is the one that matters:
/// the adapter has no credential and the person must go and get one, which is
/// not something a retry fixes.
impl From<AcpError> for ProviderError {
    fn from(error: AcpError) -> Self {
        match error {
            AcpError::Spawn(message) => ProviderError::Config { message },
            AcpError::Transport(message) => ProviderError::Transport { message },
            AcpError::Protocol(message) => ProviderError::Stream { message },
            AcpError::Refused(rpc) => refused(rpc),
        }
    }
}

fn refused(rpc: RpcError) -> ProviderError {
    let message = rpc.message;
    match rpc.code {
        ErrorCode::AuthRequired => ProviderError::Auth { message },
        ErrorCode::InvalidParams | ErrorCode::InvalidRequest => ProviderError::Request { message },
        ErrorCode::MethodNotFound => ProviderError::Unsupported { message },
        _ => ProviderError::Server {
            status: 500,
            message,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(error: AcpError) -> ProviderError {
        error.into()
    }

    /// An adapter with no login says so through the protocol's own code
    /// (`-32000`, which is what `codex-acp` throws from `session/new`), and a
    /// person must read the adapter's words, not ours: `claude login` is an
    /// instruction only the adapter knows how to give.
    #[test]
    fn an_agent_with_no_credential_is_an_auth_error_in_its_own_words() {
        assert_eq!(i32::from(RpcError::auth_required().code), -32000);
        let told = kind(AcpError::Refused(RpcError::new(
            -32000,
            "run `claude login`",
        )));
        assert!(
            matches!(&told, ProviderError::Auth { message } if message == "run `claude login`")
        );
        assert!(!told.retryable(), "a missing login is not a retry");
    }

    /// A child that died mid-turn is the same trouble as a dropped socket:
    /// the turn may be tried again.
    #[test]
    fn a_pipe_that_closed_is_retryable_transport() {
        assert!(kind(AcpError::transport("the adapter ended")).retryable());
        assert!(!kind(AcpError::Spawn("no such command".into())).retryable());
    }

    #[test]
    fn a_method_the_agent_does_not_have_is_unsupported_not_a_server_fault() {
        assert!(matches!(
            kind(AcpError::Refused(RpcError::method_not_found())),
            ProviderError::Unsupported { .. }
        ));
    }
}

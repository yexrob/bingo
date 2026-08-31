//! What can go wrong between this surface and a platform.

use bingo_sdk::{ErrorCode, KernelError};

#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    /// The adapter cannot start at all: no credential, a bad address, a
    /// second process on one app (ADR-0016 §5).
    #[error("{0}")]
    Refused(String),
    /// The platform answered, and said no.
    #[error("{0}")]
    Platform(String),
    /// The platform did not answer, or answered something unreadable.
    #[error("{0}")]
    Transport(String),
    /// This adapter has no mechanism for what was asked of it.
    #[error("{0} is not something this channel can do")]
    Unsupported(&'static str),
}

impl From<ChannelError> for KernelError {
    fn from(error: ChannelError) -> Self {
        let code = match error {
            ChannelError::Refused(_) => ErrorCode::InvalidInput,
            _ => ErrorCode::Internal,
        };
        KernelError::new(code, error.to_string())
    }
}

impl ChannelError {
    /// An HTTP or socket failure. The message is the caller's context plus
    /// the cause; a credential never appears in either.
    pub fn transport(what: &str, cause: impl std::fmt::Display) -> Self {
        ChannelError::Transport(format!("{what}: {cause}"))
    }
}

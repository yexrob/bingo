//! One failure type for every flow, because a caller's decision is the same
//! whichever flow produced it: sign in, sign in again, or report and stop.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("no stored credential")]
    SignedOut,
    #[error("the stored credential expired: {0}")]
    Expired(String),
    #[error("cancelled")]
    Cancelled,
    #[error("timed out")]
    Timeout,
    #[error("http {status}: {body}")]
    Http { status: u16, body: String },
    #[error("transport: {0}")]
    Transport(String),
    #[error("credential store: {0}")]
    Store(String),
    #[error("invalid: {0}")]
    Invalid(String),
}

impl AuthError {
    /// A non-success reply from the issuer, classified: a refresh token the
    /// issuer has retired can never work again, so the entry goes rather than
    /// the caller retrying forever.
    pub fn http(status: u16, body: String) -> Self {
        if permanent(&body) {
            AuthError::Expired(body)
        } else {
            AuthError::Http { status, body }
        }
    }
}

/// A port that will not bind or a socket that will not read is transport; a
/// request that is not a redirect is a redirect this flow cannot use.
impl From<bingo_loopback::LoopbackError> for AuthError {
    fn from(error: bingo_loopback::LoopbackError) -> Self {
        use bingo_loopback::LoopbackError;
        match &error {
            LoopbackError::NoPort(_) | LoopbackError::Io(_) => {
                AuthError::Transport(error.to_string())
            }
            LoopbackError::Malformed(_) | LoopbackError::TooLarge(_) | LoopbackError::Answer(_) => {
                AuthError::Invalid(error.to_string())
            }
        }
    }
}

impl From<reqwest::Error> for AuthError {
    fn from(error: reqwest::Error) -> Self {
        AuthError::Transport(error.to_string())
    }
}

/// Whether the issuer said the refresh token itself is finished (codex's own
/// wording). Anything else — a 500, a timeout — is worth another attempt.
pub fn permanent(body: &str) -> bool {
    [
        "refresh_token_expired",
        "refresh_token_reused",
        "refresh_token_invalidated",
    ]
    .iter()
    .any(|reason| body.contains(reason))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_retired_refresh_tokens_are_permanent_and_nothing_else_is() {
        assert!(permanent(r#"{"error":"refresh_token_expired"}"#));
        assert!(permanent(r#"{"error":"refresh_token_reused"}"#));
        assert!(permanent(r#"{"error":"refresh_token_invalidated"}"#));
        assert!(!permanent(r#"{"error":"server_error"}"#));
        assert!(!permanent(""));
    }

    #[test]
    fn a_permanent_body_classifies_as_expired_whatever_the_status() {
        assert!(matches!(
            AuthError::http(400, "refresh_token_reused".into()),
            AuthError::Expired(_)
        ));
        assert!(matches!(
            AuthError::http(500, "upstream is unwell".into()),
            AuthError::Http { status: 500, .. }
        ));
    }
}

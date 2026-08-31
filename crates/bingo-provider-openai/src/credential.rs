//! What the provider puts in its `Authorization` header, and what it says
//! when it cannot.
//!
//! Two kinds of credential: a key, which is pasted or configured and does not
//! expire (`key.rs`), and a subscription bearer, which only an OAuth flow
//! yields and which does. The difference is one enum here rather than a
//! branch in every method, and the hints are written once so a refusal at
//! session open and a dialog inside a session read alike.

use std::sync::Arc;

use bingo_auth_oauth::{AuthError, Status, TokenSource};
use bingo_sdk::{AuthStatus, ProviderError};

use crate::key::ApiKey;

#[derive(Debug)]
pub enum Credential {
    /// An API key: the store, then the settings or the environment
    /// (ADR-0017 §3).
    Key(ApiKey),
    /// A subscription token set, refreshed in place (ADR-0012 §3).
    Tokens(Arc<TokenSource>),
}

impl Credential {
    /// The bearer for one request. A `Tokens` credential may renew here, so
    /// the first request of a turn is already up to date.
    pub async fn bearer(&self) -> Result<String, ProviderError> {
        match self {
            Credential::Key(key) => key.bearer(),
            Credential::Tokens(source) => source
                .access_token()
                .await
                .map_err(|error| failure(source.provider(), error)),
        }
    }

    /// Synchronous, so the kernel can refuse a session before a turn starts.
    pub fn status(&self) -> AuthStatus {
        match self {
            Credential::Key(key) => key.status(),
            Credential::Tokens(source) => match source.status() {
                Status::SignedIn { .. } => AuthStatus::Ready,
                Status::SignedOut => AuthStatus::Missing {
                    hint: sign_in(source.provider()),
                },
                Status::Expired { .. } => AuthStatus::Expired {
                    hint: sign_in_again(source.provider()),
                },
            },
        }
    }
}

/// Named after the provider rather than after codex, so a second
/// subscription provider reads right the day it exists.
pub fn sign_in(provider: &str) -> String {
    format!("Run `bingo login {provider}`, or `/login {provider}` in a session.")
}

pub fn sign_in_again(provider: &str) -> String {
    format!("Run `bingo login {provider}` to sign in again.")
}

/// One translation from the library's vocabulary to the sdk's, used by the
/// bearer path and the login path alike.
pub fn failure(provider: &str, error: AuthError) -> ProviderError {
    match error {
        AuthError::SignedOut => ProviderError::Auth {
            message: sign_in(provider),
        },
        AuthError::Expired(_) => ProviderError::Auth {
            message: sign_in_again(provider),
        },
        AuthError::Cancelled => ProviderError::Auth {
            message: "Sign-in cancelled.".into(),
        },
        AuthError::Timeout => ProviderError::Timeout,
        AuthError::Transport(message) => ProviderError::Transport { message },
        AuthError::Http { status, body } if status >= 500 => ProviderError::Server {
            status,
            message: body,
        },
        AuthError::Http { status, body } => ProviderError::Auth {
            message: format!("the issuer answered {status}: {body}"),
        },
        other => ProviderError::Auth {
            message: other.to_string(),
        },
    }
}

//! What the provider puts in its `Authorization` header, and what it says
//! when it cannot.
//!
//! Two endpoints, two kinds of credential: the public API takes a key from
//! settings or the environment, the ChatGPT subscription takes a bearer that
//! only an OAuth flow yields and that expires. The difference is one enum
//! here rather than a branch in every method, and the hints are written once
//! so a refusal at session open and a dialog inside a session read alike.

use std::sync::Arc;

use bingo_auth_oauth::{AuthError, Status, TokenSource};
use bingo_sdk::{AuthStatus, ProviderError};

use crate::API_KEY_ENV;

#[derive(Debug)]
pub enum Credential {
    /// An API key, absent until someone configures one.
    Key(Option<String>),
    /// A subscription token set, refreshed in place (ADR-0012 §3).
    Tokens(Arc<TokenSource>),
}

impl Credential {
    /// The bearer for one request. A `Tokens` credential may renew here, so
    /// the first request of a turn is already up to date.
    pub async fn bearer(&self) -> Result<String, ProviderError> {
        match self {
            Credential::Key(Some(key)) => Ok(key.clone()),
            Credential::Key(None) => Err(ProviderError::Auth {
                message: format!(
                    "no OpenAI API key: set {API_KEY_ENV} or the `openai.apiKey` setting"
                ),
            }),
            Credential::Tokens(source) => source
                .access_token()
                .await
                .map_err(|error| failure(source.provider(), error)),
        }
    }

    /// Synchronous, so the kernel can refuse a session before a turn starts.
    /// `missing_key` is the provider's to write: only it knows which settings
    /// file a person would put a key in.
    pub fn status(&self, missing_key: impl FnOnce() -> String) -> AuthStatus {
        match self {
            Credential::Key(Some(_)) => AuthStatus::Ready,
            Credential::Key(None) => AuthStatus::Missing {
                hint: missing_key(),
            },
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

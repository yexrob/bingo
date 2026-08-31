//! A model provider: one wire protocol, streaming, in the neutral vocabulary.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::host::Prompter;
use crate::model::{EndpointCapabilities, ModelRequest, ModelStream, ProviderError};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AuthStatus {
    NotApplicable,
    Ready,
    /// No credentials; `hint` says where to put them, in the user's words.
    Missing {
        hint: String,
    },
    /// Credentials that stopped working; `hint` says how to renew them.
    Expired {
        hint: String,
    },
}

/// How a person wants to sign in (ADR-0012 §4); `None` is the provider's
/// default. The flow that results is what `LoginFlow` shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum LoginMethod {
    /// A browser on this machine and a loopback callback.
    Browser,
    /// A code entered in a browser anywhere; polled here.
    Device,
    /// A credential minted elsewhere, pasted in.
    Paste,
}

impl LoginMethod {
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "browser" => Some(Self::Browser),
            "device" => Some(Self::Device),
            "paste" => Some(Self::Paste),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// The provider id configuration refers to (`anthropic`, `openai`, `codex`, `fake`).
    fn id(&self) -> &str;

    /// The model family this provider serves — what a model catalogue files
    /// its models under. A named instance (ADR-0017) answers the shape it
    /// speaks (`openai`, `anthropic`, `codex`); the default is the id, which
    /// is where the built-ins are filed.
    fn family(&self) -> &str {
        self.id()
    }

    /// What this endpoint does with a request for `model`. Fails closed: a
    /// provider that does not know says `false`.
    fn endpoint(&self, model: &str) -> EndpointCapabilities;

    /// Stream one response. Non-streaming completion is this, drained.
    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ModelStream, ProviderError>;

    async fn count_tokens(&self, _request: &ModelRequest) -> Result<u64, ProviderError> {
        Err(ProviderError::Unsupported {
            message: "count_tokens".into(),
        })
    }

    async fn models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        Ok(Vec::new())
    }

    fn auth(&self) -> AuthStatus {
        AuthStatus::NotApplicable
    }

    /// Interactive login (ADR-0012 §4): an OAuth flow shows itself through
    /// the prompter as `InteractionKind::Login` and answers with a receipt
    /// a person can read (`Signed in to codex as …`).
    async fn login(
        &self,
        _prompter: Arc<dyn Prompter>,
        _method: Option<LoginMethod>,
    ) -> Result<String, ProviderError> {
        Err(ProviderError::Unsupported {
            message: "login".into(),
        })
    }

    /// Forget the stored credential, revoking it where the issuer allows.
    async fn logout(&self) -> Result<String, ProviderError> {
        Err(ProviderError::Unsupported {
            message: "logout".into(),
        })
    }
}

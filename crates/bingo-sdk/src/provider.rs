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

    /// Interactive login (OAuth flows ask through the prompter).
    async fn login(&self, _prompter: Arc<dyn Prompter>) -> Result<(), ProviderError> {
        Err(ProviderError::Unsupported {
            message: "login".into(),
        })
    }
}

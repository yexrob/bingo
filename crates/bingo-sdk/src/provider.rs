//! A model provider: one wire protocol, streaming, in the neutral vocabulary.

use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::host::Prompter;
use crate::model::{ModelCapabilities, ModelRequest, ModelStream, ProviderError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AuthStatus {
    NotApplicable,
    Ready,
    Missing,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<ModelCapabilities>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    /// The provider id configuration refers to (`anthropic`, `openai`, `codex`, `fake`).
    fn id(&self) -> &str;

    fn capabilities(&self, model: &str) -> ModelCapabilities;

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

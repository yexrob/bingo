//! Which provider and which model a session runs on (ADR-0004): the list
//! every reader of it reads, the one a person named or the build picks, the
//! model id, and the four owners of that model's facts resolved into one.

use std::sync::Arc;

use bingo_sdk::*;

use super::{Host, auth};
use crate::models::{self, ModelCatalog};
use crate::turn::{ModelChoice, ProviderSet};

impl Host {
    /// Every provider this host can choose from, resolved at the one point
    /// they are resolved (ADR-0030 §2): the registered ones and whatever the
    /// sources answer with now. `provider`, the catalogue and `/model`'s
    /// reading of `<provider>/<model>` all read this and nothing else.
    pub async fn providers(&self) -> Vec<Arc<dyn Provider>> {
        ProviderSet {
            fixed: self.registry.providers.clone(),
            sources: self.registry.sources.providers.clone(),
        }
        .gather()
        .await
    }

    /// The provider `id` names; with none, the settings' provider; with
    /// neither, the first that can actually answer.
    pub async fn provider(&self, id: Option<&str>) -> Result<Arc<dyn Provider>, KernelError> {
        let providers = self.providers().await;
        let named = id
            .map(str::to_string)
            .or_else(|| self.settings.kernel.provider.clone());
        match named {
            Some(wanted) => by_id(&providers, &wanted),
            None => default_provider(&providers),
        }
    }

    pub(super) async fn model(
        &self,
        provider: &dyn Provider,
        wanted: Option<&str>,
    ) -> Result<String, KernelError> {
        if let Some(model) = wanted
            .map(str::to_string)
            .or_else(|| self.settings.kernel.model.clone())
        {
            return Ok(model);
        }
        let models = provider
            .models()
            .await
            .map_err(|e| KernelError::new(ErrorCode::ProviderUnavailable, e.to_string()))?;
        models.first().map(|m| m.id.clone()).ok_or_else(|| {
            KernelError::new(
                ErrorCode::InvalidInput,
                format!("no model configured for provider {}", provider.id()),
            )
        })
    }

    /// The model a spec runs on: none for a `Log` session (ADR-0011 §1),
    /// which resolves no provider and calls none.
    pub(super) async fn model_for(
        &self,
        spec: &SessionSpec,
    ) -> Result<Option<ModelChoice>, KernelError> {
        match spec.driver {
            Driver::Model => Ok(Some(self.choose_model(spec).await?)),
            Driver::Log => Ok(None),
        }
    }

    /// The spec holds the level too (ADR-0047 §1), resolved at open: a level
    /// no model reasons at reaches no request, and that is what the turn
    /// asks for.
    pub(super) async fn choose_model(
        &self,
        spec: &SessionSpec,
    ) -> Result<ModelChoice, KernelError> {
        let provider = self.provider(spec.provider.as_deref()).await?;
        auth::check(provider.as_ref())?;
        let model = self.model(provider.as_ref(), spec.model.as_deref()).await?;
        let capabilities = self.resolve_model(provider.as_ref(), &model);
        Ok(ModelChoice {
            max_tokens: models::max_tokens(&capabilities, self.settings.kernel.max_tokens),
            reasoning: spec.thinking.flatten().filter(|_| capabilities.reasoning),
            learned: self.learned.clone(),
            provider,
            id: model,
            capabilities,
        })
    }

    /// The four owners of a model's facts, read once per session (ADR-0004).
    pub(super) fn resolve_model(&self, provider: &dyn Provider, model: &str) -> ModelCapabilities {
        let key = models::declared::key(provider.id(), model);
        models::resolve(
            self.settings.kernel.models.get(&key),
            self.learned.window(provider.id(), model),
            ModelCatalog::embedded().facts_for(provider.family(), provider.id(), model),
            provider.endpoint(model),
        )
    }
}

/// The provider registered under this id.
fn by_id(providers: &[Arc<dyn Provider>], wanted: &str) -> Result<Arc<dyn Provider>, KernelError> {
    providers
        .iter()
        .find(|p| p.id() == wanted)
        .cloned()
        .ok_or_else(|| {
            let known: Vec<&str> = providers.iter().map(|p| p.id()).collect();
            KernelError::new(
                ErrorCode::ProviderUnavailable,
                format!(
                    "No provider called `{wanted}`. Registered: {}.",
                    known.join(", ")
                ),
            )
        })
}

/// Nobody named one, so the build picks: the first in registration order
/// whose credentials are in place. Registration order alone would hand a
/// person the provider they have never signed in to while another is ready
/// and waiting, and [`auth::check`] would then refuse the run outright.
fn default_provider(providers: &[Arc<dyn Provider>]) -> Result<Arc<dyn Provider>, KernelError> {
    if providers.is_empty() {
        return Err(KernelError::new(
            ErrorCode::ProviderUnavailable,
            "No model provider is registered in this build.",
        ));
    }
    providers
        .iter()
        .find(|p| auth::signed_in(p.as_ref()))
        .cloned()
        .ok_or_else(|| auth::nobody_signed_in(providers))
}

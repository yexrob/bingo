//! Which provider and which model a session runs on (ADR-0004): the list
//! every reader of it reads, the one a person named or the build picks, the
//! model id, and the four owners of that model's facts resolved into one.

use std::sync::Arc;

use bingo_sdk::*;

use super::{Host, auth};
use crate::models::{self, ModelCatalog};
use crate::turn::{Endpoint, ModelChoice, ProviderSet};

impl Host {
    /// Every provider this host can choose from, resolved at the one point
    /// they are resolved (ADR-0030 §2): the registered ones and whatever the
    /// sources answer with now. `provider`, the catalogue and `/model`'s
    /// reading of `<provider>/<model>` all read this and nothing else.
    pub async fn providers(&self) -> Vec<Arc<dyn Provider>> {
        let providers = ProviderSet {
            fixed: self.registry.provider_state.registered.clone(),
            sources: self.registry.sources.providers.clone(),
        }
        .gather()
        .await;
        let instances = &self.registry.provider_state.instances;
        for held in self
            .registry
            .provider_state
            .registered
            .iter()
            .filter(|p| p.instance())
        {
            if let Some(winner) = providers
                .iter()
                .find(|p| p.id() == held.id() && !p.instance())
            {
                instances.skip(
                    held.id(),
                    self.instance_collision(held.as_ref(), winner.as_ref()),
                );
            }
        }
        instances.gather(providers, |held, winner| {
            self.instance_collision(held, winner)
        })
    }

    fn instance_collision(&self, held: &dyn Provider, winner: &dyn Provider) -> String {
        super::instances::collision(
            held,
            winner.plugin().unwrap_or_else(|| winner.family()),
            &self.config.layers,
            &self.config.env.user_settings().display().to_string(),
        )
    }

    pub(super) async fn restorable_provider(&self, id: &str) -> bool {
        self.providers()
            .await
            .iter()
            .any(|provider| provider.id() == id && auth::signed_in(provider.as_ref()))
            || self.registry.agents.iter().any(|agent| agent.id() == id)
    }

    /// The provider `id` names; with none, the settings' provider; with
    /// neither, the first that can actually answer.
    pub async fn provider(&self, id: Option<&str>) -> Result<Arc<dyn Provider>, KernelError> {
        let named = id.or_else(|| configured_provider(self)).map(str::to_owned);
        let providers = self.providers().await;
        match named.as_deref() {
            Some(wanted) => by_id(&providers, wanted),
            None => default_provider(&providers),
        }
    }

    pub(super) async fn model(
        &self,
        provider: &dyn Provider,
        wanted: Option<&str>,
    ) -> Result<String, KernelError> {
        if let Some(model) = wanted {
            return Ok(model.into());
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

    /// The spec holds the level too (ADR-0047 §1), resolved at open. Model
    /// metadata never vetoes it: only `off` omits the reasoning parameter.
    pub(super) async fn choose_model(
        &self,
        spec: &SessionSpec,
    ) -> Result<ModelChoice, KernelError> {
        // Startup may publish a late provider between awaits. Capture both
        // defaults together, including absence, before gathering any source.
        let (provider_default, model_default) = configured_pair(self);
        let named = spec
            .provider
            .as_deref()
            .or(provider_default)
            .map(str::to_owned);
        let model = spec.model.as_deref().or(model_default).map(str::to_owned);
        let providers = self.providers().await;
        let agent = self
            .registry
            .agents
            .iter()
            .find(|a| named.as_deref().is_some_and(|n| n == a.id()));
        let agent = match agent {
            Some(agent) => Some(agent),
            None if named.is_none() && providers.is_empty() => self.registry.agents.first(),
            None => None,
        };
        if let Some(agent) = agent {
            return self.choose_agent(agent.clone(), spec, model).await;
        }
        let provider = match named.as_deref() {
            Some(id) => by_id(&providers, id),
            None => default_provider(&providers),
        }?;
        auth::check(provider.as_ref())?;
        let model = self.model(provider.as_ref(), model.as_deref()).await?;
        let models::Resolved {
            capabilities,
            window_known,
        } = self.resolve_model(provider.as_ref(), &model);
        Ok(ModelChoice {
            max_tokens: models::max_tokens(&capabilities, self.settings.kernel.max_tokens),
            window_known,
            reasoning: spec.thinking.flatten(),
            learned: self.learned.clone(),
            endpoint: Endpoint::Provider(provider),
            id: model,
            capabilities,
        })
    }

    async fn choose_agent(
        &self,
        agent: Arc<dyn Agent>,
        spec: &SessionSpec,
        wanted: Option<String>,
    ) -> Result<ModelChoice, KernelError> {
        let id = match wanted {
            Some(id) => id,
            None => agent
                .models()
                .await
                .map_err(|e| KernelError::new(ErrorCode::ProviderUnavailable, e.to_string()))?
                .first()
                .map(|m| m.id.clone())
                .ok_or_else(|| {
                    KernelError::new(ErrorCode::InvalidInput, "the agent named no models")
                })?,
        };
        let key = models::declared::key(agent.id(), &id);
        let models::Resolved {
            capabilities,
            window_known,
        } = models::resolve(
            self.settings.kernel.models.get(&key),
            self.learned.window(agent.id(), &id),
            None,
            EndpointCapabilities {
                images: true,
                ..Default::default()
            },
        );
        Ok(ModelChoice {
            endpoint: Endpoint::Agent(agent),
            id,
            max_tokens: models::max_tokens(&capabilities, None),
            capabilities,
            window_known,
            reasoning: spec.thinking.flatten(),
            learned: self.learned.clone(),
        })
    }

    /// The four owners of a model's facts, read once per session (ADR-0004).
    pub(super) fn resolve_model(&self, provider: &dyn Provider, model: &str) -> models::Resolved {
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
pub(super) fn configured_provider(host: &Host) -> Option<&str> {
    configured_pair(host).0
}

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

pub(super) fn configured_model(host: &Host) -> Option<String> {
    configured_pair(host).1.map(str::to_owned)
}

fn configured_pair(host: &Host) -> (Option<&str>, Option<&str>) {
    if host
        .registry
        .provider_state
        .default_cleared
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return (None, None);
    }
    match host.remembered.get() {
        Some(choice) => (Some(&choice.provider), Some(&choice.model)),
        None => (
            host.settings.kernel.provider.as_deref(),
            host.settings.kernel.model.as_deref(),
        ),
    }
}

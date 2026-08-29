//! The plugin host. It loads plugins in the order the binary hands them
//! over, refuses a plugin whose requirements no earlier plugin provides,
//! collects every contribution into one registry, and serves `HostApi` over
//! a map of session actors. It knows no plugin by name.

mod catalog;
mod registry;
mod tool_host;

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use bingo_sdk::*;
use jiff::Timestamp;
use serde_json::Value;
use tokio::sync::broadcast;

pub use registry::{PluginStatus, Registry};
use tool_host::SessionToolHost;

use crate::gate::DefaultPolicy;
use crate::prompt::{self, PromptInput};
use crate::session::{self, Mailbox};
use crate::settings::{self, Claim, Layer, Merged, SettingsError};
use crate::turn::{TurnBudget, TurnConfig};

/// Output tokens a turn may ask for when neither the model nor the config says.
const DEFAULT_MAX_TOKENS: u64 = 8_192;

/// Gateway events buffered per subscriber before the oldest is dropped.
const GATEWAY_CAPACITY: usize = 64;

#[derive(Clone, Debug)]
pub struct HostConfig {
    /// Settings layers, lowest priority first; the command line is the last.
    pub layers: Vec<Layer>,
    /// A system block appended after the kernel's own (hosts, tests).
    pub extra_system: Option<String>,
    pub budget: TurnBudget,
    pub env: Env,
    /// How deep a chain of sub-sessions may go; 1 = one level below a root.
    pub max_child_depth: u32,
    /// Live children one session may have.
    pub max_children: usize,
}

impl HostConfig {
    pub fn new(env: Env) -> Self {
        Self {
            layers: Vec::new(),
            extra_system: None,
            budget: TurnBudget::default(),
            env,
            max_child_depth: 1,
            max_children: 20,
        }
    }

    /// Add the highest-priority layer so far; a non-object is ignored.
    pub fn with_layer(mut self, source: &str, value: Value) -> Self {
        if let Value::Object(map) = value {
            self.layers.push(Layer::new(source, map));
        }
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("plugin {plugin} failed to register: {source}")]
    Register {
        plugin: String,
        #[source]
        source: PluginError,
    },
    #[error("plugin {plugin} conflicts with an earlier one: {what}")]
    Conflict { plugin: String, what: String },
    #[error("plugin {plugin} failed to start: {source}")]
    Start {
        plugin: String,
        #[source]
        source: PluginError,
    },
    #[error(transparent)]
    Settings(#[from] SettingsError),
}

pub struct Host {
    config: HostConfig,
    settings: Merged,
    registry: Registry,
    plugins: Vec<Box<dyn Plugin>>,
    sessions: Mutex<BTreeMap<SessionId, Live>>,
    gateway: broadcast::Sender<GatewayEvent>,
    weak: Weak<Host>,
}

#[derive(Clone)]
struct Live {
    mailbox: Mailbox,
    key: Option<String>,
    cwd: String,
    parent: Option<SessionId>,
    created_at: Timestamp,
}

impl Live {
    fn new(mailbox: Mailbox, summary: &SessionSummary) -> Self {
        Self {
            mailbox,
            key: summary.key.clone(),
            cwd: summary.cwd.clone(),
            parent: summary.parent.as_ref().map(|p| p.session.clone()),
            created_at: summary.created_at,
        }
    }
}

/// The provider and model a new session runs on, with the ceiling that follows.
struct ModelChoice {
    provider: Arc<dyn Provider>,
    model: String,
    capabilities: ModelCapabilities,
    max_tokens: u32,
}

impl std::fmt::Debug for Host {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Host")
            .field("plugins", &self.registry.plugins)
            .finish_non_exhaustive()
    }
}

impl Host {
    /// Register every plugin in order, then start the enabled ones.
    pub async fn build(
        plugins: Vec<Box<dyn Plugin>>,
        config: HostConfig,
    ) -> Result<Arc<Host>, HostError> {
        let claims: Vec<Claim> = plugins
            .iter()
            .filter_map(|p| Claim::from_manifest(p.manifest()))
            .collect();
        let settings = settings::merge(&config.layers, &claims)?;
        let registry = Registry::load(&plugins, &settings.plugins, &config.env)?;
        let (gateway, _) = broadcast::channel(GATEWAY_CAPACITY);
        let host = Arc::new_cyclic(|weak| Host {
            config,
            settings,
            registry,
            plugins,
            sessions: Mutex::new(BTreeMap::new()),
            gateway,
            weak: weak.clone(),
        });
        host.start_plugins().await?;
        Ok(host)
    }

    /// Start the enabled plugins in load order; each receives a host handle.
    async fn start_plugins(&self) -> Result<(), HostError> {
        for plugin in &self.plugins {
            let manifest = plugin.manifest();
            if !self.registry.enabled(manifest.id) {
                continue;
            }
            plugin
                .start(self.handle())
                .await
                .map_err(|source| HostError::Start {
                    plugin: manifest.id.to_string(),
                    source,
                })?;
        }
        Ok(())
    }

    pub fn handle(&self) -> HostHandle {
        // The host is only ever built behind an `Arc`; a handle asked for
        // during teardown would be the only way to see `None`.
        match self.weak.upgrade() {
            Some(host) => HostHandle(host),
            None => HostHandle(Arc::new(Unavailable)),
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// What startup found worth telling a person: `(code, text)` pairs.
    pub fn notices(&self) -> Vec<(String, String)> {
        self.settings
            .unknown
            .iter()
            .map(|u| {
                (
                    "UNKNOWN_SETTING".to_string(),
                    format!("unknown setting `{}` in {}", u.key, u.source),
                )
            })
            .collect()
    }

    pub fn surface(&self, id: &str) -> Option<Arc<dyn Surface>> {
        self.registry
            .surfaces
            .iter()
            .find(|s| s.id() == id)
            .cloned()
    }

    /// Close every session and stop every plugin, in reverse order.
    pub async fn shutdown(&self) {
        let live: Vec<Live> = self.lock().values().cloned().collect();
        for session in live {
            session.mailbox.close(CloseReason::Shutdown);
        }
        for plugin in self.plugins.iter().rev() {
            if !self.registry.enabled(plugin.manifest().id) {
                continue;
            }
            if let Err(e) = plugin.stop().await {
                tracing::warn!(plugin = plugin.manifest().id, error = %e, "plugin stop failed");
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<SessionId, Live>> {
        self.sessions.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn live(&self, id: &SessionId) -> Result<Live, KernelError> {
        self.lock()
            .get(id)
            .cloned()
            .ok_or_else(|| KernelError::new(ErrorCode::SessionNotFound, format!("no session {id}")))
    }

    fn depth(&self, id: &SessionId) -> u32 {
        let sessions = self.lock();
        let mut depth = 0;
        let mut cursor = sessions.get(id).and_then(|l| l.parent.clone());
        while let Some(parent) = cursor {
            depth += 1;
            cursor = sessions.get(&parent).and_then(|l| l.parent.clone());
        }
        depth
    }

    fn provider(&self, id: Option<&str>) -> Result<Arc<dyn Provider>, KernelError> {
        let wanted = id
            .map(str::to_string)
            .or_else(|| self.settings.kernel.provider.clone())
            .or_else(|| self.registry.providers.first().map(|p| p.id().to_string()))
            .ok_or_else(|| {
                KernelError::new(
                    ErrorCode::ProviderUnavailable,
                    "No model provider is registered in this build.",
                )
            })?;
        self.registry
            .providers
            .iter()
            .find(|p| p.id() == wanted)
            .cloned()
            .ok_or_else(|| {
                let known: Vec<&str> = self.registry.providers.iter().map(|p| p.id()).collect();
                KernelError::new(
                    ErrorCode::ProviderUnavailable,
                    format!(
                        "No provider called `{wanted}`. Registered: {}.",
                        known.join(", ")
                    ),
                )
            })
    }

    async fn model(
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

    /// A sub-session may go neither deeper nor wider than the host allows.
    fn check_parent_limits(&self, parent: &SessionId) -> Result<(), KernelError> {
        self.live(parent)?;
        if self.depth(parent) + 1 > self.config.max_child_depth {
            return Err(KernelError::new(
                ErrorCode::InvalidInput,
                format!(
                    "sub-session depth limit {} reached",
                    self.config.max_child_depth
                ),
            ));
        }
        let children = self
            .lock()
            .values()
            .filter(|l| l.parent.as_ref() == Some(parent))
            .count();
        if children >= self.config.max_children {
            return Err(KernelError::new(
                ErrorCode::InvalidInput,
                format!("sub-session limit {} reached", self.config.max_children),
            ));
        }
        Ok(())
    }

    /// A routing key names one live session at a time.
    fn check_key_free(&self, key: Option<&str>) -> Result<(), KernelError> {
        let Some(key) = key else { return Ok(()) };
        if self.lock().values().any(|l| l.key.as_deref() == Some(key)) {
            return Err(KernelError::new(
                ErrorCode::SessionLocked,
                format!("session key {key} is in use"),
            ));
        }
        Ok(())
    }

    async fn choose_model(&self, spec: &SessionSpec) -> Result<ModelChoice, KernelError> {
        let provider = self.provider(spec.provider.as_deref())?;
        check_auth(provider.as_ref())?;
        let model = self.model(provider.as_ref(), spec.model.as_deref()).await?;
        let endpoint = provider.endpoint(&model);
        let capabilities = ModelCapabilities {
            context_window: 200_000,
            max_output: DEFAULT_MAX_TOKENS,
            images: endpoint.images,
            reasoning: false,
            count_tokens: endpoint.count_tokens,
            caching: endpoint.caching,
        };
        let max_tokens = self
            .settings
            .kernel
            .max_tokens
            .unwrap_or_else(|| capabilities.max_output.min(DEFAULT_MAX_TOKENS) as u32);
        Ok(ModelChoice {
            provider,
            model,
            capabilities,
            max_tokens,
        })
    }

    fn summarize(&self, spec: &SessionSpec, choice: &ModelChoice) -> SessionSummary {
        let now = Timestamp::now();
        SessionSummary {
            id: SessionId::mint(),
            key: spec.key.clone(),
            title: spec.title.clone(),
            cwd: spec.cwd.display().to_string(),
            parent: spec.parent.clone(),
            model: Some(choice.model.clone()),
            provider: Some(choice.provider.id().to_string()),
            created_at: now,
            updated_at: now,
            usage: Usage::default(),
            busy: false,
        }
    }

    /// The registered tools this session may call; `None` means every one.
    fn tools_for(&self, wanted: Option<&[String]>) -> Vec<Arc<dyn Tool>> {
        self.registry
            .tools
            .iter()
            .filter(|t| wanted.is_none_or(|names| names.contains(&t.spec().name)))
            .cloned()
            .collect()
    }

    /// The host prompt, cached, then whatever this session adds on top.
    fn system_blocks(&self, spec: &SessionSpec, choice: &ModelChoice) -> Vec<SystemBlock> {
        let mut system = prompt::system_blocks(&PromptInput {
            cwd: &spec.cwd,
            provider: choice.provider.id(),
            model: &choice.model,
            platform: std::env::consts::OS,
            date: jiff::Zoned::now().date(),
        });
        let extras = [
            self.config.extra_system.as_deref(),
            spec.system_extra.as_deref(),
        ];
        system.extend(extras.into_iter().flatten().map(|text| SystemBlock {
            text: text.to_string(),
            cache: false,
        }));
        system
    }

    fn turn_config(
        &self,
        spec: &SessionSpec,
        summary: &SessionSummary,
        choice: ModelChoice,
        mailbox: &Mailbox,
    ) -> TurnConfig {
        let system = self.system_blocks(spec, &choice);
        TurnConfig {
            session: summary.clone(),
            cwd: spec.cwd.clone(),
            provider: choice.provider,
            model: choice.model,
            capabilities: choice.capabilities,
            max_tokens: choice.max_tokens,
            reasoning: self.settings.kernel.thinking,
            system,
            tools: self.tools_for(spec.tools.as_deref()),
            policy: self
                .registry
                .policy
                .clone()
                .unwrap_or_else(|| Arc::new(DefaultPolicy)),
            hooks: self.registry.hooks.clone(),
            contributors: self.registry.contributors.clone(),
            compactor: self.registry.compactor.clone(),
            budget: self.config.budget,
            env: Arc::new(self.config.env.clone()),
            tool_host: Arc::new(SessionToolHost {
                mailbox: mailbox.clone(),
                host: self.weak.clone(),
            }),
        }
    }

    async fn create(&self, spec: SessionSpec) -> Result<Mailbox, KernelError> {
        if let Some(parent) = &spec.parent {
            self.check_parent_limits(&parent.session)?;
        }
        self.check_key_free(spec.key.as_deref())?;
        let choice = self.choose_model(&spec).await?;
        let summary = self.summarize(&spec, &choice);
        if let Some(store) = &self.registry.store {
            store.create(&summary).await?;
        }
        let mailbox = session::spawn(summary.clone(), self.registry.store.clone(), |mailbox| {
            Arc::new(self.turn_config(&spec, &summary, choice, mailbox))
        });
        let live = Live::new(mailbox, &summary);
        self.lock().insert(summary.id.clone(), live.clone());
        let _ = self.gateway.send(GatewayEvent::SessionCreated {
            summary: Box::new(summary),
        });
        Ok(live.mailbox)
    }

    fn resolve(&self, selector: SessionSelector) -> Result<Option<Mailbox>, KernelError> {
        let sessions = self.lock();
        let found = match selector {
            SessionSelector::Create { .. } => return Ok(None),
            SessionSelector::ById { id } => sessions.get(&id).map(|l| l.mailbox.clone()),
            SessionSelector::ByKey { key } => sessions
                .values()
                .find(|l| l.key.as_deref() == Some(key.as_str()))
                .map(|l| l.mailbox.clone()),
            SessionSelector::Latest { cwd } => {
                let cwd = cwd.display().to_string();
                sessions
                    .values()
                    .filter(|l| l.cwd == cwd)
                    .max_by_key(|l| l.created_at)
                    .map(|l| l.mailbox.clone())
            }
        };
        found
            .map(Some)
            .ok_or_else(|| KernelError::new(ErrorCode::SessionNotFound, "no such session"))
    }
}

/// A provider that cannot authenticate is refused before any turn is spent on it.
fn check_auth(provider: &dyn Provider) -> Result<(), KernelError> {
    let refuse = |message: String| Err(KernelError::new(ErrorCode::AuthRequired, message));
    match provider.auth() {
        AuthStatus::Ready | AuthStatus::NotApplicable => Ok(()),
        AuthStatus::Missing { hint } => refuse(format!(
            "The {} provider has no credentials. {hint}",
            provider.id()
        )),
        AuthStatus::Expired { hint } => refuse(format!(
            "The {} provider's credentials have expired. {hint}",
            provider.id()
        )),
    }
}

#[async_trait]
impl HostApi for Host {
    async fn sessions(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        let live: Vec<Live> = self.lock().values().cloned().collect();
        let mut out = Vec::new();
        for session in live {
            if filter
                .cwd
                .as_ref()
                .is_some_and(|cwd| cwd.display().to_string() != session.cwd)
            {
                continue;
            }
            if filter
                .parent
                .as_ref()
                .is_some_and(|p| session.parent.as_ref() != Some(p))
            {
                continue;
            }
            if let Ok(summary) = session.mailbox.summary().await {
                out.push(summary);
            }
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        if let Some(limit) = filter.limit {
            out.truncate(limit);
        }
        Ok(out)
    }

    async fn open(
        &self,
        selector: SessionSelector,
        who: ClientIdentity,
    ) -> Result<Attachment, KernelError> {
        let mailbox = match selector {
            SessionSelector::Create { spec } => self.create(spec).await?,
            other => self.resolve(other)?.ok_or_else(|| {
                KernelError::new(ErrorCode::Internal, "selector resolved to nothing")
            })?,
        };
        let (snapshot, events) = mailbox.attach().await?;
        Ok(Attachment {
            session: mailbox.id().clone(),
            snapshot,
            events,
            handle: mailbox.port(who),
        })
    }

    async fn close(&self, session: &SessionId, _reason: CloseReason) -> Result<(), KernelError> {
        // Detaching is dropping the attachment; the session keeps running.
        self.live(session).map(|_| ())
    }

    async fn delete(&self, session: &SessionId) -> Result<(), KernelError> {
        let live = self.lock().remove(session).ok_or_else(|| {
            KernelError::new(ErrorCode::SessionNotFound, format!("no session {session}"))
        })?;
        live.mailbox.close(CloseReason::Deleted);
        if let Some(store) = &self.registry.store {
            store.delete(session).await?;
        }
        let _ = self.gateway.send(GatewayEvent::SessionRemoved {
            session: session.clone(),
        });
        Ok(())
    }

    fn catalog(&self, kind: CatalogKind) -> Catalog {
        Catalog {
            kind,
            entries: catalog::entries(&self.registry, self.settings.kernel.model.as_deref(), kind),
        }
    }

    fn gateway_events(&self) -> GatewayStream {
        let rx = self.gateway.subscribe();
        Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(event) => return Some((event, rx)),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        }))
    }

    fn service_any(&self, key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        self.registry.services.get(key).cloned()
    }
}

/// The handle a host hands out after it has been torn down.
struct Unavailable;

fn unavailable() -> KernelError {
    KernelError::new(ErrorCode::SessionClosed, "the host is shut down")
}

#[async_trait]
impl HostApi for Unavailable {
    async fn sessions(&self, _: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        Err(unavailable())
    }
    async fn open(&self, _: SessionSelector, _: ClientIdentity) -> Result<Attachment, KernelError> {
        Err(unavailable())
    }
    async fn close(&self, _: &SessionId, _: CloseReason) -> Result<(), KernelError> {
        Err(unavailable())
    }
    async fn delete(&self, _: &SessionId) -> Result<(), KernelError> {
        Err(unavailable())
    }
    fn catalog(&self, kind: CatalogKind) -> Catalog {
        Catalog {
            kind,
            entries: Vec::new(),
        }
    }
    fn gateway_events(&self) -> GatewayStream {
        Box::pin(futures::stream::empty())
    }
    fn service_any(&self, _: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

#[cfg(test)]
mod tests;

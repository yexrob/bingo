//! The plugin host. It loads plugins in the order the binary hands them
//! over, refuses a plugin whose requirements no earlier plugin provides,
//! collects every contribution into one registry, and serves `HostApi` over
//! a map of session actors. It knows no plugin by name.

use std::any::Any;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use bingo_sdk::*;
use jiff::Timestamp;
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;

use crate::gate::DefaultPolicy;
use crate::session::{self, Mailbox};
use crate::turn::{TurnBudget, TurnConfig};

/// Output tokens a turn may ask for when neither the model nor the config says.
const DEFAULT_MAX_TOKENS: u64 = 8_192;

/// Gateway events buffered per subscriber before the oldest is dropped.
const GATEWAY_CAPACITY: usize = 64;

#[derive(Clone, Debug)]
pub struct HostConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub max_tokens: Option<u32>,
    pub reasoning: Option<Effort>,
    pub system_prompt: String,
    pub budget: TurnBudget,
    pub env: Env,
    /// The merged settings object; plugins receive the keys they claim.
    pub settings: Value,
    /// How deep a chain of sub-sessions may go; 1 = one level below a root.
    pub max_child_depth: u32,
    /// Live children one session may have.
    pub max_children: usize,
}

impl HostConfig {
    pub fn new(env: Env) -> Self {
        Self {
            provider: None,
            model: None,
            max_tokens: None,
            reasoning: None,
            system_prompt: String::new(),
            budget: TurnBudget::default(),
            env,
            settings: Value::Object(Map::new()),
            max_child_depth: 1,
            max_children: 20,
        }
    }

    /// The slice of settings a plugin claimed; an empty object when it claims none.
    fn plugin_settings(&self, manifest: &PluginManifest) -> Value {
        let Some(claim) = manifest.config else {
            return Value::Object(Map::new());
        };
        let mut out = Map::new();
        for (key, _) in claim.keys {
            if let Some(value) = self.settings.get(key) {
                out.insert((*key).to_string(), value.clone());
            }
        }
        Value::Object(out)
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginStatus {
    pub id: String,
    pub version: String,
    pub enabled: bool,
    pub reason: Option<String>,
}

#[derive(Default)]
pub struct Registry {
    pub tools: Vec<Arc<dyn Tool>>,
    pub providers: Vec<Arc<dyn Provider>>,
    pub policy: Option<Arc<dyn PermissionPolicy>>,
    pub hooks: Vec<Arc<dyn Hook>>,
    pub contributors: Vec<Arc<dyn ContextContributor>>,
    pub commands: Vec<Arc<dyn Command>>,
    pub surfaces: Vec<Arc<dyn Surface>>,
    pub store: Option<Arc<dyn SessionStore>>,
    pub compactor: Option<Arc<dyn Compactor>>,
    pub services: HashMap<String, Arc<dyn Any + Send + Sync>>,
    pub plugins: Vec<PluginStatus>,
}

impl Registry {
    fn add(&mut self, plugin: &str, contribution: Contribution) -> Result<(), HostError> {
        let conflict = |what: String| HostError::Conflict {
            plugin: plugin.to_string(),
            what,
        };
        match contribution {
            Contribution::Tool(tool) => {
                let name = tool.spec().name;
                if self.tools.iter().any(|t| t.spec().name == name) {
                    return Err(conflict(format!("tool {name} is already registered")));
                }
                self.tools.push(tool);
            }
            Contribution::Provider(provider) => {
                if self.providers.iter().any(|p| p.id() == provider.id()) {
                    return Err(conflict(format!(
                        "provider {} is already registered",
                        provider.id()
                    )));
                }
                self.providers.push(provider);
            }
            Contribution::Policy(policy) => {
                if let Some(existing) = &self.policy {
                    return Err(conflict(format!(
                        "policy {} is already active",
                        existing.id()
                    )));
                }
                self.policy = Some(policy);
            }
            Contribution::Hook(hook) => self.hooks.push(hook),
            Contribution::Context(contributor) => self.contributors.push(contributor),
            Contribution::Command(command) => {
                let name = command.spec().name;
                if self.commands.iter().any(|c| c.spec().name == name) {
                    return Err(conflict(format!("command {name} is already registered")));
                }
                self.commands.push(command);
            }
            Contribution::Surface(surface) => {
                if self.surfaces.iter().any(|s| s.id() == surface.id()) {
                    return Err(conflict(format!(
                        "surface {} is already registered",
                        surface.id()
                    )));
                }
                self.surfaces.push(surface);
            }
            Contribution::Store(store) => {
                if self.store.is_some() {
                    return Err(conflict("a session store is already registered".into()));
                }
                self.store = Some(store);
            }
            Contribution::Compactor(compactor) => {
                if self.compactor.is_some() {
                    return Err(conflict("a compactor is already registered".into()));
                }
                self.compactor = Some(compactor);
            }
            Contribution::Service { key, value } => {
                if self.services.contains_key(&key) {
                    return Err(conflict(format!("service {key} is already registered")));
                }
                self.services.insert(key, value);
            }
        }
        Ok(())
    }
}

pub struct Host {
    config: HostConfig,
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
        let mut registry = Registry::default();
        let mut provided: HashSet<&'static str> = HashSet::new();
        for plugin in &plugins {
            let manifest = plugin.manifest();
            let missing: Vec<&str> = manifest
                .requires
                .iter()
                .copied()
                .filter(|r| !provided.contains(r))
                .collect();
            if !missing.is_empty() {
                let reason = format!("unmet requirements: {}", missing.join(", "));
                tracing::warn!(plugin = manifest.id, %reason, "plugin disabled");
                registry.plugins.push(PluginStatus {
                    id: manifest.id.to_string(),
                    version: manifest.version.to_string(),
                    enabled: false,
                    reason: Some(reason),
                });
                continue;
            }
            let mut registrar = Registrar::new(manifest.id, config.plugin_settings(manifest));
            plugin
                .register(&mut registrar)
                .map_err(|source| HostError::Register {
                    plugin: manifest.id.to_string(),
                    source,
                })?;
            for contribution in registrar.into_contributions() {
                registry.add(manifest.id, contribution)?;
            }
            provided.extend(manifest.provides.iter().copied());
            registry.plugins.push(PluginStatus {
                id: manifest.id.to_string(),
                version: manifest.version.to_string(),
                enabled: true,
                reason: None,
            });
        }
        let (gateway, _) = broadcast::channel(GATEWAY_CAPACITY);
        let host = Arc::new_cyclic(|weak| Host {
            config,
            registry,
            plugins,
            sessions: Mutex::new(BTreeMap::new()),
            gateway,
            weak: weak.clone(),
        });
        for plugin in &host.plugins {
            let manifest = plugin.manifest();
            if !host.enabled(manifest.id) {
                continue;
            }
            plugin
                .start(host.handle())
                .await
                .map_err(|source| HostError::Start {
                    plugin: manifest.id.to_string(),
                    source,
                })?;
        }
        Ok(host)
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

    pub fn surface(&self, id: &str) -> Option<Arc<dyn Surface>> {
        self.registry
            .surfaces
            .iter()
            .find(|s| s.id() == id)
            .cloned()
    }

    fn enabled(&self, plugin: &str) -> bool {
        self.registry
            .plugins
            .iter()
            .any(|p| p.id == plugin && p.enabled)
    }

    /// Close every session and stop every plugin, in reverse order.
    pub async fn shutdown(&self) {
        let live: Vec<Live> = self.lock().values().cloned().collect();
        for session in live {
            session.mailbox.close(CloseReason::Shutdown);
        }
        for plugin in self.plugins.iter().rev() {
            if !self.enabled(plugin.manifest().id) {
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
            .or_else(|| self.config.provider.clone())
            .or_else(|| self.registry.providers.first().map(|p| p.id().to_string()))
            .ok_or_else(|| {
                KernelError::new(ErrorCode::ProviderUnavailable, "no provider is registered")
            })?;
        self.registry
            .providers
            .iter()
            .find(|p| p.id() == wanted)
            .cloned()
            .ok_or_else(|| {
                KernelError::new(
                    ErrorCode::ProviderUnavailable,
                    format!("no provider {wanted}"),
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
            .or_else(|| self.config.model.clone())
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

    async fn create(&self, spec: SessionSpec) -> Result<Mailbox, KernelError> {
        if let Some(parent) = &spec.parent {
            let parent_id = &parent.session;
            self.live(parent_id)?;
            if self.depth(parent_id) + 1 > self.config.max_child_depth {
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
                .filter(|l| l.parent.as_ref() == Some(parent_id))
                .count();
            if children >= self.config.max_children {
                return Err(KernelError::new(
                    ErrorCode::InvalidInput,
                    format!("sub-session limit {} reached", self.config.max_children),
                ));
            }
        }
        if let Some(key) = &spec.key
            && self.lock().values().any(|l| l.key.as_ref() == Some(key))
        {
            return Err(KernelError::new(
                ErrorCode::SessionLocked,
                format!("session key {key} is in use"),
            ));
        }
        let provider = self.provider(spec.provider.as_deref())?;
        let model = self.model(provider.as_ref(), spec.model.as_deref()).await?;
        let capabilities = provider.capabilities(&model);
        let max_tokens = self
            .config
            .max_tokens
            .unwrap_or_else(|| capabilities.max_output.min(DEFAULT_MAX_TOKENS) as u32);
        let now = Timestamp::now();
        let summary = SessionSummary {
            id: SessionId::mint(),
            key: spec.key.clone(),
            title: spec.title.clone(),
            cwd: spec.cwd.display().to_string(),
            parent: spec.parent.clone(),
            model: Some(model.clone()),
            provider: Some(provider.id().to_string()),
            created_at: now,
            updated_at: now,
            usage: Usage::default(),
            busy: false,
        };
        if let Some(store) = &self.registry.store {
            store.create(&summary).await?;
        }
        let tools: Vec<Arc<dyn Tool>> = self
            .registry
            .tools
            .iter()
            .filter(|t| {
                spec.tools
                    .as_ref()
                    .is_none_or(|names| names.contains(&t.spec().name))
            })
            .cloned()
            .collect();
        let mut system = Vec::new();
        if !self.config.system_prompt.is_empty() {
            system.push(SystemBlock {
                text: self.config.system_prompt.clone(),
                cache: true,
            });
        }
        if let Some(extra) = &spec.system_extra {
            system.push(SystemBlock {
                text: extra.clone(),
                cache: false,
            });
        }
        let policy = self
            .registry
            .policy
            .clone()
            .unwrap_or_else(|| Arc::new(DefaultPolicy));
        let weak = self.weak.clone();
        let live = Live {
            mailbox: session::spawn(summary.clone(), self.registry.store.clone(), |mailbox| {
                Arc::new(TurnConfig {
                    session: summary.clone(),
                    cwd: spec.cwd.clone(),
                    provider,
                    model,
                    capabilities,
                    max_tokens,
                    reasoning: self.config.reasoning,
                    system,
                    tools,
                    policy,
                    hooks: self.registry.hooks.clone(),
                    contributors: self.registry.contributors.clone(),
                    compactor: self.registry.compactor.clone(),
                    budget: self.config.budget,
                    env: Arc::new(self.config.env.clone()),
                    tool_host: Arc::new(SessionToolHost {
                        mailbox: mailbox.clone(),
                        host: weak,
                    }),
                })
            }),
            key: summary.key.clone(),
            cwd: summary.cwd.clone(),
            parent: summary.parent.as_ref().map(|p| p.session.clone()),
            created_at: summary.created_at,
        };
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
        let entries = match kind {
            CatalogKind::Models => self
                .registry
                .providers
                .iter()
                .filter_map(|p| {
                    let model = self.config.model.clone()?;
                    Some(CatalogEntry {
                        id: format!("{}/{model}", p.id()),
                        label: model,
                        meta: json!({ "provider": p.id() }),
                    })
                })
                .collect(),
            CatalogKind::Providers => self
                .registry
                .providers
                .iter()
                .map(|p| CatalogEntry {
                    id: p.id().to_string(),
                    label: p.id().to_string(),
                    meta: json!({ "auth": p.auth() }),
                })
                .collect(),
            CatalogKind::Tools => self
                .registry
                .tools
                .iter()
                .map(|t| {
                    let spec = t.spec();
                    CatalogEntry {
                        id: spec.name.clone(),
                        label: spec.name,
                        meta: json!({ "description": spec.description }),
                    }
                })
                .collect(),
            CatalogKind::Commands => self
                .registry
                .commands
                .iter()
                .map(|c| {
                    let spec = c.spec();
                    CatalogEntry {
                        id: spec.name.clone(),
                        label: spec.hint.clone(),
                        meta: serde_json::to_value(spec).unwrap_or(Value::Null),
                    }
                })
                .collect(),
            CatalogKind::Skills => Vec::new(),
            CatalogKind::Plugins => self
                .registry
                .plugins
                .iter()
                .map(|p| CatalogEntry {
                    id: p.id.clone(),
                    label: format!("{} {}", p.id, p.version),
                    meta: json!({ "enabled": p.enabled, "reason": p.reason }),
                })
                .collect(),
        };
        Catalog { kind, entries }
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

/// What a tool can reach: its own session by mail, the rest through the host.
struct SessionToolHost {
    mailbox: Mailbox,
    host: Weak<Host>,
}

impl SessionToolHost {
    fn host(&self) -> Result<Arc<Host>, KernelError> {
        self.host
            .upgrade()
            .ok_or_else(|| KernelError::new(ErrorCode::SessionClosed, "the host is gone"))
    }
}

#[async_trait]
impl Prompter for SessionToolHost {
    async fn ask(
        &self,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        self.mailbox.ask(None, kind, answers).await
    }
}

#[async_trait]
impl ToolHost for SessionToolHost {
    fn progress(&self, item: &ItemId, tail: String) {
        self.mailbox.progress(item.clone(), tail);
    }

    async fn record(&self, body: ItemBody) -> Result<ItemId, KernelError> {
        self.mailbox.record(body).await
    }

    async fn spawn_session(&self, spec: SessionSpec) -> Result<SessionId, KernelError> {
        let host = self.host()?;
        let mailbox = host.create(spec).await?;
        Ok(mailbox.id().clone())
    }

    fn submit(&self, to: &SessionId, intent: IntentId, input: Input) {
        if let Some(host) = self.host.upgrade()
            && let Ok(live) = host.live(to)
        {
            live.mailbox.submit(intent, input);
        }
    }

    fn service_any(&self, key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        self.host.upgrade()?.registry.services.get(key).cloned()
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

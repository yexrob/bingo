//! The plugin host. It loads plugins in the order the binary hands them
//! over, refuses a plugin whose requirements no earlier plugin provides,
//! collects every contribution into one registry, and serves `HostApi` over
//! a map of session actors. It knows no plugin by name.

mod ask;
mod catalog;
mod refresh;
mod registry;
mod resume;
mod tool_host;
mod tree;
mod unavailable;

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use bingo_sdk::*;
use jiff::Timestamp;
use serde_json::Value;
use tokio::sync::broadcast;

pub(crate) use refresh::Refreshed;
pub use registry::{PluginStatus, Registry};
use tool_host::SessionToolHost;
use unavailable::Unavailable;

use crate::gate::DefaultPolicy;
use crate::models::{self, Learned, ModelCatalog, ServedModels};
use crate::prompt::{self, PromptInput};
use crate::session::{self, Mailbox};
use crate::settings::{self, Claim, Layer, Merged, SettingsError};
use crate::turn::{
    CompactorSet, ContributorSet, HookSet, ModelChoice, ProviderSet, ToolSet, TurnBudget,
    TurnConfig,
};

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
    /// Windows the servers have corrected since this host started (ADR-0004).
    learned: Arc<Learned>,
    /// What each endpoint last said it serves, from earlier processes and
    /// this one's own refresh (ADR-0026 §4).
    served: Arc<ServedModels>,
    weak: Weak<Host>,
}

#[derive(Clone)]
struct Live {
    mailbox: Mailbox,
    key: Option<String>,
    cwd: String,
    parent: Option<SessionId>,
    created_at: Timestamp,
    /// What the session was opened with; `/model` rewrites it (ADR-0008 §4).
    spec: SessionSpec,
    /// The effort the session asks for; `None` is off.
    thinking: Option<Effort>,
}

impl Live {
    fn new(
        mailbox: Mailbox,
        summary: &SessionSummary,
        spec: SessionSpec,
        thinking: Option<Effort>,
    ) -> Self {
        Self {
            mailbox,
            key: summary.key.clone(),
            cwd: summary.cwd.clone(),
            parent: summary.parent.as_ref().map(|p| p.session.clone()),
            created_at: summary.created_at,
            spec,
            thinking,
        }
    }
}

/// What a command may change about a running session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Model {
        /// Stays as it was when absent.
        provider: Option<String>,
        model: String,
    },
    Thinking(Option<Effort>),
}

impl Change {
    fn apply(self, spec: &mut SessionSpec, thinking: &mut Option<Effort>) {
        match self {
            Change::Model { provider, model } => {
                if provider.is_some() {
                    spec.provider = provider;
                }
                spec.model = Some(model);
            }
            Change::Thinking(level) => *thinking = level,
        }
    }
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
        let mut registry = Registry::load(&plugins, &settings.plugins, &config.env)?;
        let (gateway, _) = broadcast::channel(GATEWAY_CAPACITY);
        let learned = Arc::new(Learned::load(
            config.env.data_dir.join("learned-windows.json"),
        ));
        // Loaded here and never fetched here: what an earlier process cached
        // is on hand before the first turn, and asking again is the
        // background's job (ADR-0026 §4).
        let served = Arc::new(ServedModels::load(
            config.env.data_dir.join("served-models.json"),
        ));
        let host = Arc::new_cyclic(|weak| {
            registry.add_builtins(crate::commands::builtins(weak.clone()));
            Host {
                config,
                settings,
                registry,
                plugins,
                sessions: Mutex::new(BTreeMap::new()),
                gateway,
                learned,
                served,
                weak: weak.clone(),
            }
        });
        host.start_plugins().await?;
        refresh::in_background(&host);
        Ok(host)
    }

    /// Ask every usable provider what it serves and wait for the answers
    /// (ADR-0026 §4): what each one said, in registration order.
    pub(crate) async fn refresh_models(self: &Arc<Self>) -> Vec<Refreshed> {
        refresh::now(self).await
    }

    /// When this provider's endpoint last answered, for a person asking how
    /// old the list is; `None` if it never has.
    pub(crate) fn served_at(&self, provider: &str) -> Option<Timestamp> {
        self.served.get(provider).map(|served| served.fetched)
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

    /// What a session actor needs from the host: the command table and a
    /// weak way back.
    fn services(&self) -> session::Services {
        let weak: Weak<dyn HostApi> = self.weak.clone();
        session::Services {
            commands: self.registry.commands.clone(),
            command_sources: self.registry.sources.commands.clone(),
            host: weak,
        }
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

    /// Close every session, wait for their post-turn work (ADR-0008 §7),
    /// then stop every plugin in reverse order.
    pub async fn shutdown(&self) {
        let live: Vec<Live> = self.lock().values().cloned().collect();
        for session in &live {
            session.mailbox.close(CloseReason::Shutdown);
        }
        let closed = futures::future::join_all(live.iter().map(|s| s.mailbox.wait_closed()));
        if tokio::time::timeout(session::AFTER_TURN_DEADLINE, closed)
            .await
            .is_err()
        {
            tracing::warn!("some sessions did not close in time");
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

    /// The session a selector means: live, or reopened from the store
    /// (ADR-0011 §3), so whatever `sessions` lists can be written to.
    async fn mailbox_for(&self, selector: SessionSelector) -> Result<Mailbox, KernelError> {
        match self.resolve(&selector) {
            Some(mailbox) => Ok(mailbox),
            None => self.reopen(selector).await,
        }
    }

    async fn mailbox_of(&self, id: &SessionId) -> Result<Mailbox, KernelError> {
        self.mailbox_for(SessionSelector::ById { id: id.clone() })
            .await
    }

    /// Every live session under `root`, parents before their children.
    fn descendants(&self, root: &SessionId) -> Vec<(SessionId, Mailbox)> {
        let sessions = self.lock();
        let mut out = Vec::new();
        let mut frontier = vec![root.clone()];
        while let Some(parent) = frontier.pop() {
            for (id, live) in sessions.iter() {
                if live.parent.as_ref() == Some(&parent) {
                    out.push((id.clone(), live.mailbox.clone()));
                    frontier.push(id.clone());
                }
            }
        }
        out
    }

    async fn delete_one(&self, session: &SessionId) -> Result<(), KernelError> {
        let live = self.lock().remove(session);
        if let Some(live) = &live {
            live.mailbox.close(CloseReason::Deleted);
        }
        match &self.registry.store {
            Some(store) => store.delete(session).await?,
            None if live.is_none() => {
                return Err(KernelError::new(
                    ErrorCode::SessionNotFound,
                    format!("no session {session}"),
                ));
            }
            None => {}
        }
        let _ = self.gateway.send(GatewayEvent::SessionRemoved {
            session: session.clone(),
        });
        Ok(())
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

    /// The one policy a plugin registered, or the kernel's own when none did.
    fn policy(&self) -> Arc<dyn PermissionPolicy> {
        self.registry
            .policy
            .clone()
            .unwrap_or_else(|| Arc::new(DefaultPolicy))
    }

    /// A session's own way of asking a person (ADR-0012 §5): what a command
    /// hands a provider's `login`.
    pub fn prompter(&self, session: &SessionId) -> Result<Arc<dyn Prompter>, KernelError> {
        let mailbox = self.live(session)?.mailbox;
        Ok(Arc::new(SessionToolHost { mailbox }))
    }

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

    /// The provider `id` names; with none, the settings' provider, else the
    /// first registered.
    pub async fn provider(&self, id: Option<&str>) -> Result<Arc<dyn Provider>, KernelError> {
        let providers = self.providers().await;
        let wanted = id
            .map(str::to_string)
            .or_else(|| self.settings.kernel.provider.clone())
            .or_else(|| providers.first().map(|p| p.id().to_string()))
            .ok_or_else(|| {
                KernelError::new(
                    ErrorCode::ProviderUnavailable,
                    "No model provider is registered in this build.",
                )
            })?;
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

    /// The model a spec runs on: none for a `Log` session (ADR-0011 §1),
    /// which resolves no provider and calls none.
    async fn model_for(
        &self,
        spec: &SessionSpec,
        thinking: Option<Effort>,
    ) -> Result<Option<ModelChoice>, KernelError> {
        match spec.driver {
            Driver::Model => Ok(Some(self.choose_model(spec, thinking).await?)),
            Driver::Log => Ok(None),
        }
    }

    async fn choose_model(
        &self,
        spec: &SessionSpec,
        thinking: Option<Effort>,
    ) -> Result<ModelChoice, KernelError> {
        let provider = self.provider(spec.provider.as_deref()).await?;
        check_auth(provider.as_ref())?;
        let model = self.model(provider.as_ref(), spec.model.as_deref()).await?;
        let capabilities = self.resolve_model(provider.as_ref(), &model);
        Ok(ModelChoice {
            max_tokens: models::max_tokens(&capabilities, self.settings.kernel.max_tokens),
            reasoning: thinking.filter(|_| capabilities.reasoning),
            learned: self.learned.clone(),
            provider,
            id: model,
            capabilities,
        })
    }

    /// The four owners of a model's facts, read once per session (ADR-0004).
    fn resolve_model(&self, provider: &dyn Provider, model: &str) -> ModelCapabilities {
        let key = models::declared::key(provider.id(), model);
        models::resolve(
            self.settings.kernel.models.get(&key),
            self.learned.window(provider.id(), model),
            ModelCatalog::embedded().facts_for(provider.family(), provider.id(), model),
            provider.endpoint(model),
        )
    }

    fn summarize(&self, spec: &SessionSpec, choice: Option<&ModelChoice>) -> SessionSummary {
        let now = Timestamp::now();
        SessionSummary {
            driver: spec.driver,
            id: SessionId::mint(),
            key: spec.key.clone(),
            title: spec.title.clone(),
            cwd: spec.cwd.display().to_string(),
            parent: spec.parent.clone(),
            model: choice.map(|c| c.id.clone()),
            provider: choice.map(|c| c.provider.id().to_string()),
            system_extra: spec.system_extra.clone(),
            tools: spec.tools.clone(),
            created_at: now,
            updated_at: now,
            usage: Usage::default(),
            busy: false,
            // A session the kernel has just minted has been counted, and the
            // count is nought; `None` is only ever a summary older than the
            // field.
            messages: Some(0),
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
            model: &choice.id,
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
        choice: Option<ModelChoice>,
        mailbox: &Mailbox,
    ) -> TurnConfig {
        let system = choice
            .as_ref()
            .map(|choice| self.system_blocks(spec, choice))
            .unwrap_or_default();
        TurnConfig {
            session: summary.clone(),
            cwd: spec.cwd.clone(),
            model: choice,
            system,
            tools: ToolSet {
                fixed: self.tools_for(spec.tools.as_deref()),
                sources: self.registry.sources.tools.clone(),
                only: spec.tools.clone(),
            },
            policy: self.policy(),
            hooks: HookSet {
                fixed: self.registry.hooks.clone(),
                sources: self.registry.sources.hooks.clone(),
            },
            contributors: ContributorSet {
                fixed: self.registry.contributors.clone(),
                sources: self.registry.sources.contexts.clone(),
            },
            compaction: Arc::new(crate::turn::Breaker::default()),
            compactor: CompactorSet {
                fixed: self.registry.compactor.clone(),
                sources: self.registry.sources.compactors.clone(),
            },
            budget: self.config.budget,
            env: Arc::new(self.config.env.clone()),
            host: self.handle(),
            tool_host: Arc::new(SessionToolHost {
                mailbox: mailbox.clone(),
            }),
        }
    }

    async fn create(&self, spec: SessionSpec) -> Result<Mailbox, KernelError> {
        if let Some(parent) = &spec.parent {
            self.check_parent_limits(&parent.session)?;
        }
        self.check_key_free(spec.key.as_deref())?;
        let thinking = self.settings.kernel.thinking;
        let choice = self.model_for(&spec, thinking).await?;
        let summary = self.summarize(&spec, choice.as_ref());
        if let Some(store) = &self.registry.store {
            store.create(&summary).await?;
            store.acquire(&summary.id).await?;
        }
        let mailbox = session::spawn(
            summary.clone(),
            self.registry.store.clone(),
            self.services(),
            |mailbox| Arc::new(self.turn_config(&spec, &summary, choice, mailbox)),
        );
        let live = Live::new(mailbox, &summary, spec, thinking);
        self.lock().insert(summary.id.clone(), live.clone());
        let _ = self.gateway.send(GatewayEvent::SessionCreated {
            summary: Box::new(summary),
        });
        Ok(live.mailbox)
    }

    /// Re-choose the model for a live session and hand the actor the config
    /// its next turn runs on (ADR-0008 §4). Returns the summary as it now is.
    pub async fn reconfigure(
        &self,
        id: &SessionId,
        change: Change,
    ) -> Result<SessionSummary, KernelError> {
        let live = self.live(id)?;
        if live.spec.driver == Driver::Log {
            return Err(KernelError::new(
                ErrorCode::InvalidInput,
                "a log session has no model",
            ));
        }
        let (mut spec, mut thinking) = (live.spec.clone(), live.thinking);
        change.apply(&mut spec, &mut thinking);
        let choice = self.choose_model(&spec, thinking).await?;
        let mut summary = live.mailbox.summary().await?;
        summary.model = Some(choice.id.clone());
        summary.provider = Some(choice.provider.id().to_string());
        let config = Arc::new(self.turn_config(&spec, &summary, Some(choice), &live.mailbox));
        if let Some(entry) = self.lock().get_mut(id) {
            entry.spec = spec;
            entry.thinking = thinking;
        }
        live.mailbox.reconfigure(config);
        Ok(summary)
    }

    /// Open a turn on a live session that only compacts.
    pub async fn compact(
        &self,
        id: &SessionId,
        instructions: Option<String>,
    ) -> Result<(), KernelError> {
        self.live(id)?.mailbox.compact(instructions).await
    }

    pub fn session_thinking(&self, id: &SessionId) -> Result<Option<Effort>, KernelError> {
        self.live(id).map(|l| l.thinking)
    }

    /// What the session's next turn would run on, chosen from the spec as it
    /// now stands: the same resolution [`Host::reconfigure`] hands the actor,
    /// so `ModelChoice::reasoning` here *is* what the turn will ask for.
    /// `None` for a log session, which resolves no provider (ADR-0011 §1).
    ///
    /// A command that reports a setting the model may ignore reads it here
    /// rather than assuming the setting reached the wire.
    pub async fn session_model(&self, id: &SessionId) -> Result<Option<ModelChoice>, KernelError> {
        let live = self.live(id)?;
        self.model_for(&live.spec, live.thinking).await
    }

    pub async fn session_summary(&self, id: &SessionId) -> Result<SessionSummary, KernelError> {
        self.live(id)?.mailbox.summary().await
    }

    /// The session's state as a client would see it on attaching; the
    /// stream that comes with it is not wanted here.
    pub async fn session_state(&self, id: &SessionId) -> Result<SessionState, KernelError> {
        let (state, _stream) = self.live(id)?.mailbox.attach().await?;
        Ok(state)
    }

    /// The live session a selector names, if it is live in this host.
    fn resolve(&self, selector: &SessionSelector) -> Option<Mailbox> {
        let sessions = self.lock();
        match selector {
            SessionSelector::Create { .. } => None,
            SessionSelector::ById { id } => sessions.get(id).map(|l| l.mailbox.clone()),
            SessionSelector::ByKey { key } => sessions
                .values()
                .find(|l| l.key.as_deref() == Some(key.as_str()))
                .map(|l| l.mailbox.clone()),
            // A person's own session, never one of its children: a role or a
            // room under it is newer, and is not what `--continue` means.
            SessionSelector::Latest { cwd } => {
                let cwd = cwd.display().to_string();
                let here: Vec<&Live> = sessions.values().filter(|l| l.cwd == cwd).collect();
                here.iter()
                    .filter(|l| l.parent.is_none())
                    .chain(here.iter())
                    .max_by_key(|l| (l.parent.is_none(), l.created_at))
                    .map(|l| l.mailbox.clone())
            }
        }
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
        if let Some(store) = &self.registry.store {
            let live_ids: Vec<SessionId> = out.iter().map(|s| s.id.clone()).collect();
            out.extend(
                store
                    .list(&filter)
                    .await?
                    .into_iter()
                    .filter(|s| !live_ids.contains(&s.id)),
            );
        }
        out.sort_by_key(|s| std::cmp::Reverse(s.updated_at));
        if let Some(limit) = filter.limit {
            out.truncate(limit);
        }
        Ok(out)
    }

    async fn open(
        &self,
        selector: SessionSelector,
        who: ClientIdentity,
        options: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        let mailbox = match selector {
            SessionSelector::Create { spec } => self.create(spec).await?,
            other => self.mailbox_for(other).await?,
        };
        if options.children {
            return tree::attach(self.weak.clone(), &self.gateway, mailbox, who).await;
        }
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

    /// A session and everything under it, children first (ADR-0010 §6).
    async fn delete(&self, session: &SessionId) -> Result<(), KernelError> {
        let mut doomed: Vec<SessionId> = self
            .descendants(session)
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        doomed.reverse();
        for id in &doomed {
            self.delete_one(id).await?;
        }
        self.delete_one(session).await
    }

    async fn deliver(
        &self,
        to: &SessionId,
        intent: IntentId,
        input: Input,
        delivery: Delivery,
    ) -> Result<(), KernelError> {
        self.mailbox_of(to).await?.deliver(intent, input, delivery);
        Ok(())
    }

    async fn extend(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        self.mailbox_of(session)
            .await?
            .extend(plugin.to_string(), kind.to_string(), payload);
        Ok(())
    }

    async fn signal(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        self.mailbox_of(session)
            .await?
            .signal(plugin.to_string(), kind.to_string(), payload);
        Ok(())
    }

    /// The session must be live and running a turn: one that is only stored
    /// is answering nobody, and reopening it would open a session with no
    /// turn to serve the call anyway (ADR-0036 §2).
    async fn invoke(
        &self,
        session: &SessionId,
        call: ToolCall,
    ) -> Result<ToolOutcome, KernelError> {
        self.live(session)?.mailbox.invoke(call).await
    }

    async fn ask(
        &self,
        session: &SessionId,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        ask::put(self, session, kind, answers).await
    }

    async fn catalog(&self, kind: CatalogKind) -> Result<Catalog, KernelError> {
        let resolved = self.providers().await;
        Ok(Catalog {
            kind,
            entries: catalog::entries(
                &self.registry,
                &resolved,
                self.settings.kernel.model.as_deref(),
                &self.served,
                kind,
            )
            .await,
        })
    }

    /// Every session that is open right now, and no other: a line that belongs
    /// to nobody in particular must not wake a session that had been put away
    /// just to hear it. Nobody open is refused, so the caller keeps the line.
    async fn notice(&self, level: Level, code: &str, text: &str) -> Result<(), KernelError> {
        let open: Vec<Live> = self.lock().values().cloned().collect();
        if open.is_empty() {
            return Err(KernelError::new(
                ErrorCode::SessionNotFound,
                "no session is open to say it to",
            ));
        }
        for session in open {
            let body = ItemBody::Notice {
                level,
                code: code.to_string(),
                text: text.to_string(),
            };
            if let Err(error) = session.mailbox.record(body).await {
                tracing::debug!(%error, code, "a notice reached no transcript");
            }
        }
        Ok(())
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
        self.registry.services.value(key)
    }

    fn service_wire(&self, key: &str) -> Option<Arc<dyn WireService>> {
        self.registry.services.wire(key)
    }

    fn open_service(&self, key: &str, wire: Arc<dyn WireService>) -> Result<(), KernelError> {
        self.registry
            .services
            .open(key, wire)
            .map_err(|why| KernelError::new(ErrorCode::InvalidInput, why))
    }
}

#[cfg(test)]
mod tests;

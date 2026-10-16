//! The plugin host. It loads plugins in the order the binary hands them
//! over, refuses a plugin whose requirements no earlier plugin provides,
//! collects every contribution into one registry, and serves `HostApi` over
//! a map of session actors. It knows no plugin by name.

mod api;
mod approval;
mod ask;
mod auth;
mod catalog;
mod choose;
mod completion;
mod config;
pub use config::HostConfig;
mod instances;
pub(crate) mod interception;
mod live;
pub(crate) mod model_switch;
pub(crate) mod pins;
pub(crate) mod plugin;
pub(crate) mod plugin_lifecycle;
pub(crate) mod provider;
use live::Live;
mod deletion;
mod fork;
mod inspection;
mod reach;
mod refresh;
mod registry;
mod registry_commands;
mod registry_folds;
mod registry_hooks;
mod render;
mod render_queue;
mod resume;
mod throttle;
mod tool_host;
mod tree;
mod unavailable;

use crate::spec::apply;

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use bingo_sdk::*;
use jiff::Timestamp;
use serde_json::Value;
use tokio::sync::broadcast;

pub(crate) use refresh::Refreshed;
pub use registry::{Registry, standing};
use tool_host::SessionToolHost;
pub(crate) use unavailable::Unavailable;

use crate::gate::DefaultPolicy;
use crate::models::{Learned, ServedModels};
use crate::prompt::{self, PromptInput};
use crate::session::{self, Mailbox};
use crate::settings::{self, Claim, Merged, SettingsError};
use crate::turn::{CompactorSet, ContributorSet, HookSet, ModelChoice, ToolSet, TurnConfig};

/// Gateway events buffered per subscriber before the oldest is dropped.
const GATEWAY_CAPACITY: usize = 64;

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
    approvals: Arc<crate::approval::Authority>,
    registry: Registry,
    plugins: Vec<Box<dyn Plugin>>,
    sessions: Mutex<BTreeMap<SessionId, Live>>,
    /// One reopening at a time: two openers of one stored session in this
    /// process would otherwise find it locked by each other.
    reopening: tokio::sync::Mutex<()>,
    gateway: broadcast::Sender<GatewayEvent>,
    /// Windows the servers have corrected since this host started (ADR-0004).
    learned: Arc<Learned>,
    /// What each endpoint last said it serves, from earlier processes and
    /// this one's own refresh (ADR-0026 §4).
    served: Arc<ServedModels>,
    weak: Weak<Host>,
    plugin_host: HostHandle,
    pinned: Mutex<Vec<String>>,
    /// The unreadable trust file the person was last told about, told once.
    trust_unread: Mutex<Option<String>>,
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
        let mut settings = settings::merge(&config.layers, &claims)?;
        let approvals = Arc::new(crate::approval::Authority::new(
            config.env.clone(),
            std::mem::take(&mut settings.kernel.approved),
            std::mem::take(&mut settings.repository_entries),
        ));
        let switched_off = settings.kernel.switched_off();
        let mut registry = Registry::load(
            &plugins,
            &settings.plugins,
            &config.env,
            &switched_off,
            &config.layers,
            &config.reserved_providers,
        )?;
        registry.reserve_providers(&config.reserved_providers, &plugins, &config.layers);
        for link in &mut registry.links {
            if let Some(name) = &link.external {
                link.tier = settings
                    .kernel
                    .plugin_trust
                    .get(name)
                    .map_or(Tier::Tighten, |grant| grant.tier);
            }
        }
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
        let pinned = Mutex::new(std::mem::take(&mut settings.kernel.pinned));
        let host = Arc::new_cyclic(|weak| {
            registry.add_builtins(crate::commands::builtins(weak.clone()));
            Host {
                config,
                settings,
                approvals,
                registry,
                plugins,
                sessions: Mutex::new(BTreeMap::new()),
                reopening: tokio::sync::Mutex::new(()),
                gateway,
                learned,
                served,
                weak: weak.clone(),
                plugin_host: plugin::handle(weak.clone()),
                pinned,
                trust_unread: Mutex::new(None),
            }
        });
        plugin::start(&host).await?;
        interception::register_pending(&host).await;
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

    /// What a session actor needs from the host: commands, drawings, a way back.
    fn services(&self, drawings: &session::Drawings) -> session::Services {
        let weak = Arc::downgrade(&self.plugin_host.0);
        session::Services {
            commands: Vec::new(),
            command_sources: self.registry.command_sources(),
            host: weak,
            drawings: drawings.clone(),
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

    /// What startup found worth telling a person: `(code, text)` pairs — a
    /// setting nobody claimed, and a switch the load could not honour.
    pub fn notices(&self) -> Vec<(String, String)> {
        let unknown = self.settings.unknown.iter().map(|u| {
            (
                "UNKNOWN_SETTING".to_string(),
                format!("unknown setting `{}` in {}", u.key, u.source),
            )
        });
        unknown
            .chain(self.settings.notices.iter().cloned())
            .chain(self.registry.notices.iter().cloned())
            .chain(self.registry.provider_state.instances.notices())
            .chain(self.registry.unavailable_notices())
            .collect()
    }

    /// Why a plugin this build ships may not be switched off, or `None` for
    /// one that may (ADR-0057 §3). A name this build does not ship has no
    /// answer here: a command asks its own listing about that first.
    pub(crate) fn needed(&self, plugin: &str) -> Option<&'static str> {
        self.plugins
            .iter()
            .map(|p| p.manifest())
            .find(|manifest| manifest.id == plugin)
            .and_then(crate::plugins::needed)
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
                if live.spec.parent.as_ref().map(|p| &p.session) == Some(&parent) {
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
            // The actor's closing writes land in the directory being removed.
            live.mailbox.wait_closed().await;
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
        let mut cursor = sessions
            .get(id)
            .and_then(|l| l.spec.parent.as_ref().map(|p| p.session.clone()));
        while let Some(parent) = cursor {
            depth += 1;
            cursor = sessions
                .get(&parent)
                .and_then(|l| l.spec.parent.as_ref().map(|p| p.session.clone()));
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
        let Some(max) = self.config.max_children else {
            return Ok(());
        };
        let children = self
            .lock()
            .values()
            .filter(|l| l.spec.parent.as_ref().map(|p| &p.session) == Some(parent))
            .count();
        if children >= max {
            return Err(KernelError::new(
                ErrorCode::InvalidInput,
                format!("sub-session limit {max} reached"),
            ));
        }
        Ok(())
    }

    /// A routing key names one live session at a time.
    fn check_key_free(&self, key: Option<&str>) -> Result<(), KernelError> {
        self.check_key_free_except(key, None)
    }

    fn check_key_free_except(
        &self,
        key: Option<&str>,
        except: Option<&SessionId>,
    ) -> Result<(), KernelError> {
        let Some(key) = key else { return Ok(()) };
        if self
            .lock()
            .iter()
            .any(|(id, l)| except != Some(id) && l.spec.key.as_deref() == Some(key))
        {
            return Err(KernelError::new(
                ErrorCode::SessionLocked,
                format!("session key {key} is in use"),
            ));
        }
        Ok(())
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
            provider: choice.endpoint.id(),
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

    pub(crate) fn hook_set(&self) -> HookSet {
        interception::hook_set(self)
    }
    pub(crate) fn catalogue_context(&self, session: Option<&SessionId>) -> HookContext {
        interception::catalogue_context(self, session)
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
            keys: self.settings.kernel.keys.clone(),
            spec: Arc::new(spec.clone()),
            session: crate::spec::stated(spec, choice.as_ref(), summary.clone()),
            cwd: spec.cwd.clone(),
            model: choice,
            system,
            tools: ToolSet {
                fixed: self.tools_for(spec.tools.as_deref()),
                sources: self.registry.sources.tools.clone(),
                only: spec.tools.clone(),
            },
            policy: self.policy(),
            hooks: self.hook_set(),
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
            host: self.plugin_host.clone(),
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
        let spec = self.inherited(spec);
        let choice = self.model_for(&spec).await?;
        let summary = crate::spec::stated(&spec, choice.as_ref(), live::summarize());
        if let Some(store) = &self.registry.store {
            store.create(&summary).await?;
            store.acquire(&summary.id).await?;
        }
        let drawings = session::Drawings::default();
        let mailbox = session::spawn(
            summary.clone(),
            self.registry.store.clone(),
            self.services(&drawings),
            |mailbox| Arc::new(self.turn_config(&spec, &summary, choice, mailbox)),
        );
        let live = Live::new(mailbox, &summary, spec, drawings);
        self.lock().insert(summary.id.clone(), live.clone());
        let _ = self.gateway.send(GatewayEvent::SessionCreated {
            summary: Box::new(summary),
        });
        registry_folds::report(self, &live.mailbox).await;
        Ok(live.mailbox)
    }

    /// What a child is opened with where its spec says nothing: its parent's
    /// provider, model and effort as they stand now — `/model` and `/think`
    /// on the parent included — rather than the host's defaults, which are
    /// what the parent was opened on and may have left since. A spec without
    /// a live parent keeps the defaults.
    fn inherited(&self, mut spec: SessionSpec) -> SessionSpec {
        let parent = spec
            .parent
            .as_ref()
            .and_then(|link| self.live(&link.session).ok());
        let Some(parent) = parent else {
            spec.thinking = spec.thinking.or(Some(self.settings.kernel.thinking));
            return spec;
        };
        spec.provider = spec.provider.or_else(|| parent.spec.provider.clone());
        spec.model = spec.model.or_else(|| parent.spec.model.clone());
        spec.thinking = spec.thinking.or(Some(parent.spec.thinking.flatten()));
        spec
    }

    /// Open a turn on a live session that only compacts.
    pub async fn compact(
        &self,
        id: &SessionId,
        instructions: Option<String>,
    ) -> Result<(), KernelError> {
        self.live(id)?.mailbox.compact(instructions).await
    }

    /// The directories this process runs in; what a command that writes a
    /// settings file needs to find one (ADR-0008 §8).
    pub fn env(&self) -> &Env {
        &self.config.env
    }

    /// The level the session asks for, whatever the model then does with it;
    /// `None` is off. Resolved at open, so it is never the absent case.
    pub fn session_thinking(&self, id: &SessionId) -> Result<Option<Effort>, KernelError> {
        self.live(id).map(|l| l.spec.thinking.flatten())
    }

    /// What the session's next turn would run on, chosen from the spec as it
    /// now stands: the same resolution [`HostApi::reconfigure`] hands the actor,
    /// so `ModelChoice::reasoning` here *is* what the turn will ask for.
    /// `None` for a log session, which resolves no provider (ADR-0011 §1).
    ///
    /// A command that reports a setting the model may ignore reads it here
    /// rather than assuming the setting reached the wire.
    pub async fn session_model(&self, id: &SessionId) -> Result<Option<ModelChoice>, KernelError> {
        let live = self.live(id)?;
        self.model_for(&live.spec).await
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
                .find(|l| l.spec.key.as_deref() == Some(key.as_str()))
                .map(|l| l.mailbox.clone()),
            // A person's own session, never one of its children: a role or a
            // room under it is newer, and is not what `--continue` means. A
            // child a plugin woke at start is live before its root is, so
            // with no live root the store answers, roots first.
            SessionSelector::Latest { cwd } => sessions
                .values()
                .filter(|l| &l.spec.cwd == cwd && l.spec.parent.is_none())
                .max_by_key(|l| l.created_at)
                .map(|l| l.mailbox.clone()),
        }
    }
}

#[cfg(test)]
mod tests;

//! Everything the loaded plugins contributed, in one place. A slot with a
//! single holder — the policy, the store, the compactor — refuses a second.

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use std::collections::{BTreeMap, BTreeSet};

use bingo_sdk::service::{Service, Services};
use bingo_sdk::*;
use serde_json::{Map, Value};

use super::HostError;
use crate::plugins::{self, ContributionKind, needed};

/// Everything that arrives after I/O (ADR-0009 §1), one list per kind, each
/// read where that kind is resolved.
///
/// They sit together because a source is the one contribution the composition
/// never arbitrates: it holds no slot, takes no name, and two of a kind are
/// both welcome. What the registry has to judge is above; what it only carries
/// is here.
#[derive(Default)]
pub struct Sources {
    pub interceptors: Vec<Arc<dyn InterceptorSource>>,
    /// Read when a turn starts.
    pub tools: Vec<Arc<dyn ToolSource>>,
    /// Read when a name is not in `commands`.
    pub commands: Vec<Arc<dyn CommandSource>>,
    /// Read where a model is chosen (ADR-0030 §2).
    pub providers: Vec<Arc<dyn ProviderSource>>,
    /// Read when a turn starts.
    pub contexts: Vec<Arc<dyn ContextSource>>,
    /// One is the turn's only where the registered slot is free.
    pub compactors: Vec<Arc<dyn CompactorSource>>,
    /// Read wherever the kernel reads its hooks (ADR-0032 §1).
    pub hooks: Vec<Arc<dyn HookSource>>,
    /// Read where a listing of plugins is drawn (ADR-0057 §5).
    pub plugins: Vec<Arc<dyn PluginSource>>,
}

#[derive(Default)]
pub struct Switches {
    /// Resident owners excluded from future dispatch snapshots.
    pub running: Arc<RwLock<BTreeSet<String>>>,
    pub switching: tokio::sync::Mutex<()>,
    /// The resolved enabledPlugins wish, represented by its disabled names.
    pub wishes: RwLock<BTreeSet<String>>,
    pub kinds: BTreeMap<String, plugin::PluginSwitch>,
    pub unavailable: RwLock<BTreeMap<String, String>>,
    pub(super) lifecycle: Vec<super::plugin_lifecycle::OwnedLifecycle>,
}

#[derive(Default)]
pub struct Registry {
    pub tools: Vec<Arc<dyn Tool>>,
    pub(super) provider_state: super::instances::ProviderState,
    pub agents: Vec<Arc<dyn Agent>>,
    pub policy: Option<Arc<dyn PermissionPolicy>>,
    pub hooks: Vec<Arc<dyn Hook>>,
    pub links: Vec<crate::chain::Link>,
    pub contributors: Vec<Arc<dyn ContextContributor>>,
    pub commands: super::registry_commands::RegisteredCommands,
    pub switches: Switches,
    pub surfaces: Vec<Arc<dyn Surface>>,
    pub store: Option<Arc<dyn SessionStore>>,
    pub compactor: Option<Arc<dyn Compactor>>,
    /// Everything registered before its I/O has happened.
    pub sources: Sources,
    /// One entry per key, holding both faces of one live object: the typed
    /// value a consumer downcasts, and the wire face a process reaches when
    /// the owner opened one (ADR-0031 §1). A service an external process
    /// declares lands here after its handshake, which is why the map is
    /// locked rather than filled once.
    pub services: Services,
    pub plugins: Vec<PluginStatus>,
    /// What loading found worth telling a person, as `(code, text)`; the host
    /// says them beside the settings' own (ADR-0057 §3).
    pub notices: Vec<(String, String)>,
}

fn contribution_kind(contribution: &Contribution) -> ContributionKind {
    match contribution {
        Contribution::Command(_) => ContributionKind::Commands,
        Contribution::CommandFold(_) => ContributionKind::Catalogue,
        Contribution::Lifecycle(_) => ContributionKind::Lifecycle,
        Contribution::Interceptor { link, external, .. } => {
            if external.is_some() {
                return ContributionKind::Process;
            }
            let matcher = link.matcher();
            ContributionKind::Interceptor {
                request_point: catalogue::ALL.iter().find_map(|entry| {
                    (entry.shapes_request
                        && (matcher.patterns.is_empty()
                            || matcher.patterns.iter().any(|p| p.matches(&entry.name))))
                    .then_some(entry.name.as_ref())
                }),
            }
        }
        Contribution::Tool(_) | Contribution::Tools(_) => ContributionKind::Tools,
        Contribution::Provider(_)
        | Contribution::Providers(_)
        | Contribution::ProviderShape(_)
        | Contribution::Agent(_) => ContributionKind::Providers,
        Contribution::Store(_) => ContributionKind::Store,
        Contribution::Surface(_) => ContributionKind::Surface,
        Contribution::Service {
            value, wire: None, ..
        } if value.is::<Arc<bingo_sdk::Pages>>() => ContributionKind::Catalogue,
        Contribution::Service { .. } => ContributionKind::Service,
        Contribution::Context(_) | Contribution::Contexts(_) => ContributionKind::Context,
        Contribution::Policy(_) => ContributionKind::Policy,
        Contribution::Hook(hook) if super::registry_hooks::event_only(&hook.matcher()) => {
            ContributionKind::EventHooks
        }
        Contribution::Hook(_) | Contribution::Hooks(_) => ContributionKind::Hooks,
        Contribution::Compactor(_) | Contribution::Compactors(_) => ContributionKind::Compactor,
        Contribution::Interceptors(_) | Contribution::Commands(_) | Contribution::Plugins(_) => {
            ContributionKind::Source
        }
    }
}

fn declared_service(
    manifest: &PluginManifest,
    contribution: &Contribution,
) -> Result<(), HostError> {
    if let Contribution::Service { key, .. } = contribution {
        let capability = format!("service:{key}");
        if !manifest.provides.contains(&capability.as_str()) {
            return Err(HostError::Conflict {
                plugin: manifest.id.to_string(),
                what: format!("service {key} is not declared in provides"),
            });
        }
    }
    Ok(())
}

impl Registry {
    /// Load plugins in the order given, standing or disabled by
    /// [`standing`]'s verdict. A disabled plugin is a warning and a status,
    /// never fatal. `slices` holds each plugin's claimed settings, by
    /// plugin id; `switched_off` is every name the settings turned off, which
    /// every registrar is handed as it is built (ADR-0057 §4).
    pub(super) fn load(
        plugins: &[Box<dyn Plugin>],
        slices: &BTreeMap<String, Value>,
        env: &Env,
        switched_off: &BTreeSet<String>,
        layers: &[crate::settings::Layer],
        reserved: &BTreeSet<String>,
    ) -> Result<Self, HostError> {
        let reserved = reserved
            .iter()
            .cloned()
            .chain(
                plugins
                    .iter()
                    .flat_map(|plugin| plugin.manifest().provides)
                    .filter_map(|capability| {
                        capability.strip_prefix("provider:").map(str::to_string)
                    }),
            )
            .collect();
        let mut registry = Registry {
            notices: ignored_switches(plugins, switched_off),
            switches: Switches {
                wishes: RwLock::new(plugins::effective_switches(plugins, switched_off.clone())),
                ..Switches::default()
            },
            ..Registry::default()
        };
        for (plugin, reason) in plugins.iter().zip(standing(plugins, switched_off)) {
            let manifest = plugin.manifest();
            if let Some(reason) = reason {
                tracing::warn!(plugin = manifest.id, %reason, "plugin disabled");
                registry
                    .plugins
                    .push(PluginStatus::disabled(manifest, reason));
                if switched_off.contains(manifest.id) && needed(manifest).is_none() {
                    registry.register_off(
                        plugin.as_ref(),
                        slices,
                        env,
                        switched_off,
                        layers,
                        &reserved,
                    );
                }
                continue;
            }
            registry.register(
                plugin.as_ref(),
                slices,
                env,
                switched_off,
                layers,
                &reserved,
            )?;
            registry.plugins.push(PluginStatus::loaded(manifest));
        }
        Ok(registry)
    }

    fn exclude(&mut self, id: &str, why: String) {
        self.commands.entries.retain(|entry| entry.plugin != id);
        self.commands.folds.retain(|entry| entry.plugin != id);
        self.links.retain(|link| link.plugin != id);
        self.switches.lifecycle.retain(|entry| entry.plugin != id);
        self.unavailable(id, why);
    }

    fn register_off(
        &mut self,
        plugin: &dyn Plugin,
        slices: &BTreeMap<String, Value>,
        env: &Env,
        switched_off: &BTreeSet<String>,
        layers: &[crate::settings::Layer],
        reserved: &BTreeSet<String>,
    ) {
        let manifest = plugin.manifest();
        let id = manifest.id;
        // Provider registration may bind credentials even before startup.
        if manifest
            .provides
            .iter()
            .any(|name| name.starts_with("provider:"))
        {
            self.switches.kinds.insert(
                id.into(),
                plugins::switch_kind(&[ContributionKind::Providers]),
            );
            return;
        }
        self.switches
            .running
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.into());
        if let Err(error) = self.register(plugin, slices, env, switched_off, layers, reserved) {
            self.exclude(id, error.to_string());
        }
    }

    /// Take one plugin's contributions, with the settings slice it claimed.
    fn register(
        &mut self,
        plugin: &dyn Plugin,
        slices: &BTreeMap<String, Value>,
        env: &Env,
        switched_off: &BTreeSet<String>,
        layers: &[crate::settings::Layer],
        reserved: &BTreeSet<String>,
    ) -> Result<(), HostError> {
        let manifest = plugin.manifest();
        let slice = slices
            .get(manifest.id)
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let mut registrar = Registrar::new(manifest.id, slice, env.clone(), switched_off.clone())
            .with_sources(crate::settings::sources::for_plugin(
                layers, manifest, reserved,
            ));
        if let Err(source) = plugin.register(&mut registrar) {
            if switched_off.contains(manifest.id) {
                self.unavailable(manifest.id, source.to_string());
                return Ok(());
            }
            return Err(HostError::Register {
                plugin: manifest.id.into(),
                source,
            });
        }
        for notice in registrar.take_notices() {
            if !self.notices.contains(&notice) {
                self.notices.push(notice);
            }
        }
        let contributions = registrar.into_contributions();
        let kinds: Vec<_> = contributions.iter().map(contribution_kind).collect();
        let classified = plugins::switch_kind(&kinds);
        let kind = if manifest.requires.is_empty() || classified != plugin::PluginSwitch::Now {
            classified
        } else {
            plugin::PluginSwitch::NextStart {
                why: "plugin requirements".into(),
            }
        };
        let retain = kind == plugin::PluginSwitch::Now
            || !switched_off.contains(manifest.id)
            || needed(manifest).is_some();
        self.switches.kinds.insert(manifest.id.into(), kind);
        if !retain {
            return Ok(());
        }
        for contribution in contributions {
            declared_service(manifest, &contribution)?;
            self.add(manifest.id, contribution, layers)?;
        }
        Ok(())
    }

    pub(super) fn add(
        &mut self,
        plugin: &str,
        contribution: Contribution,
        layers: &[crate::settings::Layer],
    ) -> Result<(), HostError> {
        let conflict = |what: String| HostError::Conflict {
            plugin: plugin.to_string(),
            what,
        };
        match contribution {
            Contribution::Tool(tool) => self.add_tool(tool),
            Contribution::Tools(source) => {
                self.sources.tools.push(source);
                Ok(())
            }
            Contribution::Agent(agent) => self.add_agent(agent, plugin, layers),
            Contribution::ProviderShape(shape) => {
                if self.provider_state.shapes.contains_key(shape.id()) {
                    Err(format!(
                        "provider shape {} is already registered",
                        shape.id()
                    ))
                } else {
                    self.provider_state
                        .shapes
                        .insert(shape.id().to_string(), shape);
                    Ok(())
                }
            }
            Contribution::Provider(provider) => self.add_provider(provider, plugin, layers),
            Contribution::Providers(source) => {
                self.sources.providers.push(source);
                Ok(())
            }
            Contribution::Policy(policy) => self.set_policy(policy),
            Contribution::Interceptor {
                external,
                link,
                placement,
            } => {
                let tier = if external.is_some() {
                    Tier::Tighten
                } else {
                    Tier::Full
                };
                self.links.push(crate::chain::Link {
                    plugin: plugin.into(),
                    placement,
                    external,
                    interceptor: link,
                    tier,
                });
                Ok(())
            }
            Contribution::Interceptors(source) => {
                self.sources.interceptors.push(source);
                Ok(())
            }
            Contribution::Hook(hook) => {
                self.add_hook(plugin, hook);
                Ok(())
            }
            Contribution::Lifecycle(observer) => {
                self.switches
                    .lifecycle
                    .push(super::plugin_lifecycle::OwnedLifecycle {
                        plugin: plugin.into(),
                        observer,
                    });
                Ok(())
            }
            Contribution::Hooks(source) => {
                self.sources.hooks.push(source);
                Ok(())
            }
            Contribution::Context(contributor) => {
                self.contributors.push(contributor);
                Ok(())
            }
            Contribution::Contexts(source) => {
                self.sources.contexts.push(source);
                Ok(())
            }
            Contribution::Command(command) => self.add_command(plugin, command),
            Contribution::CommandFold(fold) => {
                self.add_command_fold(plugin, fold);
                Ok(())
            }
            Contribution::Commands(source) => {
                self.sources.commands.push(source);
                Ok(())
            }
            Contribution::Surface(surface) => self.add_surface(surface),
            Contribution::Store(store) => self.set_store(store),
            Contribution::Compactor(compactor) => self.set_compactor(compactor),
            Contribution::Compactors(source) => {
                self.sources.compactors.push(source);
                Ok(())
            }
            Contribution::Plugins(source) => {
                self.sources.plugins.push(source);
                Ok(())
            }
            Contribution::Service { key, value, wire } => {
                self.services.add(key, Service { value, wire })
            }
        }
        .map_err(conflict)
    }

    fn add_tool(&mut self, tool: Arc<dyn Tool>) -> Result<(), String> {
        let name = tool.spec().name;
        if self.tools.iter().any(|t| t.spec().name == name) {
            return Err(format!("tool {name} is already registered"));
        }
        self.tools.push(tool);
        Ok(())
    }

    pub(super) fn endpoint_name_taken(&self, name: &str) -> bool {
        self.agents.iter().any(|endpoint| endpoint.id() == name)
    }

    fn add_provider(
        &mut self,
        provider: Arc<dyn Provider>,
        plugin: &str,
        layers: &[crate::settings::Layer],
    ) -> Result<(), String> {
        let collision = self
            .provider_state
            .registered
            .iter()
            .find(|p| p.id() == provider.id())
            .cloned();
        if collision.is_some() || self.endpoint_name_taken(provider.id()) {
            if provider.instance() {
                let winner = collision
                    .as_ref()
                    .map_or(plugin, |p| p.plugin().unwrap_or_else(|| p.family()));
                self.skip_instance(provider.as_ref(), winner, layers);
                return Ok(());
            }
            if let Some(held) = collision.filter(|p| p.instance()) {
                self.skip_instance(held.as_ref(), plugin, layers);
                self.provider_state
                    .registered
                    .retain(|p| p.id() != provider.id());
            } else {
                return Err(format!("provider {} is already registered", provider.id()));
            }
        }
        self.provider_state.registered.push(provider);
        Ok(())
    }

    fn skip_instance(
        &mut self,
        instance: &dyn Provider,
        winner: &str,
        layers: &[crate::settings::Layer],
    ) {
        self.notices.push((
            "provider.instance".into(),
            super::instances::collision(instance, winner, layers, "the user settings file"),
        ));
    }

    pub(super) fn reserve_providers(
        &mut self,
        names: &BTreeSet<String>,
        plugins: &[Box<dyn Plugin>],
        layers: &[crate::settings::Layer],
    ) {
        let skipped: Vec<_> = self
            .provider_state
            .registered
            .iter()
            .filter(|p| p.instance() && names.contains(p.id()))
            .cloned()
            .collect();
        self.provider_state
            .registered
            .retain(|p| !p.instance() || !names.contains(p.id()));
        for instance in skipped {
            let capability = format!("provider:{}", instance.id());
            let owner = plugins
                .iter()
                .map(|plugin| plugin.manifest())
                .find(|manifest| manifest.provides.contains(&capability.as_str()));
            if let Some(owner) = owner {
                self.skip_instance(instance.as_ref(), owner.id, layers);
            } else {
                self.notices.push((
                    "provider.instance".into(),
                    super::instances::collision_reason(
                        instance.as_ref(),
                        "this binary reserves that provider name",
                        layers,
                        "the user settings file",
                    ),
                ));
            }
        }
    }

    fn add_agent(
        &mut self,
        agent: Arc<dyn Agent>,
        plugin: &str,
        layers: &[crate::settings::Layer],
    ) -> Result<(), String> {
        if let Some(held) = self
            .provider_state
            .registered
            .iter()
            .find(|p| p.id() == agent.id() && p.instance())
            .cloned()
        {
            self.skip_instance(held.as_ref(), plugin, layers);
            self.provider_state
                .registered
                .retain(|p| p.id() != agent.id());
        }
        if self
            .provider_state
            .registered
            .iter()
            .any(|p| p.id() == agent.id())
            || self.agents.iter().any(|a| a.id() == agent.id())
        {
            return Err(format!("endpoint {} is already registered", agent.id()));
        }
        self.agents.push(agent);
        Ok(())
    }

    fn set_policy(&mut self, policy: Arc<dyn PermissionPolicy>) -> Result<(), String> {
        if let Some(existing) = &self.policy {
            return Err(format!("policy {} is already active", existing.id()));
        }
        self.policy = Some(policy);
        Ok(())
    }

    /// The kernel's own commands, added last: a plugin that took a name
    /// first keeps it.
    pub(super) fn add_builtins(&mut self, commands: Vec<Arc<dyn Command>>) {
        self.commands
            .entries
            .extend(
                commands
                    .into_iter()
                    .map(|command| super::registry_commands::OwnedCommand {
                        plugin: String::new(),
                        command,
                    }),
            );
        self.validate_command_folds();
    }

    fn add_command(&mut self, plugin: &str, command: Arc<dyn Command>) -> Result<(), String> {
        let name = command.spec().name;
        if let Some(owner) = self
            .commands
            .entries
            .iter()
            .find(|c| c.spec().name == name)
            .map(|entry| entry.plugin.clone())
        {
            let off = self
                .switches
                .running
                .read()
                .unwrap_or_else(|e| e.into_inner());
            if off.contains(plugin) || !off.contains(&owner) {
                return Err(format!("command {name} is already registered"));
            }
            drop(off);
            // An off resident must never prevent an active owner from starting.
            self.exclude(&owner, format!("command {name} conflicts with {plugin}"));
        }
        self.commands
            .entries
            .push(super::registry_commands::OwnedCommand {
                plugin: plugin.into(),
                command,
            });
        Ok(())
    }

    fn add_surface(&mut self, surface: Arc<dyn Surface>) -> Result<(), String> {
        if self.surfaces.iter().any(|s| s.id() == surface.id()) {
            return Err(format!("surface {} is already registered", surface.id()));
        }
        self.surfaces.push(surface);
        Ok(())
    }

    fn set_store(&mut self, store: Arc<dyn SessionStore>) -> Result<(), String> {
        if self.store.is_some() {
            return Err("a session store is already registered".into());
        }
        self.store = Some(store);
        Ok(())
    }

    fn set_compactor(&mut self, compactor: Arc<dyn Compactor>) -> Result<(), String> {
        if self.compactor.is_some() {
            return Err("a compactor is already registered".into());
        }
        self.compactor = Some(compactor);
        Ok(())
    }

    pub(super) fn command_sources(&self) -> Vec<Arc<dyn CommandSource>> {
        let mut sources: Vec<Arc<dyn CommandSource>> =
            vec![Arc::new(super::registry_commands::Commands {
                table: self.commands.entries.clone(),
                running: self.switches.running.clone(),
            })];
        sources.extend(self.sources.commands.clone());
        sources
    }

    pub(crate) fn switch_kind(&self, plugin: &str) -> Option<plugin::PluginSwitch> {
        if let Some(why) = self
            .switches
            .unavailable
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(plugin)
        {
            return Some(plugin::PluginSwitch::NextStart { why: why.clone() });
        }
        self.switches.kinds.get(plugin).cloned()
    }

    pub(super) fn unavailable(&self, plugin: &str, why: String) {
        self.switches
            .unavailable
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(plugin.into(), why);
    }

    pub(super) fn unavailable_notices(&self) -> Vec<(String, String)> {
        self.switches
            .unavailable
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(name, why)| {
                (
                    "PLUGIN_DISABLED".into(),
                    format!("{name} stays off until next start: {why}"),
                )
            })
            .collect()
    }

    pub(super) fn enabled(&self, plugin: &str) -> bool {
        if self.switch_kind(plugin) == Some(plugin::PluginSwitch::Now) {
            return true;
        }
        self.plugins
            .iter()
            .any(|p| p.id == plugin && p.state.is_on())
    }
}

/// Why each plugin cannot stand, or `None` for one that can.
///
/// A switch goes first: a plugin the settings turned off is down before the
/// fixpoint runs, so whoever required what it provided cascades in the usual
/// way, naming the requirement rather than the switch (ADR-0057 §2). Then the
/// requirements nobody provides, checked against what the whole composition
/// provides — never against the accident of the caller's order — to a
/// fixpoint, so a plugin whose provider was itself disabled goes down with
/// it, the reason naming what went missing.
///
/// It is public because the headless `bingo plugins list` says what the next
/// start will do, which is this verdict on the same composition.
pub fn standing(
    plugins: &[Box<dyn Plugin>],
    switched_off: &BTreeSet<String>,
) -> Vec<Option<String>> {
    let mut reasons: Vec<Option<String>> = plugins
        .iter()
        .map(|plugin| switched(plugin.manifest(), switched_off))
        .collect();
    loop {
        let provided: HashSet<&'static str> = plugins
            .iter()
            .zip(&reasons)
            .filter(|(_, reason)| reason.is_none())
            .flat_map(|(plugin, _)| plugin.manifest().provides.iter().copied())
            .collect();
        let mut changed = false;
        for (i, plugin) in plugins.iter().enumerate() {
            if reasons[i].is_none()
                && let Some(reason) = unmet(plugin.manifest(), &provided)
            {
                reasons[i] = Some(reason);
                changed = true;
            }
        }
        if !changed {
            return reasons;
        }
    }
}

/// The reason a switch takes this plugin down, or `None`: nobody turned it
/// off, or it is one the binary cannot run without, which keeps it standing
/// (ADR-0057 §3).
fn switched(manifest: &PluginManifest, switched_off: &BTreeSet<String>) -> Option<String> {
    let off = switched_off.contains(manifest.id) && needed(manifest).is_none();
    off.then(|| SWITCHED_OFF.to_string())
}

/// What a plugin that ignored its switch has to say for itself, for the host
/// to pass on: a person who turned one off and still sees it is owed the
/// reason (ADR-0057 §3).
fn ignored_switches(
    plugins: &[Box<dyn Plugin>],
    switched_off: &BTreeSet<String>,
) -> Vec<(String, String)> {
    plugins
        .iter()
        .map(|plugin| plugin.manifest())
        .filter(|manifest| switched_off.contains(manifest.id))
        .filter_map(|manifest| {
            let why = needed(manifest)?;
            let text = format!("`{}` stays on: {why}", manifest.id);
            Some((plugins::NEEDED.to_string(), text))
        })
        .collect()
}

fn unmet(manifest: &PluginManifest, provided: &HashSet<&'static str>) -> Option<String> {
    let missing: Vec<&str> = manifest
        .requires
        .iter()
        .copied()
        .filter(|r| !provided.contains(r))
        .collect();
    (!missing.is_empty()).then(|| format!("unmet requirements: {}", missing.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{PluginError, Registrar};

    /// A plugin that is nothing but its manifest.
    struct Paper(&'static PluginManifest);

    #[async_trait::async_trait]
    impl Plugin for Paper {
        fn manifest(&self) -> &'static PluginManifest {
            self.0
        }
        fn register(&self, _: &mut Registrar) -> Result<(), PluginError> {
            Ok(())
        }
    }

    #[test]
    fn a_missing_used_service_leaves_the_plugin_standing() {
        static USES: PluginManifest = PluginManifest {
            uses: &["test.optional"],
            ..GIVES
        };
        let plugins: Vec<Box<dyn Plugin>> = vec![Box::new(Paper(&USES))];
        assert_eq!(standing(&plugins, &BTreeSet::new()), [None]);
    }

    static NEEDS: PluginManifest = PluginManifest {
        id: "test.needs",
        version: "0.0.0",
        uses: &[],
        provides: &[],
        requires: &["service:x"],
        config: None,
    };
    static GIVES: PluginManifest = PluginManifest {
        id: "test.gives",
        version: "0.0.0",
        uses: &[],
        provides: &["service:x"],
        requires: &[],
        config: None,
    };
    static CHAIN: PluginManifest = PluginManifest {
        id: "test.chain",
        version: "0.0.0",
        uses: &[],
        provides: &["service:y"],
        requires: &["service:missing"],
        config: None,
    };
    static LEANS: PluginManifest = PluginManifest {
        id: "test.leans",
        version: "0.0.0",
        uses: &[],
        provides: &[],
        requires: &["service:y"],
        config: None,
    };
    static STORE: PluginManifest = PluginManifest {
        id: "test.store",
        version: "0.0.0",
        uses: &[],
        provides: &["store:memory"],
        requires: &[],
        config: None,
    };

    /// A plugin that contributes something, so a test can see whether it was
    /// registered at all.
    struct Contributor(&'static PluginManifest);

    #[async_trait::async_trait]
    impl Plugin for Contributor {
        fn manifest(&self) -> &'static PluginManifest {
            self.0
        }
        fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
            registrar.add(Contribution::Tools(Arc::new(Nothing)));
            Ok(())
        }
    }

    fn loaded(plugins: Vec<Box<dyn Plugin>>) -> Registry {
        switched(plugins, &[])
    }

    /// The same load, with the names a person turned off.
    fn switched(plugins: Vec<Box<dyn Plugin>>, off: &[&str]) -> Registry {
        let off: BTreeSet<String> = off.iter().map(|n| (*n).to_string()).collect();
        Registry::load(
            &plugins,
            &BTreeMap::new(),
            &Env::rooted("/nowhere"),
            &off,
            &[],
            &BTreeSet::new(),
        )
        .expect("nothing here fails to register")
    }

    fn status<'a>(registry: &'a Registry, id: &str) -> &'a PluginStatus {
        registry
            .plugins
            .iter()
            .find(|status| status.id == id)
            .unwrap_or_else(|| panic!("{id} is not in {:?}", registry.plugins))
    }

    /// A source of every late kind, answering with nothing — which is never
    /// wrong (ADR-0009 §1) and is all this table asks of it.
    struct Nothing;

    #[async_trait::async_trait]
    impl ToolSource for Nothing {
        fn id(&self) -> &str {
            "nothing"
        }
        async fn tools(&self) -> Vec<Arc<dyn Tool>> {
            Vec::new()
        }
    }

    #[async_trait::async_trait]
    impl CommandSource for Nothing {
        fn id(&self) -> &str {
            "nothing"
        }
        async fn commands(&self, _: &std::path::Path) -> Vec<Arc<dyn Command>> {
            Vec::new()
        }
    }

    #[async_trait::async_trait]
    impl ContextSource for Nothing {
        fn id(&self) -> &str {
            "nothing"
        }
        async fn contributors(&self) -> Vec<Arc<dyn ContextContributor>> {
            Vec::new()
        }
    }

    #[async_trait::async_trait]
    impl CompactorSource for Nothing {
        fn id(&self) -> &str {
            "nothing"
        }
        async fn compactors(&self) -> Vec<Arc<dyn Compactor>> {
            Vec::new()
        }
    }

    #[async_trait::async_trait]
    impl ProviderSource for Nothing {
        fn id(&self) -> &str {
            "nothing"
        }
        async fn providers(&self) -> Vec<Arc<dyn Provider>> {
            Vec::new()
        }
    }

    #[async_trait::async_trait]
    impl HookSource for Nothing {
        fn id(&self) -> &str {
            "nothing"
        }
        async fn hooks(&self) -> Vec<Arc<dyn Hook>> {
            Vec::new()
        }
    }

    #[async_trait::async_trait]
    impl PluginSource for Nothing {
        fn id(&self) -> &str {
            "nothing"
        }
        async fn plugins(&self) -> Vec<PluginStatus> {
            Vec::new()
        }
    }

    /// Every kind that arrives after I/O lands in the list named for it, and a
    /// second one is welcome: a source holds no slot.
    /// One row of the table: a source to register, and where it must land.
    type Source = fn() -> Contribution;
    type Kept = fn(&Registry) -> usize;

    #[test]
    fn a_late_source_of_every_kind_is_kept_where_the_turn_reads_it() {
        let table: Vec<(Source, Kept)> = vec![
            (
                || Contribution::Tools(Arc::new(Nothing)),
                |registry| registry.sources.tools.len(),
            ),
            (
                || Contribution::Commands(Arc::new(Nothing)),
                |registry| registry.sources.commands.len(),
            ),
            (
                || Contribution::Contexts(Arc::new(Nothing)),
                |registry| registry.sources.contexts.len(),
            ),
            (
                || Contribution::Compactors(Arc::new(Nothing)),
                |registry| registry.sources.compactors.len(),
            ),
            (
                || Contribution::Providers(Arc::new(Nothing)),
                |registry| registry.sources.providers.len(),
            ),
            (
                || Contribution::Hooks(Arc::new(Nothing)),
                |registry| registry.sources.hooks.len(),
            ),
            (
                || Contribution::Plugins(Arc::new(Nothing)),
                |registry| registry.sources.plugins.len(),
            ),
        ];
        for (contribute, count) in table {
            let mut registry = Registry::default();
            for _ in 0..2 {
                registry
                    .add("test.late", contribute(), &[])
                    .expect("a source never conflicts");
            }
            assert_eq!(count(&registry), 2, "{:?}", contribute());
        }
    }

    /// The bug this module had: dependency correctness hung on the bin's
    /// hand-written plugin order, and a provider listed later silently
    /// disabled its consumer.
    #[test]
    fn a_requirement_met_later_in_the_order_still_stands() {
        let registry = loaded(vec![Box::new(Paper(&NEEDS)), Box::new(Paper(&GIVES))]);
        assert!(
            registry.plugins.iter().all(|status| status.state.is_on()),
            "{:?}",
            registry.plugins
        );
    }

    #[test]
    fn a_loss_cascades_to_whoever_required_the_lost_capability() {
        let registry = loaded(vec![Box::new(Paper(&LEANS)), Box::new(Paper(&CHAIN))]);
        let chain = &registry.plugins[1];
        assert!(!chain.state.is_on(), "{:?}", registry.plugins);
        assert!(
            chain
                .state
                .reason()
                .unwrap_or_default()
                .contains("service:missing"),
            "{:?}",
            chain.state.reason()
        );
        let leans = &registry.plugins[0];
        assert!(
            !leans.state.is_on(),
            "the loss cascades: {:?}",
            registry.plugins
        );
        assert!(
            leans
                .state
                .reason()
                .unwrap_or_default()
                .contains("service:y"),
            "{:?}",
            leans.state.reason()
        );
    }

    /// A switch is a verdict like an unmet requirement: the plugin is listed,
    /// disabled, with the reason — and nothing it would have contributed is
    /// in the registry, because `register` was never called (ADR-0057 §2).
    #[test]
    fn a_plugin_switched_off_is_disabled_with_its_reason_and_registers_nothing() {
        let registry = switched(vec![Box::new(Contributor(&GIVES))], &["test.gives"]);
        let gives = status(&registry, "test.gives");
        assert!(!gives.state.is_on(), "{:?}", registry.plugins);
        assert_eq!(gives.state.reason(), Some(SWITCHED_OFF));
        assert_eq!(gives.from, BUILT_IN);
        assert!(
            registry.sources.tools.is_empty(),
            "a plugin that never registered contributed nothing"
        );
        assert!(!registry.enabled("test.gives"));
        assert!(registry.notices.is_empty(), "{:?}", registry.notices);
    }

    /// Whoever required what the switched-off plugin provided goes down with
    /// it, naming the requirement rather than the switch: the fixpoint does
    /// not care why a capability is missing.
    #[test]
    fn a_switch_cascades_to_whoever_required_what_it_provided() {
        let registry = switched(
            vec![Box::new(Paper(&NEEDS)), Box::new(Paper(&GIVES))],
            &["test.gives"],
        );
        let needs = status(&registry, "test.needs");
        assert!(!needs.state.is_on(), "{:?}", registry.plugins);
        assert!(
            needs
                .state
                .reason()
                .unwrap_or_default()
                .contains("service:x"),
            "{:?}",
            needs.state.reason()
        );
    }

    /// What the binary cannot run without ignores its switch, stays
    /// registered, and the person who turned it off is told why it is still
    /// there (ADR-0057 §3).
    #[test]
    fn a_store_ignores_its_switch_and_the_notice_names_it() {
        let registry = switched(vec![Box::new(Contributor(&STORE))], &["test.store"]);
        assert!(
            status(&registry, "test.store").state.is_on(),
            "{:?}",
            registry.plugins
        );
        assert_eq!(registry.sources.tools.len(), 1, "it registered as usual");
        let (code, text) = registry.notices.first().expect("one notice");
        assert_eq!(code, crate::plugins::NEEDED);
        assert!(text.contains("test.store"), "{text}");
    }
}

#[cfg(test)]
mod page_switch_tests {
    use super::*;
    #[test]
    fn static_guides_are_catalogue_content_but_runtime_services_are_not() {
        let pages = Contribution::service(
            &bingo_sdk::Pages::key("example"),
            Arc::new(bingo_sdk::Pages(&[])),
            None,
        );
        assert_eq!(contribution_kind(&pages), ContributionKind::Catalogue);
        let runtime = Contribution::service(
            &bingo_sdk::ServiceKey::new("example"),
            Arc::new(42u64),
            None,
        );
        assert_eq!(contribution_kind(&runtime), ContributionKind::Service);
    }
}

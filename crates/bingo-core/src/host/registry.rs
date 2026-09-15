//! Everything the loaded plugins contributed, in one place. A slot with a
//! single holder — the policy, the store, the compactor — refuses a second.

use std::collections::HashSet;
use std::sync::Arc;

use std::collections::{BTreeMap, BTreeSet};

use bingo_sdk::service::{Service, Services};
use bingo_sdk::*;
use serde_json::{Map, Value};

use super::HostError;
use crate::plugins::{self, needed};

/// Everything that arrives after I/O (ADR-0009 §1), one list per kind, each
/// read where that kind is resolved.
///
/// They sit together because a source is the one contribution the composition
/// never arbitrates: it holds no slot, takes no name, and two of a kind are
/// both welcome. What the registry has to judge is above; what it only carries
/// is here.
#[derive(Default)]
pub struct Sources {
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
    ) -> Result<Self, HostError> {
        let mut registry = Registry {
            notices: ignored_switches(plugins, switched_off),
            ..Registry::default()
        };
        for (plugin, reason) in plugins.iter().zip(standing(plugins, switched_off)) {
            let manifest = plugin.manifest();
            if let Some(reason) = reason {
                tracing::warn!(plugin = manifest.id, %reason, "plugin disabled");
                registry
                    .plugins
                    .push(PluginStatus::disabled(manifest, reason));
                continue;
            }
            registry.register(plugin.as_ref(), slices, env, switched_off)?;
            registry.plugins.push(PluginStatus::loaded(manifest));
        }
        Ok(registry)
    }

    /// Take one plugin's contributions, with the settings slice it claimed.
    fn register(
        &mut self,
        plugin: &dyn Plugin,
        slices: &BTreeMap<String, Value>,
        env: &Env,
        switched_off: &BTreeSet<String>,
    ) -> Result<(), HostError> {
        let manifest = plugin.manifest();
        let slice = slices
            .get(manifest.id)
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let mut registrar = Registrar::new(manifest.id, slice, env.clone(), switched_off.clone());
        plugin
            .register(&mut registrar)
            .map_err(|source| HostError::Register {
                plugin: manifest.id.to_string(),
                source,
            })?;
        for contribution in registrar.into_contributions() {
            self.add(manifest.id, contribution)?;
        }
        Ok(())
    }

    pub(super) fn add(
        &mut self,
        plugin: &str,
        contribution: Contribution,
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
            Contribution::Provider(provider) => self.add_provider(provider),
            Contribution::Providers(source) => {
                self.sources.providers.push(source);
                Ok(())
            }
            Contribution::Policy(policy) => self.set_policy(policy),
            Contribution::Hook(hook) => {
                self.hooks.push(hook);
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
            Contribution::Command(command) => self.add_command(command),
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

    fn add_provider(&mut self, provider: Arc<dyn Provider>) -> Result<(), String> {
        if self.providers.iter().any(|p| p.id() == provider.id()) {
            return Err(format!("provider {} is already registered", provider.id()));
        }
        self.providers.push(provider);
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
        for command in commands {
            if let Err(taken) = self.add_command(command) {
                tracing::debug!(%taken, "a plugin's command shadows the kernel's");
            }
        }
    }

    fn add_command(&mut self, command: Arc<dyn Command>) -> Result<(), String> {
        let name = command.spec().name;
        if self.commands.iter().any(|c| c.spec().name == name) {
            return Err(format!("command {name} is already registered"));
        }
        self.commands.push(command);
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

    pub(super) fn enabled(&self, plugin: &str) -> bool {
        self.plugins.iter().any(|p| p.id == plugin && p.enabled)
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

    static NEEDS: PluginManifest = PluginManifest {
        id: "test.needs",
        version: "0.0.0",
        sdk: "^0.1",
        provides: &[],
        requires: &["service:x"],
        config: None,
    };
    static GIVES: PluginManifest = PluginManifest {
        id: "test.gives",
        version: "0.0.0",
        sdk: "^0.1",
        provides: &["service:x"],
        requires: &[],
        config: None,
    };
    static CHAIN: PluginManifest = PluginManifest {
        id: "test.chain",
        version: "0.0.0",
        sdk: "^0.1",
        provides: &["service:y"],
        requires: &["service:missing"],
        config: None,
    };
    static LEANS: PluginManifest = PluginManifest {
        id: "test.leans",
        version: "0.0.0",
        sdk: "^0.1",
        provides: &[],
        requires: &["service:y"],
        config: None,
    };
    static STORE: PluginManifest = PluginManifest {
        id: "test.store",
        version: "0.0.0",
        sdk: "^0.1",
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
        Registry::load(&plugins, &BTreeMap::new(), &Env::rooted("/nowhere"), &off)
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
                    .add("test.late", contribute())
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
            registry.plugins.iter().all(|status| status.enabled),
            "{:?}",
            registry.plugins
        );
    }

    #[test]
    fn a_loss_cascades_to_whoever_required_the_lost_capability() {
        let registry = loaded(vec![Box::new(Paper(&LEANS)), Box::new(Paper(&CHAIN))]);
        let chain = &registry.plugins[1];
        assert!(!chain.enabled, "{:?}", registry.plugins);
        assert!(
            chain
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("service:missing"),
            "{:?}",
            chain.reason
        );
        let leans = &registry.plugins[0];
        assert!(!leans.enabled, "the loss cascades: {:?}", registry.plugins);
        assert!(
            leans
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("service:y"),
            "{:?}",
            leans.reason
        );
    }

    /// A switch is a verdict like an unmet requirement: the plugin is listed,
    /// disabled, with the reason — and nothing it would have contributed is
    /// in the registry, because `register` was never called (ADR-0057 §2).
    #[test]
    fn a_plugin_switched_off_is_disabled_with_its_reason_and_registers_nothing() {
        let registry = switched(vec![Box::new(Contributor(&GIVES))], &["test.gives"]);
        let gives = status(&registry, "test.gives");
        assert!(!gives.enabled, "{:?}", registry.plugins);
        assert_eq!(gives.reason.as_deref(), Some(SWITCHED_OFF));
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
        assert!(!needs.enabled, "{:?}", registry.plugins);
        assert!(
            needs
                .reason
                .as_deref()
                .unwrap_or_default()
                .contains("service:x"),
            "{:?}",
            needs.reason
        );
    }

    /// What the binary cannot run without ignores its switch, stays
    /// registered, and the person who turned it off is told why it is still
    /// there (ADR-0057 §3).
    #[test]
    fn a_store_ignores_its_switch_and_the_notice_names_it() {
        let registry = switched(vec![Box::new(Contributor(&STORE))], &["test.store"]);
        assert!(
            status(&registry, "test.store").enabled,
            "{:?}",
            registry.plugins
        );
        assert_eq!(registry.sources.tools.len(), 1, "it registered as usual");
        let (code, text) = registry.notices.first().expect("one notice");
        assert_eq!(code, crate::plugins::NEEDED);
        assert!(text.contains("test.store"), "{text}");
    }
}

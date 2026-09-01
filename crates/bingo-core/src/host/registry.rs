//! Everything the loaded plugins contributed, in one place. A slot with a
//! single holder — the policy, the store, the compactor — refuses a second.

use std::collections::HashSet;
use std::sync::Arc;

use std::collections::BTreeMap;

use bingo_sdk::service::{Service, Services};
use bingo_sdk::*;
use serde_json::{Map, Value};

use super::HostError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginStatus {
    pub id: String,
    pub version: String,
    pub enabled: bool,
    pub reason: Option<String>,
}

impl PluginStatus {
    pub(super) fn loaded(manifest: &PluginManifest) -> Self {
        Self {
            id: manifest.id.to_string(),
            version: manifest.version.to_string(),
            enabled: true,
            reason: None,
        }
    }

    pub(super) fn disabled(manifest: &PluginManifest, reason: String) -> Self {
        Self {
            id: manifest.id.to_string(),
            version: manifest.version.to_string(),
            enabled: false,
            reason: Some(reason),
        }
    }
}

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
}

impl Registry {
    /// Load plugins in the order given, standing or disabled by
    /// [`standing`]'s verdict. A disabled plugin is a warning and a status,
    /// never fatal. `slices` holds each plugin's claimed settings, by
    /// plugin id.
    pub(super) fn load(
        plugins: &[Box<dyn Plugin>],
        slices: &BTreeMap<String, Value>,
        env: &Env,
    ) -> Result<Self, HostError> {
        let mut registry = Registry::default();
        for (plugin, reason) in plugins.iter().zip(standing(plugins)) {
            let manifest = plugin.manifest();
            if let Some(reason) = reason {
                tracing::warn!(plugin = manifest.id, %reason, "plugin disabled");
                registry
                    .plugins
                    .push(PluginStatus::disabled(manifest, reason));
                continue;
            }
            registry.register(plugin.as_ref(), slices, env)?;
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
    ) -> Result<(), HostError> {
        let manifest = plugin.manifest();
        let slice = slices
            .get(manifest.id)
            .cloned()
            .unwrap_or_else(|| Value::Object(Map::new()));
        let mut registrar = Registrar::new(manifest.id, slice, env.clone());
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

/// The requirements nobody has provided yet, as a reason to disable.
/// Why each plugin cannot stand, or `None` for one that can. A requirement is
/// checked against what the whole composition provides — never against the
/// accident of the caller's order — and the check runs to a fixpoint, so a
/// plugin whose provider was itself disabled goes down with it, the reason
/// naming what went missing.
fn standing(plugins: &[Box<dyn Plugin>]) -> Vec<Option<String>> {
    let mut reasons: Vec<Option<String>> = vec![None; plugins.len()];
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

    fn loaded(plugins: Vec<Box<dyn Plugin>>) -> Registry {
        Registry::load(&plugins, &BTreeMap::new(), &Env::rooted("/nowhere"))
            .expect("nothing here fails to register")
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
}

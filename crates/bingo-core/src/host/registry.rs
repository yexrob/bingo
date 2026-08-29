//! Everything the loaded plugins contributed, in one place. A slot with a
//! single holder — the policy, the store, the compactor — refuses a second.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use std::collections::BTreeMap;

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
    /// Load plugins in the order given. One whose requirements no earlier
    /// plugin provides is disabled with a reason, never fatal.
    /// `slices` holds each plugin's claimed settings, by plugin id.
    pub(super) fn load(
        plugins: &[Box<dyn Plugin>],
        slices: &BTreeMap<String, Value>,
        env: &Env,
    ) -> Result<Self, HostError> {
        let mut registry = Registry::default();
        let mut provided: HashSet<&'static str> = HashSet::new();
        for plugin in plugins {
            let manifest = plugin.manifest();
            if let Some(reason) = unmet(manifest, &provided) {
                tracing::warn!(plugin = manifest.id, %reason, "plugin disabled");
                registry
                    .plugins
                    .push(PluginStatus::disabled(manifest, reason));
                continue;
            }
            registry.register(plugin.as_ref(), slices, env)?;
            provided.extend(manifest.provides.iter().copied());
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
            Contribution::Provider(provider) => self.add_provider(provider),
            Contribution::Policy(policy) => self.set_policy(policy),
            Contribution::Hook(hook) => {
                self.hooks.push(hook);
                Ok(())
            }
            Contribution::Context(contributor) => {
                self.contributors.push(contributor);
                Ok(())
            }
            Contribution::Command(command) => self.add_command(command),
            Contribution::Surface(surface) => self.add_surface(surface),
            Contribution::Store(store) => self.set_store(store),
            Contribution::Compactor(compactor) => self.set_compactor(compactor),
            Contribution::Service { key, value } => self.add_service(key, value),
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

    fn add_service(
        &mut self,
        key: String,
        value: Arc<dyn Any + Send + Sync>,
    ) -> Result<(), String> {
        if self.services.contains_key(&key) {
            return Err(format!("service {key} is already registered"));
        }
        self.services.insert(key, value);
        Ok(())
    }

    pub(super) fn enabled(&self, plugin: &str) -> bool {
        self.plugins.iter().any(|p| p.id == plugin && p.enabled)
    }
}

/// The requirements nobody has provided yet, as a reason to disable.
fn unmet(manifest: &PluginManifest, provided: &HashSet<&'static str>) -> Option<String> {
    let missing: Vec<&str> = manifest
        .requires
        .iter()
        .copied()
        .filter(|r| !provided.contains(r))
        .collect();
    (!missing.is_empty()).then(|| format!("unmet requirements: {}", missing.join(", ")))
}

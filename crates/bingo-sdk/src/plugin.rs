//! How a plugin enters the process: a static manifest, one synchronous
//! `register` that only adds contributions, and `start`/`stop` for I/O.

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::command::Command;
use crate::compactor::Compactor;
use crate::contributor::ContextContributor;
use crate::hook::Hook;
use crate::host::HostHandle;
use crate::policy::PermissionPolicy;
use crate::provider::Provider;
use crate::store::SessionStore;
use crate::surface::Surface;
use crate::tool::Tool;

#[derive(Clone, Copy, Debug)]
pub struct PluginManifest {
    /// Reverse-dotted, e.g. `bingo.tools.fs`.
    pub id: &'static str,
    pub version: &'static str,
    /// Semver requirement on `bingo-sdk`, checked at boot.
    pub sdk: &'static str,
    /// Capabilities offered, as `kind:name` (`tool:Read`, `provider:anthropic`, `service:bingo.checkpoint`).
    pub provides: &'static [&'static str],
    /// Capabilities needed. Missing ones disable the plugin with a notice; they never crash.
    pub requires: &'static [&'static str],
    pub config: Option<ConfigClaim>,
}

/// The top-level settings keys a plugin owns, how each merges across layers,
/// and the schema the loader validates the slice against.
#[derive(Clone, Copy, Debug)]
pub struct ConfigClaim {
    pub keys: &'static [(&'static str, Merge)],
    pub schema: fn() -> schemars::Schema,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Merge {
    /// The higher layer wins.
    Replace,
    /// Lists concatenate across layers (permission rules, disabled servers).
    Accumulate,
    /// Objects merge per key (named providers, MCP servers).
    ByName,
}

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("configuration: {0}")]
    Config(String),
    #[error("unmet requirement: {0}")]
    Unmet(String),
    #[error("{0}")]
    Failed(String),
}

#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    fn manifest(&self) -> &'static PluginManifest;

    /// Synchronous, in dependency order. Only registers; does no I/O.
    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError>;

    /// After every plugin has registered. May spawn tasks.
    async fn start(&self, _host: HostHandle) -> Result<(), PluginError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), PluginError> {
        Ok(())
    }
}

/// What a plugin hands the host. One enum so the in-process path and a future
/// out-of-process bridge share one representation.
#[non_exhaustive]
pub enum Contribution {
    Tool(Arc<dyn Tool>),
    Provider(Arc<dyn Provider>),
    Policy(Arc<dyn PermissionPolicy>),
    Hook(Arc<dyn Hook>),
    Context(Arc<dyn ContextContributor>),
    Command(Arc<dyn Command>),
    Surface(Arc<dyn Surface>),
    Store(Arc<dyn SessionStore>),
    Compactor(Arc<dyn Compactor>),
    /// A typed value other plugins may look up by key (`service:<key>` in the manifest).
    Service {
        key: String,
        value: Arc<dyn Any + Send + Sync>,
    },
}

impl fmt::Debug for Contribution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Contribution::Tool(t) => write!(f, "Tool({})", t.spec().name),
            Contribution::Provider(p) => write!(f, "Provider({})", p.id()),
            Contribution::Policy(p) => write!(f, "Policy({})", p.id()),
            Contribution::Hook(h) => write!(f, "Hook({})", h.id()),
            Contribution::Context(c) => write!(f, "Context({})", c.id()),
            Contribution::Command(c) => write!(f, "Command({})", c.spec().name),
            Contribution::Surface(s) => write!(f, "Surface({})", s.id()),
            Contribution::Store(_) => write!(f, "Store"),
            Contribution::Compactor(_) => write!(f, "Compactor"),
            Contribution::Service { key, .. } => write!(f, "Service({key})"),
        }
    }
}

/// Handed to `Plugin::register`. Owned by the host.
#[derive(Debug)]
pub struct Registrar {
    plugin_id: String,
    config: Value,
    contributions: Vec<Contribution>,
}

impl Registrar {
    pub fn new(plugin_id: impl Into<String>, config: Value) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            config,
            contributions: Vec::new(),
        }
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// The plugin's claimed configuration slice, already merged and validated.
    pub fn config<T: DeserializeOwned>(&self) -> Result<T, PluginError> {
        serde_json::from_value(self.config.clone()).map_err(|e| PluginError::Config(e.to_string()))
    }

    pub fn add(&mut self, contribution: Contribution) {
        self.contributions.push(contribution);
    }

    pub fn tool(&mut self, tool: Arc<dyn Tool>) {
        self.add(Contribution::Tool(tool));
    }

    pub fn provider(&mut self, provider: Arc<dyn Provider>) {
        self.add(Contribution::Provider(provider));
    }

    pub fn surface(&mut self, surface: Arc<dyn Surface>) {
        self.add(Contribution::Surface(surface));
    }

    pub fn into_contributions(self) -> Vec<Contribution> {
        self.contributions
    }
}

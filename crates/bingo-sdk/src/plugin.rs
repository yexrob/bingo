//! How a plugin enters the process: a static manifest, one synchronous
//! `register` that only adds contributions, and `start`/`stop` for I/O.

use std::any::Any;
use std::fmt;
use std::path::Path;
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
use crate::tool::{Env, Tool};

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

/// Tools that exist only after I/O — a server's, once it has answered. Read
/// when a turn starts; answers from what it has now (ADR-0009).
#[async_trait]
pub trait ToolSource: Send + Sync {
    fn id(&self) -> &str;
    async fn tools(&self) -> Vec<Arc<dyn Tool>>;
}

/// Commands that exist only after I/O — a directory's skills. Read when a
/// name is not in the static table (ADR-0009), for the directory the
/// session works in: what `/name` means depends on where it is typed.
#[async_trait]
pub trait CommandSource: Send + Sync {
    fn id(&self) -> &str;
    async fn commands(&self, cwd: &Path) -> Vec<Arc<dyn Command>>;
}

/// Contributors that exist only after I/O — an external process's, once it has
/// answered the handshake. Read when a turn starts, beside the tool sources
/// (ADR-0009 §1, ADR-0030 §2).
#[async_trait]
pub trait ContextSource: Send + Sync {
    fn id(&self) -> &str;
    async fn contributors(&self) -> Vec<Arc<dyn ContextContributor>>;
}

/// Providers that exist only after I/O — an external process's, once it has
/// answered the handshake. Read where a provider is resolved: when a session
/// chooses its model, and when a catalogue is read (ADR-0009 §1, ADR-0030 §2).
#[async_trait]
pub trait ProviderSource: Send + Sync {
    fn id(&self) -> &str;
    async fn providers(&self) -> Vec<Arc<dyn Provider>>;
}

/// Compaction strategies that exist only after I/O. Read when a turn starts;
/// the slot holds one, so a source's strategy is the turn's only where nothing
/// in-process already holds it.
#[async_trait]
pub trait CompactorSource: Send + Sync {
    fn id(&self) -> &str;
    async fn compactors(&self) -> Vec<Arc<dyn Compactor>>;
}

/// What a plugin hands the host. One enum so the in-process path and a future
/// out-of-process bridge share one representation.
pub enum Contribution {
    Tool(Arc<dyn Tool>),
    /// Tools resolved late, from a source that does its I/O elsewhere.
    Tools(Arc<dyn ToolSource>),
    Provider(Arc<dyn Provider>),
    /// Providers resolved late, from a source that does its I/O elsewhere.
    Providers(Arc<dyn ProviderSource>),
    Policy(Arc<dyn PermissionPolicy>),
    Hook(Arc<dyn Hook>),
    Context(Arc<dyn ContextContributor>),
    /// Contributors resolved late, from a source that does its I/O elsewhere.
    Contexts(Arc<dyn ContextSource>),
    Command(Arc<dyn Command>),
    /// Commands resolved late, from a source that does its I/O elsewhere.
    Commands(Arc<dyn CommandSource>),
    Surface(Arc<dyn Surface>),
    Store(Arc<dyn SessionStore>),
    Compactor(Arc<dyn Compactor>),
    /// Compaction strategies resolved late, ditto.
    Compactors(Arc<dyn CompactorSource>),
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
            Contribution::Tools(s) => write!(f, "Tools({})", s.id()),
            Contribution::Provider(p) => write!(f, "Provider({})", p.id()),
            Contribution::Providers(s) => write!(f, "Providers({})", s.id()),
            Contribution::Policy(p) => write!(f, "Policy({})", p.id()),
            Contribution::Hook(h) => write!(f, "Hook({})", h.id()),
            Contribution::Context(c) => write!(f, "Context({})", c.id()),
            Contribution::Contexts(s) => write!(f, "Contexts({})", s.id()),
            Contribution::Command(c) => write!(f, "Command({})", c.spec().name),
            Contribution::Commands(s) => write!(f, "Commands({})", s.id()),
            Contribution::Surface(s) => write!(f, "Surface({})", s.id()),
            Contribution::Store(_) => write!(f, "Store"),
            Contribution::Compactor(_) => write!(f, "Compactor"),
            Contribution::Compactors(s) => write!(f, "Compactors({})", s.id()),
            Contribution::Service { key, .. } => write!(f, "Service({key})"),
        }
    }
}

/// Handed to `Plugin::register`. Owned by the host.
#[derive(Debug)]
pub struct Registrar {
    plugin_id: String,
    config: Value,
    env: Env,
    contributions: Vec<Contribution>,
}

impl Registrar {
    pub fn new(plugin_id: impl Into<String>, config: Value, env: Env) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            config,
            env,
            contributions: Vec::new(),
        }
    }

    /// Where the host lives: home, config and data directories.
    pub fn env(&self) -> &Env {
        &self.env
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

//! How a plugin enters the process: a static manifest, one synchronous
//! `register` that only adds contributions, and `start`/`stop` for I/O.

mod contents;
mod lifecycle;
mod reload;
pub use contents::PluginContents;
pub use lifecycle::PluginLifecycle;
mod settings;
mod state;
pub use reload::PluginReload;
pub use settings::{ConfigLayer, ConfigOrigin, ConfigSources};

use std::any::Any;
use std::collections::BTreeSet;
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
use crate::provider::{Provider, ProviderShape};
use crate::service::WireService;
use crate::store::SessionStore;
use crate::surface::Surface;
use crate::tool::{Env, Tool};

#[derive(Clone, Copy, Debug)]
pub struct PluginManifest {
    /// Reverse-dotted, e.g. `bingo.tools.fs`.
    pub id: &'static str,
    pub version: &'static str,
    /// Optional service keys. Missing ones do not disable this plugin.
    pub uses: &'static [&'static str],
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
    pub approve: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Merge {
    /// The higher layer wins.
    Replace,
    /// Lists concatenate across layers (permission rules, disabled servers).
    Accumulate,
    /// Accumulate only personal or explicitly supplied layers, never repository layers.
    PersonalAccumulate,
    /// Objects merge per key (named providers, MCP servers).
    ByName,
}

/// Where a plugin in a listing came from: this build itself, as opposed to a
/// source that found one on the machine (ADR-0057 §5).
pub const BUILT_IN: &str = "built in";

/// Why a plugin the settings switched off is not standing (ADR-0057 §2). The
/// kernel says it of a plugin the build ships and a source says it of one it
/// found for itself, so the sentence is written once.
pub const SWITCHED_OFF: &str = "switched off in the settings";

/// The directory that supplied an external plugin.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum PluginOrigin {
    User,
    Project,
    Explicit,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum PluginRuntime {
    Process,
    Module,
}

#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct PluginGrant {
    pub tier: crate::Tier,
    pub uses: BTreeSet<String>,
}

/// The registration chain may reduce a grant or refuse the plugin in words.
#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(untagged)]
pub enum PluginDecision {
    Granted(PluginGrant),
    Denied { reason: String },
}

/// Where an external plugin came from and how it runs: known as soon as a
/// source finds it, before any grant is decided.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct PluginPlacement {
    pub origin: PluginOrigin,
    pub runtime: PluginRuntime,
}

#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct PluginExecution {
    pub origin: PluginOrigin,
    pub runtime: PluginRuntime,
    #[serde(flatten)]
    pub grant: PluginGrant,
}

#[derive(
    Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct PluginRegistration {
    pub name: String,
    pub version: String,
    #[serde(flatten)]
    pub execution: PluginExecution,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum PluginState {
    On,
    Off {
        why: String,
    },
    Failed {
        why: String,
    },
    Waiting {
        changed: bool,
    },
    #[serde(rename = "pending")]
    Registering,
    Pending {
        now: bool,
        at: NextStart,
    },
}

impl PluginState {
    pub fn is_on(&self) -> bool {
        matches!(self, Self::On | Self::Pending { now: true, .. })
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::On => "on",
            Self::Off { .. } => "off",
            Self::Failed { .. } => "failed",
            Self::Waiting { .. } => "waiting",
            Self::Registering => "registering",
            Self::Pending { .. } => "pending",
        }
    }
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::On => None,
            Self::Off { why } | Self::Failed { why } => Some(why),
            Self::Waiting { changed: true } => Some("changed since your yes"),
            Self::Waiting { changed: false } => Some("waits for your yes"),
            Self::Registering => Some("registration pending"),
            Self::Pending {
                now: true,
                at: NextStart::AfterTurn,
            } => Some("off after the running turn"),
            Self::Pending {
                now: false,
                at: NextStart::AfterTurn,
            } => Some("on after the running turn"),
            Self::Pending { now: true, .. } => Some("off at next start"),
            Self::Pending { now: false, .. } => Some("on at next start"),
        }
    }
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum NextStart {
    NextStart,
    AfterTurn,
}

/// When a contribution set can honor a switch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginSwitch {
    Now,
    AfterTurn,
    NextStart { why: String },
}

/// What one plugin is, for a listing: what it is called, whether it will run,
/// and why not. It covers a plugin the kernel registered and one only a
/// [`PluginSource`] has ever seen, so the two draw as one table (ADR-0057 §5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginStatus {
    pub id: String,
    pub version: String,
    pub state: PluginState,
    /// [`BUILT_IN`], or the id of the source that listed it.
    pub from: String,
    /// Absent for a plugin this build ships.
    pub placement: Option<PluginPlacement>,
    /// What the registration chain granted; absent until it has.
    pub grant: Option<PluginGrant>,
}

impl PluginStatus {
    /// One this build ships, standing.
    pub fn loaded(manifest: &PluginManifest) -> Self {
        Self {
            id: manifest.id.to_string(),
            version: manifest.version.to_string(),
            state: PluginState::On,
            from: BUILT_IN.to_string(),
            placement: None,
            grant: None,
        }
    }

    /// One this build ships that will not run, and why.
    pub fn disabled(manifest: &PluginManifest, reason: impl Into<String>) -> Self {
        Self {
            state: PluginState::Off { why: reason.into() },
            ..Self::loaded(manifest)
        }
    }
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

/// Hooks that exist only after I/O — an external process's, once it has
/// answered the handshake. Read wherever the kernel reads its hooks, which is
/// the one place they are resolved (ADR-0009 §1, ADR-0030 §2, ADR-0032 §1).
#[async_trait]
pub trait HookSource: Send + Sync {
    fn id(&self) -> &str;
    async fn hooks(&self) -> Vec<Arc<dyn Hook>>;
}

/// Plugins the kernel never registered — an external process a bridge found
/// on this machine. Read where a listing of plugins is drawn; answering with
/// nothing before the discovery has happened is never wrong (ADR-0009 §1,
/// ADR-0057 §5).
#[async_trait]
pub trait PluginSource: Send + Sync {
    fn id(&self) -> &str;
    async fn plugins(&self) -> Vec<PluginStatus>;
    async fn contents(&self, _name: &str) -> PluginContents {
        PluginContents::default()
    }
    /// The discovery record owns the repository root, independently of a session cwd.
    async fn approval(&self, _name: &str) -> Option<crate::Approval> {
        None
    }
    /// Rescan reloadable plugins; sources without live reload return no changes.
    async fn reload(&self) -> Result<PluginReload, crate::KernelError> {
        Ok(PluginReload::default())
    }
    /// Classify a discovered plugin without changing its admission or generation.
    async fn switch_kind(&self, _name: &str) -> PluginSwitch {
        PluginSwitch::NextStart {
            why: "process plugin".into(),
        }
    }
    /// Apply a live switch through the owner's admission and reload lifecycle.
    async fn switch(&self, name: &str, _enabled: bool) -> Result<PluginSwitch, crate::KernelError> {
        Ok(self.switch_kind(name).await)
    }
    /// Claim pending generations once, before they may contribute links.
    async fn registrations(
        &self,
        _trust: &std::collections::BTreeMap<String, crate::PluginTrust>,
    ) -> Vec<PluginRegistration> {
        Vec::new()
    }
    async fn registered(&self, _name: &str, _grant: Result<PluginGrant, String>) {}
}

/// A default sits after every ordinary link and before the engine.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum Placement {
    #[default]
    Outer,
    Builtin,
}

/// What a plugin hands the host. One enum so the in-process path and a future
/// out-of-process bridge share one representation.
pub enum Contribution {
    Tool(Arc<dyn Tool>),
    /// Tools resolved late, from a source that does its I/O elsewhere.
    Tools(Arc<dyn ToolSource>),
    Provider(Arc<dyn Provider>),
    ProviderShape(Arc<dyn ProviderShape>),
    Agent(Arc<dyn crate::Agent>),
    /// Providers resolved late, from a source that does its I/O elsewhere.
    Providers(Arc<dyn ProviderSource>),
    Policy(Arc<dyn PermissionPolicy>),
    Hook(Arc<dyn Hook>),
    /// Native owner-scoped cleanup after a confirmed live disable.
    Lifecycle(Arc<dyn PluginLifecycle>),
    Interceptor {
        placement: Placement,
        external: Option<String>,
        link: Arc<dyn crate::Interceptor>,
    },
    Interceptors(Arc<dyn crate::InterceptorSource>),
    /// Hooks resolved late, from a source that does its I/O elsewhere.
    Hooks(Arc<dyn HookSource>),
    Context(Arc<dyn ContextContributor>),
    /// Contributors resolved late, from a source that does its I/O elsewhere.
    Contexts(Arc<dyn ContextSource>),
    Command(Arc<dyn Command>),
    CommandFold(crate::CommandFold),
    /// Commands resolved late, from a source that does its I/O elsewhere.
    Commands(Arc<dyn CommandSource>),
    Surface(Arc<dyn Surface>),
    Store(Arc<dyn SessionStore>),
    Compactor(Arc<dyn Compactor>),
    /// Compaction strategies resolved late, ditto.
    Compactors(Arc<dyn CompactorSource>),
    /// Plugins of its own this one knows about, for a listing to draw beside
    /// the ones the kernel registered (ADR-0057 §5).
    Plugins(Arc<dyn PluginSource>),
    /// A typed value other plugins may look up by key (`service:<key>` in the
    /// manifest), and — when its owner opened one — the wire face that lets a
    /// process call it. Two faces of one live object; without the second the
    /// service does not exist across a process line (ADR-0031 §3).
    Service {
        key: String,
        value: Arc<dyn Any + Send + Sync>,
        wire: Option<Arc<dyn WireService>>,
    },
}

impl Contribution {
    pub fn service<T: ?Sized + Any + Send + Sync>(
        key: &crate::ServiceKey<T>,
        value: Arc<T>,
        wire: Option<Arc<dyn WireService>>,
    ) -> Self {
        Self::Service {
            key: key.name().to_string(),
            value: Arc::new(value),
            wire,
        }
    }
}

impl fmt::Debug for Contribution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Contribution::Tool(t) => write!(f, "Tool({})", t.spec().name),
            Contribution::Tools(s) => write!(f, "Tools({})", s.id()),
            Contribution::Agent(a) => write!(f, "Agent({})", a.id()),
            Contribution::ProviderShape(p) => write!(f, "ProviderShape({})", p.id()),
            Contribution::Provider(p) => write!(f, "Provider({})", p.id()),
            Contribution::Providers(s) => write!(f, "Providers({})", s.id()),
            Contribution::Policy(p) => write!(f, "Policy({})", p.id()),
            Contribution::Interceptor { external, link, .. } => {
                write!(f, "Interceptor({external:?}, {})", link.id())
            }
            Contribution::Interceptors(source) => write!(f, "Interceptors({})", source.id()),
            Contribution::Hook(h) => write!(f, "Hook({})", h.id()),
            Contribution::Lifecycle(_) => write!(f, "Lifecycle"),
            Contribution::Hooks(s) => write!(f, "Hooks({})", s.id()),
            Contribution::Context(c) => write!(f, "Context({})", c.id()),
            Contribution::Contexts(s) => write!(f, "Contexts({})", s.id()),
            Contribution::Command(c) => write!(f, "Command({})", c.spec().name),
            Contribution::CommandFold(fold) => f.debug_tuple("CommandFold").field(fold).finish(),
            Contribution::Commands(s) => write!(f, "Commands({})", s.id()),
            Contribution::Surface(s) => write!(f, "Surface({})", s.id()),
            Contribution::Store(_) => write!(f, "Store"),
            Contribution::Compactor(_) => write!(f, "Compactor"),
            Contribution::Compactors(s) => write!(f, "Compactors({})", s.id()),
            Contribution::Plugins(s) => write!(f, "Plugins({})", s.id()),
            Contribution::Service { key, .. } => write!(f, "Service({key})"),
        }
    }
}

/// Handed to `Plugin::register`. Owned by the host.
#[derive(Debug)]
pub struct Registrar {
    plugin_id: String,
    config: Value,
    sources: ConfigSources,
    env: Env,
    switched_off: BTreeSet<String>,
    contributions: Vec<Contribution>,
    notices: Vec<(String, String)>,
}

struct OneInterceptor {
    plugin: String,
    link: Arc<dyn crate::Interceptor>,
}
#[async_trait]
impl crate::InterceptorSource for OneInterceptor {
    fn id(&self) -> &str {
        &self.plugin
    }
    async fn interceptors(&self) -> Vec<crate::ExternalInterceptor> {
        vec![crate::ExternalInterceptor {
            plugin: self.plugin.clone(),
            link: self.link.clone(),
        }]
    }
}

impl Registrar {
    pub fn external_interceptor(
        &mut self,
        name: impl Into<String>,
        link: Arc<dyn crate::Interceptor>,
    ) {
        self.add(Contribution::Interceptors(Arc::new(OneInterceptor {
            plugin: name.into(),
            link,
        })));
    }

    pub fn new(
        plugin_id: impl Into<String>,
        config: Value,
        env: Env,
        switched_off: BTreeSet<String>,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            config,
            sources: ConfigSources::default(),
            env,
            switched_off,
            contributions: Vec::new(),
            notices: Vec::new(),
        }
    }

    pub fn with_sources(mut self, sources: ConfigSources) -> Self {
        self.sources = sources;
        self
    }

    pub fn sources(&self) -> &ConfigSources {
        &self.sources
    }

    /// Where the host lives: home, config and data directories.
    pub fn env(&self) -> &Env {
        &self.env
    }

    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// Every plugin name the settings switched off, the whole set rather than
    /// this plugin's own row: a plugin that hosts plugins of its own reads it
    /// to leave them alone, and one that hosts none ignores it and loses
    /// nothing (ADR-0057 §4).
    pub fn switched_off(&self) -> &BTreeSet<String> {
        &self.switched_off
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

    pub fn provider_shape(&mut self, shape: Arc<dyn ProviderShape>) {
        self.add(Contribution::ProviderShape(shape));
    }

    pub fn provider(&mut self, provider: Arc<dyn Provider>) {
        self.add(Contribution::Provider(provider));
    }

    pub fn agent(&mut self, agent: Arc<dyn crate::Agent>) {
        self.add(Contribution::Agent(agent));
    }

    pub fn surface(&mut self, surface: Arc<dyn Surface>) {
        self.add(Contribution::Surface(surface));
    }

    pub fn notice(&mut self, code: impl Into<String>, text: impl Into<String>) {
        self.notices.push((code.into(), text.into()));
    }

    pub fn take_notices(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.notices)
    }

    pub fn into_contributions(self) -> Vec<Contribution> {
        self.contributions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static MANIFEST: PluginManifest = PluginManifest {
        id: "test.plugin",
        version: "0.2.0",
        uses: &[],
        provides: &[],
        requires: &[],
        config: None,
    };

    fn registrar(switched_off: BTreeSet<String>) -> Registrar {
        Registrar::new(
            MANIFEST.id,
            Value::Null,
            Env::rooted("/nowhere"),
            switched_off,
        )
    }

    /// The kernel hands every plugin the whole set, so a plugin that runs
    /// plugins of its own can read it (ADR-0057 §4).
    #[test]
    fn a_registrar_answers_with_the_switches_it_was_built_with() {
        assert!(registrar(BTreeSet::new()).switched_off().is_empty());
        let off = BTreeSet::from(["wordcount".to_string(), "bingo.tools.web".to_string()]);
        let registrar = registrar(off.clone());
        assert_eq!(registrar.switched_off(), &off);
    }

    /// A status the kernel makes says the build is where the plugin came from;
    /// a disabled one keeps its id and version and adds the reason.
    #[test]
    fn a_built_in_status_names_the_build_and_carries_its_reason() {
        let loaded = PluginStatus::loaded(&MANIFEST);
        assert_eq!(
            (
                loaded.id.as_str(),
                loaded.version.as_str(),
                loaded.state.is_on()
            ),
            ("test.plugin", "0.2.0", true)
        );
        assert_eq!(loaded.from, BUILT_IN);
        assert_eq!(loaded.state.reason(), None);

        let disabled = PluginStatus::disabled(&MANIFEST, SWITCHED_OFF);
        assert!(!disabled.state.is_on());
        assert_eq!(disabled.state.reason(), Some(SWITCHED_OFF));
        assert_eq!(disabled.from, BUILT_IN);
    }
}

#[cfg(test)]
mod placement_contract {
    use super::Placement;
    #[test]
    fn placement_defaults_outer_and_uses_camel_case() {
        assert_eq!(Placement::default(), Placement::Outer);
        assert_eq!(serde_json::to_value(Placement::Builtin).unwrap(), "builtin");
        assert_eq!(
            serde_json::from_str::<Placement>("\"outer\"").unwrap(),
            Placement::Outer
        );
    }
}

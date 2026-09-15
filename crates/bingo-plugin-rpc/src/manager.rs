//! Every discovered plugin, and the one place a source read reaches them.
//!
//! Discovery is I/O, so it happens at `Plugin::start` rather than at
//! `register` (ADR-0001): the sources are registered first and answer with
//! nothing until the bridges exist, which is never wrong (ADR-0009 §1). The
//! set is fixed once discovered, so it is behind a `OnceLock` rather than a
//! lock a source read would have to take.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, OnceLock};

use bingo_sdk::{
    CancellationToken, Command, Compactor, ContextContributor, Env, Hook, HostHandle, PluginStatus,
    Provider, SWITCHED_OFF, Tool,
};
use serde_json::Value;

use crate::bridge::{Bridge, Setting};
use crate::discovery::{self, Found};
use crate::doors::{self, Caller, Doors};
use crate::notice::{self, Notices};
use crate::source::ID;
use crate::wire::HostEnv;

/// One plugin this machine has installed: what it is called, what its
/// manifest says it is, and the bridge that runs it — or nothing, where the
/// settings switched it off (ADR-0057 §4).
struct Installed {
    name: String,
    version: String,
    bridge: Option<Arc<Bridge>>,
}

impl Installed {
    /// What a listing shows about it, without spawning anything: a switched-off
    /// plugin has no process to ask, and a dead one answers with why.
    async fn status(&self) -> PluginStatus {
        let (enabled, reason) = match &self.bridge {
            Some(bridge) => bridge.standing().await,
            None => (false, Some(SWITCHED_OFF.to_string())),
        };
        PluginStatus {
            id: self.name.clone(),
            version: self.version.clone(),
            enabled,
            reason,
            from: ID.to_string(),
        }
    }
}

/// The bridges, and what they were built from.
pub struct Manager {
    env: Env,
    /// Each plugin's own settings slice, by plugin name.
    settings: BTreeMap<String, Value>,
    /// Every plugin name the settings turned off — this plugin's own among
    /// them, which is not this plugin's business (ADR-0057 §4).
    switched_off: BTreeSet<String>,
    notices: Arc<Notices>,
    /// The host's own service, built here so every bridge hands its process a
    /// face of the one object (ADR-0033 §1).
    doors: Arc<Doors>,
    /// Stops the one notice drain when the host does.
    stop: CancellationToken,
    installed: OnceLock<Vec<Installed>>,
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Manager")
            .field("plugins", &self.names())
            .finish_non_exhaustive()
    }
}

impl Manager {
    pub fn new(
        env: Env,
        settings: BTreeMap<String, Value>,
        switched_off: BTreeSet<String>,
    ) -> Self {
        let notices = Arc::new(Notices::default());
        Self {
            env,
            settings,
            switched_off,
            doors: Doors::new(Arc::clone(&notices)),
            notices,
            stop: CancellationToken::new(),
            installed: OnceLock::new(),
        }
    }

    pub fn notices(&self) -> &Arc<Notices> {
        &self.notices
    }

    /// Every discovered plugin's name, in the order a person reads them —
    /// the ones that are off included: they are installed either way.
    pub fn names(&self) -> Vec<&str> {
        self.discovered()
            .iter()
            .map(|installed| installed.name.as_str())
            .collect()
    }

    /// What a listing shows: every plugin installed on this machine, whether
    /// its process is up, and why it is not (ADR-0057 §5).
    pub async fn plugins(&self) -> Vec<PluginStatus> {
        let mut listed = Vec::new();
        for installed in self.discovered() {
            listed.push(installed.status().await);
        }
        listed
    }

    /// Read the two layers, then spawn and shake hands with every plugin at
    /// once, so ten plugins cost the slowest one rather than the sum. Returns
    /// when the last of them has answered or given up, so the first turn of a
    /// session has whatever they contribute; with nothing discovered it does
    /// nothing at all.
    pub async fn start(&self, cwd: &Path, host: HostHandle) {
        let found = discovery::discover(&discovery::dirs(&self.env, cwd), &self.notices);
        let installed: Vec<Installed> = found
            .into_iter()
            .map(|f| self.install(f, host.clone()))
            .collect();
        if self.installed.set(installed).is_err() {
            return;
        }
        self.open_doors(&host);
        self.say(host);
        let mut connecting = tokio::task::JoinSet::new();
        for bridge in self.bridges() {
            let bridge = Arc::clone(bridge);
            connecting.spawn(async move { bridge.connect().await });
        }
        while connecting.join_next().await.is_some() {}
    }

    pub async fn tools(&self) -> Vec<Arc<dyn Tool>> {
        let mut tools = Vec::new();
        for bridge in self.bridges() {
            tools.extend(bridge.tools().await);
        }
        tools
    }

    pub async fn commands(&self) -> Vec<Arc<dyn Command>> {
        let mut commands = Vec::new();
        for bridge in self.bridges() {
            commands.extend(bridge.commands().await);
        }
        commands
    }

    pub async fn contributors(&self) -> Vec<Arc<dyn ContextContributor>> {
        let mut contributors = Vec::new();
        for bridge in self.bridges() {
            contributors.extend(bridge.contributors().await);
        }
        contributors
    }

    pub async fn compactors(&self) -> Vec<Arc<dyn Compactor>> {
        let mut compactors = Vec::new();
        for bridge in self.bridges() {
            compactors.extend(bridge.compactors().await);
        }
        compactors
    }

    pub async fn providers(&self) -> Vec<Arc<dyn Provider>> {
        let mut providers = Vec::new();
        for bridge in self.bridges() {
            providers.extend(bridge.providers().await);
        }
        providers
    }

    pub async fn hooks(&self) -> Vec<Arc<dyn Hook>> {
        let mut hooks = Vec::new();
        for bridge in self.bridges() {
            hooks.extend(bridge.hooks().await);
        }
        hooks
    }

    pub async fn shutdown(&self) {
        self.stop.cancel();
        for bridge in self.bridges() {
            bridge.stop().await;
        }
    }

    /// The host's own service, in the registry under its reserved key, before
    /// any process is spawned: a plugin can call it from its first line, and
    /// no plugin can publish under the key because it is taken (ADR-0033 §1).
    /// The face here is bound to this process itself; each connection's hub
    /// holds the face bound to it.
    ///
    /// A host that keeps no registry at all is a line in the log and not a
    /// notice: every process still reaches these doors through its own hub, so
    /// there is nothing for a person to do about it.
    fn open_doors(&self, host: &HostHandle) {
        if let Err(why) = host.open_service(doors::KEY, self.doors.face(Caller::Host)) {
            tracing::debug!(key = doors::KEY, %why, "the host's own service is not in the registry");
        }
    }

    /// The one drain (ADR-0033 §4): from here on a notice is said when it
    /// happens, without waiting for a tool call — which is the defect M29
    /// carried, and why nothing else drains this channel.
    fn say(&self, host: HostHandle) {
        tokio::spawn(notice::drain(
            Arc::clone(&self.notices),
            host,
            self.stop.clone(),
        ));
    }

    fn discovered(&self) -> &[Installed] {
        self.installed.get().map(Vec::as_slice).unwrap_or_default()
    }

    /// The bridges there are to read: a plugin that is switched off has none,
    /// so nothing that gathers contributions has to know about switches.
    fn bridges(&self) -> impl Iterator<Item = &Arc<Bridge>> {
        self.discovered()
            .iter()
            .filter_map(|installed| installed.bridge.as_ref())
    }

    /// A discovered plugin, with a process unless the settings say otherwise:
    /// switched off, it is a directory that is read and a process that is
    /// never started (ADR-0057 §4).
    fn install(&self, found: Found, host: HostHandle) -> Installed {
        let name = found.name.clone();
        let version = found.manifest.version.clone();
        let on = !self.switched_off.contains(&name);
        Installed {
            name,
            version,
            bridge: on.then(|| self.bridge(found, host)),
        }
    }

    fn bridge(&self, found: Found, host: HostHandle) -> Arc<Bridge> {
        let config = self.settings.get(&found.name).cloned().unwrap_or_default();
        Arc::new(Bridge::new(
            found.name,
            found.root.clone(),
            found.manifest.entry.rooted(&found.root),
            config,
            Setting {
                env: HostEnv::from(&self.env),
                data_dir: self.env.data_dir.clone(),
                notices: Arc::clone(&self.notices),
                doors: Arc::clone(&self.doors),
                host,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_manager_that_has_discovered_nothing_answers_nothing() {
        let manager = Manager::new(Env::rooted("/nowhere"), BTreeMap::new(), BTreeSet::new());
        assert!(manager.tools().await.is_empty());
        assert!(manager.commands().await.is_empty());
        assert!(manager.contributors().await.is_empty());
        assert!(manager.compactors().await.is_empty());
        assert!(manager.providers().await.is_empty());
        assert!(manager.hooks().await.is_empty());
        assert!(manager.names().is_empty());
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn a_home_and_a_project_with_no_plugins_directory_start_quietly() {
        let home = tempfile::tempdir().expect("a home");
        let manager = Manager::new(Env::rooted(home.path()), BTreeMap::new(), BTreeSet::new());
        manager
            .start(home.path(), bingo_sdk::testing::NoHost::handle())
            .await;
        assert!(manager.names().is_empty());
        assert!(manager.notices().drain().is_empty());
    }

    /// A plugin whose command does not exist, so that a spawn is visible: it
    /// leaves a notice, and a plugin that is never spawned leaves none.
    fn installed(home: &Path, name: &str) {
        let root = home.join(".bingo/plugins").join(name);
        std::fs::create_dir_all(&root).expect("a plugin directory");
        let manifest = serde_json::json!({
            "name": name,
            "version": "0.4.2",
            "entry": { "command": "bingo-no-such-command" },
        });
        std::fs::write(root.join("plugin.json"), manifest.to_string()).expect("a manifest");
    }

    async fn started(home: &Path, switched_off: &[&str]) -> Manager {
        let off = switched_off.iter().map(|n| (*n).to_string()).collect();
        let manager = Manager::new(Env::rooted(home), BTreeMap::new(), off);
        manager
            .start(home, bingo_sdk::testing::NoHost::handle())
            .await;
        manager
    }

    /// A switched-off plugin is a directory that is read and a process that is
    /// never started: it is listed, off, with the reason — and nothing was
    /// spawned, which is why there is nothing to say about it (ADR-0057 §4).
    #[tokio::test]
    async fn a_switched_off_plugin_is_listed_and_never_spawned() {
        let home = tempfile::tempdir().expect("a home");
        installed(home.path(), "wordcount");
        let manager = started(home.path(), &["wordcount"]).await;

        assert_eq!(manager.names(), ["wordcount"], "it is installed either way");
        let listed = manager.plugins().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "wordcount");
        assert_eq!(listed[0].version, "0.4.2");
        assert!(!listed[0].enabled);
        assert_eq!(listed[0].reason.as_deref(), Some(SWITCHED_OFF));
        assert_eq!(listed[0].from, ID, "the source says where it came from");
        assert!(manager.tools().await.is_empty());
        assert!(
            manager.notices().drain().is_empty(),
            "a process that is never started cannot fail to start"
        );
        manager.shutdown().await;
    }

    /// The same directory, left alone: the process is spawned, the spawn
    /// fails because the command is not there, and the listing says so.
    #[tokio::test]
    async fn a_plugin_nobody_switched_off_is_spawned_and_answers_for_itself() {
        let home = tempfile::tempdir().expect("a home");
        installed(home.path(), "wordcount");
        let manager = started(home.path(), &[]).await;

        let listed = manager.plugins().await;
        assert!(!listed[0].enabled, "its command does not exist");
        let reason = listed[0].reason.clone().unwrap_or_default();
        assert_ne!(reason, SWITCHED_OFF, "nobody switched it off: {reason}");
        assert!(!reason.is_empty(), "a listing says why it is not running");
        let said = manager.notices().drain();
        assert_eq!(said.len(), 1, "{said:?}");
        assert_eq!(said[0].code, "PLUGIN_UNAVAILABLE");
        manager.shutdown().await;
    }
}

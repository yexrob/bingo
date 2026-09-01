//! Every discovered plugin, and the one place a source read reaches them.
//!
//! Discovery is I/O, so it happens at `Plugin::start` rather than at
//! `register` (ADR-0001): the sources are registered first and answer with
//! nothing until the bridges exist, which is never wrong (ADR-0009 §1). The
//! set is fixed once discovered, so it is behind a `OnceLock` rather than a
//! lock a source read would have to take.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use bingo_sdk::{Command, Compactor, ContextContributor, Env, HostHandle, Provider, Tool};
use serde_json::Value;

use crate::bridge::{Bridge, Setting};
use crate::discovery::{self, Found};
use crate::notice::Notices;
use crate::wire::HostEnv;

/// The bridges, and what they were built from.
pub struct Manager {
    env: Env,
    /// Each plugin's own settings slice, by plugin name.
    settings: BTreeMap<String, Value>,
    notices: Arc<Notices>,
    bridges: OnceLock<Vec<Arc<Bridge>>>,
}

impl std::fmt::Debug for Manager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Manager")
            .field("plugins", &self.names())
            .finish_non_exhaustive()
    }
}

impl Manager {
    pub fn new(env: Env, settings: BTreeMap<String, Value>) -> Self {
        Self {
            env,
            settings,
            notices: Arc::new(Notices::default()),
            bridges: OnceLock::new(),
        }
    }

    pub fn notices(&self) -> &Arc<Notices> {
        &self.notices
    }

    /// Every discovered plugin's name, in the order a person reads them.
    pub fn names(&self) -> Vec<&str> {
        self.bridges().iter().map(|b| b.name()).collect()
    }

    /// Read the two layers, then spawn and shake hands with every plugin at
    /// once, so ten plugins cost the slowest one rather than the sum. Returns
    /// when the last of them has answered or given up, so the first turn of a
    /// session has whatever they contribute; with nothing discovered it does
    /// nothing at all.
    pub async fn start(&self, cwd: &Path, host: HostHandle) {
        let found = discovery::discover(&discovery::dirs(&self.env, cwd), &self.notices);
        let bridges: Vec<Arc<Bridge>> = found
            .into_iter()
            .map(|f| self.bridge(f, host.clone()))
            .collect();
        if self.bridges.set(bridges).is_err() {
            return;
        }
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

    pub async fn shutdown(&self) {
        for bridge in self.bridges() {
            bridge.stop().await;
        }
    }

    fn bridges(&self) -> &[Arc<Bridge>] {
        self.bridges.get().map(Vec::as_slice).unwrap_or_default()
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
        let manager = Manager::new(Env::rooted("/nowhere"), BTreeMap::new());
        assert!(manager.tools().await.is_empty());
        assert!(manager.commands().await.is_empty());
        assert!(manager.contributors().await.is_empty());
        assert!(manager.compactors().await.is_empty());
        assert!(manager.providers().await.is_empty());
        assert!(manager.names().is_empty());
        manager.shutdown().await;
    }

    #[tokio::test]
    async fn a_home_and_a_project_with_no_plugins_directory_start_quietly() {
        let home = tempfile::tempdir().expect("a home");
        let manager = Manager::new(Env::rooted(home.path()), BTreeMap::new());
        manager
            .start(home.path(), bingo_sdk::testing::NoHost::handle())
            .await;
        assert!(manager.names().is_empty());
        assert!(manager.notices().drain().is_empty());
    }
}

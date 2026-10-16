//! Command ownership is retained until each lookup snapshots the running set.
use bingo_sdk::*;
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, RwLock},
};

#[derive(Default)]
pub struct RegisteredCommands {
    pub entries: Vec<OwnedCommand>,
    pub(super) folds: Vec<super::registry_folds::OwnedCommandFold>,
}

#[derive(Clone)]
pub struct OwnedCommand {
    pub plugin: String,
    pub command: Arc<dyn Command>,
}
impl std::ops::Deref for OwnedCommand {
    type Target = Arc<dyn Command>;
    fn deref(&self) -> &Self::Target {
        &self.command
    }
}

pub(super) struct Commands {
    pub table: Vec<OwnedCommand>,
    pub running: Arc<RwLock<BTreeSet<String>>>,
}
#[async_trait::async_trait]
impl CommandSource for Commands {
    fn id(&self) -> &str {
        "kernel.commands"
    }
    async fn commands(&self, _: &Path) -> Vec<Arc<dyn Command>> {
        let off = self.running.read().unwrap_or_else(|e| e.into_inner());
        let mut names = BTreeSet::new();
        self.table
            .iter()
            .filter(|entry| !off.contains(&entry.plugin) && names.insert(entry.command.spec().name))
            .map(|entry| entry.command.clone())
            .collect()
    }
}

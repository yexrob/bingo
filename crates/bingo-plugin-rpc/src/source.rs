//! The contributions the kernel reads when it needs the set (ADR-0009 §1): one
//! source per kind, for every plugin at once, because what a plugin
//! contributes is not known until its process has answered.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Command, CommandSource, Compactor, CompactorSource, ContextContributor, ContextSource, Hook,
    HookSource, Provider, ProviderSource, Tool, ToolSource,
};

use crate::manager::Manager;

/// The id both sources answer to, and the plugin's own short name.
pub const ID: &str = "plugin-rpc";

pub struct PluginTools {
    manager: Arc<Manager>,
}

impl PluginTools {
    pub fn new(manager: Arc<Manager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl ToolSource for PluginTools {
    fn id(&self) -> &str {
        ID
    }

    async fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.manager.tools().await
    }
}

pub struct PluginCommands {
    manager: Arc<Manager>,
}

impl PluginCommands {
    pub fn new(manager: Arc<Manager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl CommandSource for PluginCommands {
    fn id(&self) -> &str {
        ID
    }

    /// The directory does not choose the set: a plugin process is discovered
    /// once, at start, and is the same one wherever a `/name` is typed.
    async fn commands(&self, _cwd: &Path) -> Vec<Arc<dyn Command>> {
        self.manager.commands().await
    }
}

pub struct PluginContributors {
    manager: Arc<Manager>,
}

impl PluginContributors {
    pub fn new(manager: Arc<Manager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl ContextSource for PluginContributors {
    fn id(&self) -> &str {
        ID
    }

    async fn contributors(&self) -> Vec<Arc<dyn ContextContributor>> {
        self.manager.contributors().await
    }
}

pub struct PluginCompactors {
    manager: Arc<Manager>,
}

impl PluginCompactors {
    pub fn new(manager: Arc<Manager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl CompactorSource for PluginCompactors {
    fn id(&self) -> &str {
        ID
    }

    async fn compactors(&self) -> Vec<Arc<dyn Compactor>> {
        self.manager.compactors().await
    }
}

pub struct PluginHooks {
    manager: Arc<Manager>,
}

impl PluginHooks {
    pub fn new(manager: Arc<Manager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl HookSource for PluginHooks {
    fn id(&self) -> &str {
        ID
    }

    async fn hooks(&self) -> Vec<Arc<dyn Hook>> {
        self.manager.hooks().await
    }
}

pub struct PluginProviders {
    manager: Arc<Manager>,
}

impl PluginProviders {
    pub fn new(manager: Arc<Manager>) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl ProviderSource for PluginProviders {
    fn id(&self) -> &str {
        ID
    }

    async fn providers(&self) -> Vec<Arc<dyn Provider>> {
        self.manager.providers().await
    }
}

//! The two contributions the kernel reads when it needs the set (ADR-0009 §1):
//! one tool source and one command source for every plugin at once, because
//! what a plugin contributes is not known until its process has answered.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{Command, CommandSource, Tool, ToolSource};

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

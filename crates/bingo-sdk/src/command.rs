//! Slash commands. One registry serves dispatch, the catalog, completion and help.

use std::path::PathBuf;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::KernelError;
use crate::host::HostHandle;
use crate::ids::{ItemId, SessionId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandSpec {
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub hint: String,
    pub args: ArgSpec,
    /// May run while a turn is busy (read-only commands).
    #[serde(default)]
    pub instant: bool,
    pub family: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ArgSpec {
    None,
    Free {
        hint: String,
    },
    /// Completed from a catalog kind the command validates against.
    Catalog {
        source: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone)]
pub struct CommandContext {
    pub session: SessionId,
    pub cwd: PathBuf,
    pub host: HostHandle,
}

impl std::fmt::Debug for CommandContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandContext")
            .field("session", &self.session)
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
#[non_exhaustive]
pub enum CommandOutcome {
    Applied {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    View {
        view: View,
    },
    /// Becomes a turn.
    Prompt {
        text: String,
    },
    /// A long-running action recorded as an `Item::Action`.
    Action {
        item: ItemId,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum View {
    Text {
        text: String,
    },
    List {
        items: Vec<String>,
    },
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
    },
}

#[async_trait]
pub trait Command: Send + Sync {
    fn spec(&self) -> CommandSpec;

    fn complete(&self, _partial: &str, _cx: &CommandContext) -> Vec<Completion> {
        Vec::new()
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError>;
}

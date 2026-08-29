//! Slash commands. One registry serves dispatch, the catalog, completion and
//! help. The session actor parses `/name args`, `!line` and `Input::Action`,
//! runs the command on its own task and answers with an `IntentAck` whose
//! `Applied.result` is `{"message"}`, `{"view"}` or `{"item"}` (ADR-0008).

use std::path::PathBuf;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::KernelError;
use crate::event::ItemBody;
use crate::host::HostHandle;
use crate::ids::SessionId;

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
pub enum CommandOutcome {
    Applied {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    View {
        view: View,
    },
    /// Becomes a turn, submitted with the command's own intent and origin.
    Prompt {
        text: String,
    },
    /// One completed item the kernel records in the transcript (a shell
    /// line's output, a login's receipt); the ack carries its id.
    Record {
        body: ItemBody,
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

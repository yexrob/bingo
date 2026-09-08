//! Slash commands. One registry serves dispatch, the catalog and help; a
//! surface completes a command's argument from the `ArgSpec` its catalogue
//! entry carries, never by asking the command. The session actor parses `/name args`, `!line` and `Input::Action`,
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
use crate::view::View;

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
    /// One of the words the command lists itself, in the order it offers
    /// them. A surface completes from the list; the kernel carries it.
    Words {
        values: Vec<String>,
    },
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

#[async_trait]
pub trait Command: Send + Sync {
    fn spec(&self) -> CommandSpec;

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The shape a surface and a plugin both read: the variant is its `kind`,
    /// the words are `values`, in the command's own order.
    #[test]
    fn a_word_list_crosses_the_wire_as_its_kind_and_its_values() {
        let args = ArgSpec::Words {
            values: vec!["off".into(), "low".into()],
        };
        let wire = serde_json::to_value(&args).expect("serialises");
        assert_eq!(wire, json!({ "kind": "words", "values": ["off", "low"] }));
        let back: ArgSpec = serde_json::from_value(wire).expect("deserialises");
        assert_eq!(back, args);
    }
}

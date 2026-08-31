//! The three methods and the two notifications a plugin process speaks
//! (ADR-0015 §2): their names, and the shape of what goes in and comes back.
//!
//! Every params and result type is an sdk type or a struct of sdk types. The
//! bridge adds envelopes, never shapes: `ToolSpec`, `CommandSpec`,
//! `ToolOutput`, `CommandOutcome` and `Completion` cross verbatim, so a plugin
//! author writes against the kernel's own vocabulary.
//!
//! `METHODS` and `NOTIFICATIONS` are the one table: the schema walks it, and
//! the host and the example plugin both dispatch on the names in [`name`].

use std::path::PathBuf;

use bingo_sdk::{
    CommandOutcome, CommandSpec, Completion, Env, SessionId, ToolOutput, ToolSpec, TurnId,
};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The major the host speaks. A process that answers with another one is
/// refused rather than guessed at (ADR-0015 §Consequences).
pub const PROTOCOL: u32 = 1;

/// Every name that travels on the wire, in one place.
pub mod name {
    /// Kernel → plugin, once, before anything else.
    pub const INITIALIZE: &str = "initialize";
    /// Kernel → plugin: run one tool call.
    pub const TOOL_CALL: &str = "tool/call";
    /// Kernel → plugin: run one `/name`.
    pub const COMMAND_RUN: &str = "command/run";
    /// Kernel → plugin: what could follow this `/name`'s partial argument.
    pub const COMMAND_COMPLETE: &str = "command/complete";

    /// Plugin → kernel: replace a running call's live output line.
    pub const TOOL_PROGRESS: &str = "tool/progress";
    /// Kernel → plugin: the turn was interrupted; the call may stop itself.
    pub const TOOL_CANCEL: &str = "tool/cancel";
}

/// Where the host lives, as a process that is not in it can read.
///
/// A projection of the sdk's `Env`, which is not `Serialize`: a plugin that
/// keeps state of its own needs somewhere to put it, and `dataDir` is where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HostEnv {
    pub home: PathBuf,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

impl From<&Env> for HostEnv {
    fn from(env: &Env) -> Self {
        Self {
            home: env.home.clone(),
            config_dir: env.config_dir.clone(),
            data_dir: env.data_dir.clone(),
        }
    }
}

/// The handshake, sent once on a fresh process.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol: u32,
    /// The directory the manifest was read from, already resolved.
    pub plugin_root: PathBuf,
    /// This plugin's settings slice (`plugins.<name>`); `null` when unset.
    #[serde(default)]
    pub config: Value,
    pub env: HostEnv,
}

/// What the process says it is and what it contributes. Everything here is a
/// claim: the tools it names are registered untrusted (ADR-0015 §4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// The major the process speaks. The host refuses any but its own, which
    /// it can only do if the process says which one it is.
    pub protocol: u32,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub tools: Vec<ToolSpec>,
    #[serde(default)]
    pub commands: Vec<CommandSpec>,
}

/// One call, named as the plugin named it — the `plugin__<name>__` prefix is
/// the model's and the permission grammar's, never the process's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallParams {
    pub call_id: String,
    pub name: String,
    pub input: Value,
    pub cwd: PathBuf,
    pub session: SessionId,
    pub turn: TurnId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallResult {
    pub output: ToolOutput,
}

/// One `/name args` line, with the argument text exactly as it was typed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandRunParams {
    pub name: String,
    pub args: String,
    pub cwd: PathBuf,
    pub session: SessionId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandRunResult {
    pub outcome: CommandOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandCompleteParams {
    pub name: String,
    pub partial: String,
    pub cwd: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CommandCompleteResult {
    #[serde(default)]
    pub completions: Vec<Completion>,
}

/// Plugin → kernel, while a call runs: the whole of the live output line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolProgressParams {
    pub call_id: String,
    pub tail: String,
}

/// Kernel → plugin: this call's turn was interrupted. The host keeps waiting
/// for the answer — a bridge tool's `Interrupt` is `Block` — so a process that
/// ignores this is slow, not broken.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCancelParams {
    pub call_id: String,
}

/// Names a type in the schema: adds it to `$defs` and answers with its `$ref`.
pub type Ref = fn(&mut SchemaGenerator) -> Schema;

pub fn schema_of<T: JsonSchema>(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<T>()
}

/// A method: its name, its params, its result.
pub type Method = (&'static str, Ref, Ref);

/// A notification: its name and its params.
pub type Notification = (&'static str, Ref);

pub static METHODS: &[Method] = &[
    (
        name::INITIALIZE,
        schema_of::<InitializeParams>,
        schema_of::<InitializeResult>,
    ),
    (
        name::TOOL_CALL,
        schema_of::<ToolCallParams>,
        schema_of::<ToolCallResult>,
    ),
    (
        name::COMMAND_RUN,
        schema_of::<CommandRunParams>,
        schema_of::<CommandRunResult>,
    ),
    (
        name::COMMAND_COMPLETE,
        schema_of::<CommandCompleteParams>,
        schema_of::<CommandCompleteResult>,
    ),
];

pub static NOTIFICATIONS: &[Notification] = &[
    (name::TOOL_PROGRESS, schema_of::<ToolProgressParams>),
    (name::TOOL_CANCEL, schema_of::<ToolCancelParams>),
];

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_wire_has_four_methods_and_two_notifications() {
        assert_eq!(METHODS.len(), 4, "ADR-0015 §3 fixes the method count");
        assert_eq!(NOTIFICATIONS.len(), 2);
    }

    #[test]
    fn no_name_is_used_twice() {
        let mut names: Vec<&str> = METHODS
            .iter()
            .map(|method| method.0)
            .chain(NOTIFICATIONS.iter().map(|notification| notification.0))
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
    }

    #[test]
    fn the_handshake_travels_in_camel_case() {
        let params = InitializeParams {
            protocol: PROTOCOL,
            plugin_root: PathBuf::from("/plugins/wordcount"),
            config: json!({ "limit": 10 }),
            env: HostEnv::from(&Env::rooted("/home/u")),
        };
        let wire = serde_json::to_value(&params).expect("the handshake serialises");
        assert_eq!(wire["pluginRoot"], json!("/plugins/wordcount"));
        assert_eq!(wire["env"]["dataDir"], json!("/home/u/.bingo/data"));
        assert_eq!(
            serde_json::from_value::<InitializeParams>(wire).expect("and parses"),
            params
        );
    }

    /// A process that says nothing about what it contributes has contributed
    /// nothing, which is never wrong (ADR-0009 §1).
    #[test]
    fn a_handshake_may_name_no_tools_and_no_commands() {
        let result: InitializeResult = serde_json::from_value(json!({
            "protocol": 1, "name": "quiet", "version": "0.1.0"
        }))
        .expect("a handshake");
        assert!(result.tools.is_empty() && result.commands.is_empty());
    }

    #[test]
    fn a_call_carries_the_ids_the_kernel_minted() {
        let params = ToolCallParams {
            call_id: "call_1".into(),
            name: "count".into(),
            input: json!({ "path": "notes.txt" }),
            cwd: PathBuf::from("/work"),
            session: SessionId::from_raw("ses_1"),
            turn: TurnId::from_raw("trn_1"),
        };
        let wire = serde_json::to_value(&params).expect("a call serialises");
        assert_eq!(wire["callId"], json!("call_1"));
        assert_eq!(wire["session"], json!("ses_1"));
        assert_eq!(wire["turn"], json!("trn_1"));
    }

    #[test]
    fn an_outcome_crosses_as_the_sdk_writes_it() {
        let result = CommandRunResult {
            outcome: CommandOutcome::Applied {
                message: Some("counted".into()),
            },
        };
        let wire = serde_json::to_value(&result).expect("an outcome serialises");
        assert_eq!(wire["outcome"]["kind"], json!("applied"));
        assert_eq!(wire["outcome"]["message"], json!("counted"));
    }
}

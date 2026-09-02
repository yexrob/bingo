//! Tasks: a session's list of what is to be done, kept in the session's own
//! journal as the extension `bingo.tasks`/`tasks` (ADR-0011 §2). The list is
//! that payload and nothing else — every tool reads the snapshot, changes the
//! list and publishes the whole of it again, so a `--continue` reads back
//! what the last run wrote and every surface already knows how to draw it.
//!
//! Four tools, one prompt block, one command:
//!
//! - `TaskCreate` adds one and answers with the id the list gave it,
//!   `TaskUpdate` moves one on, `TaskGet` reads one in full, `TaskList` reads
//!   them all.
//! - A system block late in the prompt lists what is still open, so the model
//!   does not have to ask.
//! - `/tasks` shows a person the same list as a table.
//!
//! Every tool is declared read-only and trusted: none of them touches
//! anything outside this process. None is concurrency-safe — they share one
//! list, and two calls at once would each publish a list without the other's
//! change.
//!
//! A room's list is the same list: the four tools and `/tasks` take an
//! optional `in: "#room"`, and the journal's read and write already address
//! any session, so the board needs no second noun and no second store
//! (ADR-0023). Without `in`, every one of them means the caller's own session
//! and does exactly what it did before.

mod board;
mod command;
mod contributor;
mod create;
mod get;
mod journal;
mod list;
mod render;
mod task;
mod update;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Command, ContextContributor, Contribution, ErrorCode, KernelError, Plugin, PluginError,
    PluginManifest, Registrar, Tool, ToolError, ToolOutput, ToolTraits,
};

pub use command::TasksCommand;
pub use contributor::TasksContributor;
pub use create::TaskCreateTool;
pub use get::TaskGetTool;
pub use list::TaskListTool;
pub use task::{Change, Draft, Status, Task};
pub use update::TaskUpdateTool;

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.tasks",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &[
        "tool:TaskCreate",
        "tool:TaskUpdate",
        "tool:TaskGet",
        "tool:TaskList",
        "command:tasks",
        "context:tasks",
    ],
    requires: &[],
    // A task list is a session's own state, not a setting.
    config: None,
};

/// What every tool here is, with `in` and without it. They read and write a
/// session's journal and nothing outside the process, so trusting these traits
/// costs a person nothing: a board write reaches another session in the same
/// tree, which is what `SendMessage` already does under the same traits, and
/// whatever that session then decides is gated where it is decided. None is
/// concurrency-safe: the list is one value, and two calls running at once —
/// on one board, two of them — would each publish it without the other's
/// change (ADR-0023 §4).
pub(crate) fn traits() -> ToolTraits {
    ToolTraits {
        read_only: true,
        trusted: true,
        concurrency_safe: false,
        ..ToolTraits::default()
    }
}

/// The host said no; the call cannot go on, and the model is told why.
pub(crate) fn failed(error: KernelError) -> ToolError {
    ToolError::Failed(error.message)
}

/// An id the list does not have: an error the model reads and recovers from,
/// not a failed call.
pub(crate) fn unknown(id: u64) -> ToolOutput {
    ToolOutput::error(format!("No task #{id}. TaskList shows what there is."))
}

/// Finding out where a list lives, or what the caller is called, went wrong. A
/// name nothing answers to is the model's to correct, like an unknown id; a
/// host that failed under the walk is not, and fails the call.
pub(crate) fn misaddressed(error: KernelError) -> Result<ToolOutput, ToolError> {
    match error.code {
        ErrorCode::InvalidInput => Ok(ToolOutput::error(error.message)),
        _ => Err(failed(error)),
    }
}

/// Registers the four tools, the prompt block and `/tasks`. It keeps nothing
/// of its own: every one of them reaches the journal through the host it is
/// handed at the call.
#[derive(Debug, Default, Clone, Copy)]
pub struct TasksPlugin;

#[async_trait]
impl Plugin for TasksPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        registrar.tool(Arc::new(TaskCreateTool) as Arc<dyn Tool>);
        registrar.tool(Arc::new(TaskUpdateTool) as Arc<dyn Tool>);
        registrar.tool(Arc::new(TaskGetTool) as Arc<dyn Tool>);
        registrar.tool(Arc::new(TaskListTool) as Arc<dyn Tool>);
        registrar.add(Contribution::Command(
            Arc::new(TasksCommand) as Arc<dyn Command>
        ));
        registrar.add(Contribution::Context(
            Arc::new(TasksContributor) as Arc<dyn ContextContributor>
        ));
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use bingo_sdk::Env;

    #[test]
    fn the_manifest_says_what_it_provides_and_claims_no_settings() {
        assert_eq!(MANIFEST.id, "bingo.tasks");
        assert_eq!(
            MANIFEST.provides,
            [
                "tool:TaskCreate",
                "tool:TaskUpdate",
                "tool:TaskGet",
                "tool:TaskList",
                "command:tasks",
                "context:tasks",
            ]
        );
        assert!(MANIFEST.requires.is_empty());
        assert!(MANIFEST.config.is_none());
    }

    #[test]
    fn registering_reads_nothing_and_contributes_what_the_manifest_promises() {
        let mut registrar =
            Registrar::new("bingo.tasks", serde_json::Value::Null, Env::rooted("/tmp"));
        TasksPlugin.register(&mut registrar).expect("register");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), MANIFEST.provides.len());
        let tools: Vec<String> = contributions
            .iter()
            .filter_map(|c| match c {
                Contribution::Tool(tool) => Some(tool.spec().name),
                _ => None,
            })
            .collect();
        assert_eq!(tools, ["TaskCreate", "TaskUpdate", "TaskGet", "TaskList"]);
        assert!(matches!(contributions[4], Contribution::Command(_)));
        assert!(matches!(contributions[5], Contribution::Context(_)));
    }

    #[test]
    fn every_tool_is_read_only_trusted_and_alone() {
        let traits = traits();
        assert!(traits.read_only && traits.trusted);
        assert!(!traits.concurrency_safe && !traits.destructive && !traits.edit);
    }
}

//! One plugin command as a bingo `/name`.
//!
//! The name is the plugin's own, unprefixed: two plugins that both want
//! `/notes` collide by the registry's existing later-duplicate-dropped rule,
//! which is the same rule two skills of one name already meet (ADR-0015 §4).

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Command, CommandContext, CommandOutcome, CommandSpec, Completion, ErrorCode, KernelError,
};

use crate::completions::Completions;
use crate::connection::Connection;
use crate::wire::{
    CommandCompleteParams, CommandCompleteResult, CommandRunParams, CommandRunResult, name,
};

/// A command a plugin process advertised, bound to the pipe that answers it.
pub struct PluginCommand {
    plugin: String,
    spec: CommandSpec,
    connection: Arc<Connection>,
    completions: Arc<Completions>,
}

impl PluginCommand {
    pub fn new(
        plugin: &str,
        spec: CommandSpec,
        connection: Arc<Connection>,
        completions: Arc<Completions>,
    ) -> Self {
        Self {
            plugin: plugin.to_string(),
            spec,
            connection,
            completions,
        }
    }

    fn failed(&self, message: impl std::fmt::Display) -> KernelError {
        KernelError::new(ErrorCode::ToolFailed, format!("{}: {message}", self.plugin))
    }

    /// Ask what could follow `partial`, and keep the answer for the next
    /// keystroke. Runs on its own task: `Command::complete` cannot wait.
    async fn ask(
        connection: Arc<Connection>,
        completions: Arc<Completions>,
        params: CommandCompleteParams,
    ) {
        let (command, partial) = (params.name.clone(), params.partial.clone());
        let Ok(value) = serde_json::to_value(params) else {
            return;
        };
        let Ok(answer) = connection.request(name::COMMAND_COMPLETE, value).await else {
            return;
        };
        match serde_json::from_value::<CommandCompleteResult>(answer) {
            Ok(result) => completions.fill(&command, &partial, result.completions),
            Err(error) => tracing::debug!(%error, "a completion answer that is not one"),
        }
    }
}

#[async_trait]
impl Command for PluginCommand {
    fn spec(&self) -> CommandSpec {
        self.spec.clone()
    }

    /// What the last ask answered, and an ask when there has been none. The
    /// first keystroke of a partial therefore offers nothing, which is what
    /// answering from a cache costs.
    fn complete(&self, partial: &str, cx: &CommandContext) -> Vec<Completion> {
        if let Some(known) = self.completions.claim(&self.spec.name, partial) {
            return known;
        }
        let params = CommandCompleteParams {
            name: self.spec.name.clone(),
            partial: partial.to_string(),
            cwd: cx.cwd.clone(),
        };
        tokio::spawn(Self::ask(
            Arc::clone(&self.connection),
            Arc::clone(&self.completions),
            params,
        ));
        Vec::new()
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let params = CommandRunParams {
            name: self.spec.name.clone(),
            args: args.to_string(),
            cwd: cx.cwd.clone(),
            session: cx.session.clone(),
        };
        let value = serde_json::to_value(params).map_err(|e| self.failed(e))?;
        let answer = self
            .connection
            .request(name::COMMAND_RUN, value)
            .await
            .map_err(|error| self.failed(error.message))?;
        let result: CommandRunResult =
            serde_json::from_value(answer).map_err(|e| self.failed(e))?;
        Ok(result.outcome)
    }
}

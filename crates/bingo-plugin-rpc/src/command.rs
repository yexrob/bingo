//! One plugin command as a bingo `/name`.
//!
//! The name is the plugin's own, unprefixed: two plugins that both want
//! `/notes` collide by the registry's existing later-duplicate-dropped rule,
//! which is the same rule two skills of one name already meet (ADR-0015 §4).

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{Command, CommandContext, CommandOutcome, CommandSpec, ErrorCode, KernelError};

use crate::connection::Connection;
use crate::wire::{CommandRunParams, CommandRunResult, name};

/// A command a plugin process advertised, bound to the pipe that answers it.
pub struct PluginCommand {
    plugin: String,
    spec: CommandSpec,
    connection: Arc<Connection>,
}

impl PluginCommand {
    pub fn new(plugin: &str, spec: CommandSpec, connection: Arc<Connection>) -> Self {
        Self {
            plugin: plugin.to_string(),
            spec,
            connection,
        }
    }

    fn failed(&self, message: impl std::fmt::Display) -> KernelError {
        KernelError::new(ErrorCode::ToolFailed, format!("{}: {message}", self.plugin))
    }
}

#[async_trait]
impl Command for PluginCommand {
    fn spec(&self) -> CommandSpec {
        self.spec.clone()
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

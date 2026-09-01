//! One plugin tool as a bingo tool.
//!
//! Everything a process says about itself is a claim: the traits are
//! [`ToolTraits::default`], the fail-closed reading, so the gate asks about
//! every call however the plugin described it (ADR-0015 §4).

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits};
use serde_json::{Value, json};

use crate::codec::{INTERNAL_ERROR, RpcError};
use crate::connection::{Connection, Reply};
use crate::notice::{Notice, Notices};
use crate::wire::{ToolCallParams, ToolCallResult, name};

/// The model-visible name of a plugin's tool.
///
/// Both names are copied as they are. The permission grammar splits
/// `plugin__<plugin>__<tool>` on its first two separators, so a plugin or a
/// tool whose own name contains `__` still reaches the rule written for it,
/// and a rewritten name would reach none.
pub fn tool_name(plugin: &str, tool: &str) -> String {
    format!("plugin__{plugin}__{tool}")
}

/// A tool a plugin process advertised, bound to the pipe that answers it.
pub struct PluginTool {
    plugin: String,
    /// The plugin's own spec, whose `name` is what `tool/call` is sent.
    spec: ToolSpec,
    connection: Arc<Connection>,
    notices: Arc<Notices>,
}

impl PluginTool {
    pub fn new(
        plugin: &str,
        spec: ToolSpec,
        connection: Arc<Connection>,
        notices: Arc<Notices>,
    ) -> Self {
        Self {
            plugin: plugin.to_string(),
            spec,
            connection,
            notices,
        }
    }

    fn params(&self, input: Value, cx: &ToolContext) -> ToolCallParams {
        ToolCallParams {
            call_id: cx.call_id.clone(),
            name: self.spec.name.clone(),
            input,
            cwd: cx.cwd.clone(),
            session: cx.session.clone(),
            turn: cx.turn.clone(),
        }
    }

    /// Send the call, pass every progress line on as it arrives, and pass the
    /// turn's interrupt down as `tool/cancel` — the call is still awaited,
    /// because a bridge tool's `Interrupt` is `Block` and a write dropped
    /// mid-flight is in an unknown state.
    async fn exchange(&self, params: ToolCallParams, cx: &ToolContext) -> Reply {
        let (sender, mut tail) = tokio::sync::mpsc::unbounded_channel();
        let call_id = params.call_id.clone();
        let _watch = self
            .connection
            .watch(&call_id, sender, Arc::clone(&cx.call));
        let value = match serde_json::to_value(params) {
            Ok(value) => value,
            Err(error) => return Err(RpcError::new(INTERNAL_ERROR, error.to_string())),
        };
        let answered = self.connection.request(name::TOOL_CALL, value);
        tokio::pin!(answered);
        let mut cancelled = false;
        loop {
            tokio::select! {
                biased;
                Some(line) = tail.recv() => cx.progress(line),
                () = cx.cancel.cancelled(), if !cancelled => {
                    cancelled = true;
                    self.connection
                        .notify(name::TOOL_CANCEL, json!({ "callId": call_id }))
                        .await;
                }
                reply = &mut answered => return reply,
            }
        }
    }

    fn output(&self, reply: Reply) -> Result<ToolOutput, ToolError> {
        match reply {
            Ok(value) => serde_json::from_value::<ToolCallResult>(value)
                .map(|result| result.output)
                .map_err(|e| ToolError::Failed(format!("{}: {e}", self.plugin))),
            Err(error) => Err(ToolError::Failed(format!(
                "{}: {}",
                self.plugin, error.message
            ))),
        }
    }

    /// A call is where a death is usually first noticed: the reply that never
    /// came is what the pipe closing looks like from here. Saying it is not
    /// this call's job — the notice goes on the crate's one channel, and the
    /// one drain says it whether or not any tool is ever called again.
    fn announce(&self) {
        if self.connection.claim_death() {
            self.notices.push(Notice::warn(
                "PLUGIN_DIED",
                format!("the {} plugin process ended; restarting it", self.plugin),
            ));
        }
    }
}

/// What a catalogue may show beside the tool: which plugin it came from.
fn meta(plugin: &str) -> serde_json::Map<String, Value> {
    let mut meta = serde_json::Map::new();
    meta.insert("plugin".to_string(), Value::String(plugin.to_string()));
    meta
}

#[async_trait]
impl Tool for PluginTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: tool_name(&self.plugin, &self.spec.name),
            description: self.spec.description.clone(),
            input_schema: self.spec.input_schema.clone(),
            meta: meta(&self.plugin),
        }
    }

    /// Untrusted, whatever the plugin claimed about itself.
    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::default()
    }

    fn subjects(&self, _input: &Value, _cwd: &Path) -> Vec<bingo_sdk::Subject> {
        Vec::new()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let reply = self.exchange(self.params(input, cx), cx).await;
        self.announce();
        self.output(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_is_named_for_its_plugin_and_itself() {
        assert_eq!(tool_name("wordcount", "count"), "plugin__wordcount__count");
    }

    #[test]
    fn a_name_that_already_holds_the_separator_is_still_copied() {
        assert_eq!(tool_name("a__b", "c__d"), "plugin__a__b__c__d");
    }

    #[test]
    fn the_catalogue_learns_which_plugin_a_tool_came_from() {
        assert_eq!(meta("wordcount")["plugin"], json!("wordcount"));
    }
}

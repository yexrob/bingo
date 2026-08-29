//! A tool: a schema the model sees, fail-closed traits the gate reads, the
//! subjects a permission rule may match, an optional dry run, and the call.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::KernelError;
use crate::event::{ItemBody, Preview, ToolOutput};
use crate::host::{Input, Prompter, SessionSpec};
use crate::ids::{IntentId, ItemId, SessionId, TurnId};
use crate::model::ToolSpec;

/// What the gate and the executor may assume about a call. Every default is
/// the unsafe reading: not concurrency-safe, not read-only, finish on interrupt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolTraits {
    pub concurrency_safe: bool,
    pub read_only: bool,
    pub destructive: bool,
    /// Targets the working tree; `acceptEdits` mode auto-allows these.
    pub edit: bool,
    pub interrupt: Interrupt,
    pub result_limit: ResultLimit,
    /// Whether the gate may trust these traits at all. False for MCP tools,
    /// whose `readOnlyHint` is a claim, not a fact.
    pub trusted: bool,
}

impl Default for ToolTraits {
    fn default() -> Self {
        Self {
            concurrency_safe: false,
            read_only: false,
            destructive: false,
            edit: false,
            interrupt: Interrupt::Block,
            result_limit: ResultLimit::Global,
            trusted: false,
        }
    }
}

impl ToolTraits {
    pub fn read_only() -> Self {
        Self {
            concurrency_safe: true,
            read_only: true,
            trusted: true,
            interrupt: Interrupt::Cancel,
            ..Self::default()
        }
    }

    pub fn edit() -> Self {
        Self {
            edit: true,
            trusted: true,
            ..Self::default()
        }
    }

    pub fn destructive() -> Self {
        Self {
            destructive: true,
            trusted: true,
            ..Self::default()
        }
    }
}

/// What the executor does with an in-flight call when the turn is interrupted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupt {
    /// Let it finish; a remote write dropped mid-flight is in an unknown state.
    Block,
    /// Drop it; nothing outside the process changes.
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultLimit {
    /// The kernel clips the result at its global cap.
    Global,
    /// The tool bounds its own output; the kernel passes it through.
    SelfBounded,
}

/// What a permission rule may match against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Subject {
    Path { path: PathBuf },
    Command { command: String },
    Url { url: String },
    Name { name: String },
}

/// A call the model asked for, before or after the gate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub call_id: String,
    pub name: String,
    pub input: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("{0}")]
    Failed(String),
    #[error("cancelled")]
    Cancelled,
}

/// Process-wide facts a tool may need.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
}

/// What a tool may reach while it runs. Everything else is a service.
pub struct ToolContext {
    pub call_id: String,
    pub session: SessionId,
    pub turn: TurnId,
    pub item: ItemId,
    pub cwd: PathBuf,
    /// Child of the turn's token; honoured per `ToolTraits::interrupt`.
    pub cancel: CancellationToken,
    pub env: Arc<Env>,
    pub host: Arc<dyn ToolHost>,
}

impl std::fmt::Debug for ToolContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolContext")
            .field("call_id", &self.call_id)
            .field("session", &self.session)
            .field("turn", &self.turn)
            .field("item", &self.item)
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

impl ToolContext {
    /// Replace the running call's progress tail (the live output line).
    pub fn progress(&self, tail: impl Into<String>) {
        self.host.progress(&self.item, tail.into());
    }

    /// A service another plugin registered, by key.
    pub fn service<T: Any + Send + Sync>(&self, key: &str) -> Option<Arc<T>> {
        self.host
            .service_any(key)
            .and_then(|v| v.downcast::<T>().ok())
    }
}

/// The kernel-side capabilities a tool context delegates to.
#[async_trait]
pub trait ToolHost: Prompter {
    fn progress(&self, item: &ItemId, tail: String);
    /// Record an item outside the call (a background completion, a notice).
    async fn record(&self, body: ItemBody) -> Result<ItemId, KernelError>;
    /// The sub-agent primitive: a child session sharing this registry.
    async fn spawn_session(&self, spec: SessionSpec) -> Result<SessionId, KernelError>;
    /// The peer-messaging primitive: the target's queue is its inbox.
    fn submit(&self, to: &SessionId, intent: IntentId, input: Input);
    fn service_any(&self, key: &str) -> Option<Arc<dyn Any + Send + Sync>>;
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn spec(&self) -> ToolSpec;

    fn traits(&self, _input: &Value) -> ToolTraits {
        ToolTraits::default()
    }

    /// What a rule may match on. Bash → commands, Edit → paths, WebFetch → urls, Skill → names.
    fn subjects(&self, _input: &Value, _cwd: &Path) -> Vec<Subject> {
        Vec::new()
    }

    /// A decision only a person may take; forces a prompt in every mode.
    fn confirm(&self, _input: &Value) -> Option<String> {
        None
    }

    /// Dry run for the approval prompt. Reads, never writes.
    fn preview(&self, _input: &Value, _cwd: &Path) -> Option<Preview> {
        None
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError>;
}

/// The input schema for a tool's argument type, as the model receives it:
/// no `$schema` or `title`, and nested types inlined so no provider has to
/// resolve `$ref`.
pub fn input_schema<T: JsonSchema>() -> Value {
    let generator = schemars::generate::SchemaSettings::default()
        .with(|s| s.inline_subschemas = true)
        .into_generator();
    let mut schema = generator.into_root_schema_for::<T>().to_value();
    if let Some(obj) = schema.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
    }
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_fail_closed() {
        let t = ToolTraits::default();
        assert!(!t.concurrency_safe && !t.read_only && !t.trusted);
        assert_eq!(t.interrupt, Interrupt::Block);
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Args {
        /// The file to read.
        file_path: String,
        offset: Option<u32>,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    enum Mode {
        Files,
        Content,
    }

    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Nested {
        mode: Mode,
        items: Vec<Args>,
    }

    #[test]
    fn nested_types_are_inlined_not_referenced() {
        let text = input_schema::<Nested>().to_string();
        assert!(!text.contains("$ref"), "{text}");
        assert!(!text.contains("$defs"), "{text}");
        assert!(text.contains("\"Files\""));
    }

    #[test]
    fn input_schema_is_a_bare_object_schema() {
        let s = input_schema::<Args>();
        assert_eq!(s["type"], "object");
        assert!(s.get("$schema").is_none());
        assert_eq!(
            s["properties"]["file_path"]["description"],
            "The file to read."
        );
    }
}

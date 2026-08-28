//! The permission policy: one is active; the gate asks it and enforces the
//! answer. `Ask` is resolved by the gate through an interaction, never here.

use std::path::Path;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::SessionId;
use crate::tool::{Subject, ToolCall, ToolTraits};

#[derive(Clone, Copy, Debug)]
pub struct PolicyInput<'a> {
    pub call: &'a ToolCall,
    pub traits: &'a ToolTraits,
    pub subjects: &'a [Subject],
    /// The tool's own reason to force a prompt, if any.
    pub confirm: Option<&'a str>,
    pub session: &'a SessionId,
    pub cwd: &'a Path,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Decision {
    Allow {
        reason: Reason,
    },
    Deny {
        reason: Reason,
    },
    Ask {
        reason: Reason,
        /// The narrowest rule that would silence this prompt, offered as "allow for session".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Reason {
    Rule { rule: String },
    Mode { mode: String },
    Hook { hook: String },
    Safety { detail: String },
    ReadOnly,
    Confirm { detail: String },
    Default,
}

/// What the person decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow { scope: Option<String> },
    Deny { feedback: Option<String> },
}

#[async_trait]
pub trait PermissionPolicy: Send + Sync {
    fn id(&self) -> &str;

    async fn decide(&self, input: PolicyInput<'_>) -> Decision;

    /// Install the session-scoped rule the user accepted. Never persisted by the kernel.
    async fn on_verdict(&self, _input: PolicyInput<'_>, _verdict: &Verdict) {}
}

//! Typed lifecycle interceptors. Shell hooks are one plugin implementing this.
//!
//! The point, the matcher and the outcome are serializable because a hook may
//! live in another process (ADR-0032 §1): the bridge sends these types as the
//! sdk writes them rather than keeping copies of its own. `HookOutcome` has no
//! `Allow` and gains none for the wire — an external hook can only ever tighten
//! what happens, which is the whole of ADR-0032 §4.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::{Frame, Item, ToolOutput};
use crate::host::{HostHandle, Input};
use crate::ids::{SessionId, TurnId};
use crate::provider::Provider;
use crate::tool::ToolCall;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum HookPoint {
    Submit,
    BeforeTool,
    AfterTool,
    Stop,
    Turn,
    Compact,
    Session,
    Event,
}

/// Which points a hook wants, so the kernel skips the rest cheaply.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HookMatcher {
    /// Empty wants every point.
    #[serde(default)]
    pub points: Vec<HookPoint>,
    /// Anchored regex on the tool name for the tool points.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

/// What a hook decided. There is no `Allow`: a hook tightens what happens or
/// stands aside, and nothing here can widen a permission the policy refused.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum HookOutcome {
    Continue,
    Deny {
        reason: String,
    },
    Ask {
        reason: String,
    },
    /// `after_tool`: end the turn after this round. `on_stop`: loop once more with the reason.
    Block {
        reason: String,
    },
    /// `on_submit`: deliver to another session instead.
    Redirect {
        session: SessionId,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Start,
    End,
}

#[derive(Clone)]
pub struct HookContext {
    pub session: SessionId,
    pub turn: Option<TurnId>,
    pub cwd: PathBuf,
    /// The session's provider and model, for a hook that asks the model
    /// (memory extraction at turn end). Absent outside a session, and for a
    /// session nothing answers.
    pub provider: Option<Arc<dyn Provider>>,
    pub model: Option<String>,
    /// The whole host (ADR-0011 §3), for a hook that reads the session tree
    /// or writes into another session.
    pub host: HostHandle,
}

impl std::fmt::Debug for HookContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HookContext")
            .field("session", &self.session)
            .field("turn", &self.turn)
            .field("cwd", &self.cwd)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

#[async_trait]
pub trait Hook: Send + Sync {
    fn id(&self) -> &str;

    fn matcher(&self) -> HookMatcher;

    async fn on_submit(&self, _input: &mut Input, _cx: &HookContext) -> HookOutcome {
        HookOutcome::Continue
    }

    /// May rewrite the input.
    async fn before_tool(&self, _call: &mut ToolCall, _cx: &HookContext) -> HookOutcome {
        HookOutcome::Continue
    }

    async fn after_tool(
        &self,
        _call: &ToolCall,
        _output: &ToolOutput,
        _cx: &HookContext,
    ) -> HookOutcome {
        HookOutcome::Continue
    }

    async fn on_stop(&self, _cx: &HookContext) -> HookOutcome {
        HookOutcome::Continue
    }

    async fn on_turn(&self, _phase: Phase, _turn: &TurnId, _items: &[Item], _cx: &HookContext) {}

    async fn on_compact(&self, _phase: Phase, _cx: &HookContext) {}

    async fn on_session(&self, _phase: Phase, _cx: &HookContext) {}

    /// Passive observer of the journal.
    async fn on_event(&self, _frame: &Frame, _cx: &HookContext) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The spelling a hook in another process reads and writes.
    #[test]
    fn an_outcome_crosses_tagged_by_its_kind() {
        let denied = HookOutcome::Deny {
            reason: "not here".into(),
        };
        assert_eq!(
            serde_json::to_value(&denied).expect("an outcome serialises"),
            json!({ "kind": "deny", "reason": "not here" })
        );
        assert_eq!(
            serde_json::from_value::<HookOutcome>(json!({ "kind": "continue" }))
                .expect("and parses"),
            HookOutcome::Continue
        );
    }

    /// The law ADR-0032 §4 rests on: there is no `Allow` to say, in any
    /// spelling a caller might try.
    #[test]
    fn no_spelling_of_allow_is_an_outcome() {
        for word in ["allow", "Allow", "approve", "permit"] {
            assert!(
                serde_json::from_value::<HookOutcome>(json!({ "kind": word })).is_err(),
                "{word} parsed as an outcome"
            );
        }
    }

    #[test]
    fn a_matcher_that_wants_everything_says_nothing() {
        let matcher: HookMatcher = serde_json::from_value(json!({})).expect("a matcher");
        assert!(matcher.points.is_empty() && matcher.tool.is_none());
        assert_eq!(
            serde_json::to_value(HookMatcher {
                points: vec![HookPoint::BeforeTool],
                tool: Some("Bash".into()),
            })
            .expect("it serialises"),
            json!({ "points": ["beforeTool"], "tool": "Bash" })
        );
    }
}

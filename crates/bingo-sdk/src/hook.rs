//! Typed lifecycle interceptors. Shell hooks are one plugin implementing this.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use crate::event::{Frame, Item, ToolOutput};
use crate::host::Input;
use crate::ids::{SessionId, TurnId};
use crate::provider::Provider;
use crate::tool::ToolCall;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, Default)]
pub struct HookMatcher {
    pub points: Vec<HookPoint>,
    /// Anchored regex on the tool name for the tool points.
    pub tool: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    /// (memory extraction at turn end). Absent outside a session.
    pub provider: Option<Arc<dyn Provider>>,
    pub model: Option<String>,
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

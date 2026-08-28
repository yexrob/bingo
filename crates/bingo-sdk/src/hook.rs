//! Typed lifecycle interceptors. Shell hooks are one plugin implementing this.

use std::path::PathBuf;

use async_trait::async_trait;

use crate::event::{Frame, Item, ToolOutput};
use crate::host::Input;
use crate::ids::{SessionId, TurnId};
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
#[non_exhaustive]
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

#[derive(Clone, Debug)]
pub struct HookContext {
    pub session: SessionId,
    pub turn: Option<TurnId>,
    pub cwd: PathBuf,
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

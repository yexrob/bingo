//! Doubles for the peer paths (ADR-0010): a tool host that routes deliveries
//! to the mailboxes it knows, and a hook that redirects `@name` lines.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bingo_sdk::*;

use crate::session::Mailbox;

/// A tool host that reaches the sessions it was told about, by mailbox.
#[derive(Default)]
pub struct RoutingHost {
    targets: Mutex<HashMap<SessionId, Mailbox>>,
}

impl RoutingHost {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn route(&self, mailbox: Mailbox) {
        self.targets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(mailbox.id().clone(), mailbox);
    }
}

#[async_trait]
impl Prompter for RoutingHost {
    async fn ask(&self, _: InteractionKind, _: Vec<AnswerSpec>) -> Result<Answer, KernelError> {
        Ok(Answer::Cancel)
    }
}

#[async_trait]
impl ToolHost for RoutingHost {
    fn progress(&self, _: &ItemId, _: String) {}
    async fn record(&self, _: ItemBody) -> Result<ItemId, KernelError> {
        Ok(ItemId::mint())
    }
    async fn spawn_session(&self, _: SessionSpec) -> Result<SessionId, KernelError> {
        Err(KernelError::new(ErrorCode::Internal, "no"))
    }
    fn deliver(
        &self,
        to: &SessionId,
        intent: IntentId,
        input: Input,
        delivery: Delivery,
    ) -> Result<(), KernelError> {
        let targets = self.targets.lock().unwrap_or_else(|e| e.into_inner());
        let target = targets.get(to).ok_or_else(|| {
            KernelError::new(ErrorCode::SessionNotFound, format!("no session {to}"))
        })?;
        target.deliver(intent, input, delivery);
        Ok(())
    }
    fn service_any(&self, _: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        None
    }
}

/// Redirects a line addressed `@<name> …` to one session, stripping the address.
pub struct RedirectHook {
    pub name: String,
    pub to: SessionId,
}

#[async_trait]
impl Hook for RedirectHook {
    fn id(&self) -> &str {
        "redirect"
    }
    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::Submit],
            tool: None,
        }
    }
    async fn on_submit(&self, input: &mut Input, _: &HookContext) -> HookOutcome {
        let Input::Text { text, .. } = input else {
            return HookOutcome::Continue;
        };
        let Some(rest) = text.strip_prefix(&format!("@{} ", self.name)) else {
            return HookOutcome::Continue;
        };
        *text = rest.to_string();
        HookOutcome::Redirect {
            session: self.to.clone(),
        }
    }
}

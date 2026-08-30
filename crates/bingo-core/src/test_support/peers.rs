//! Doubles for the peer paths (ADR-0010, ADR-0011): a host that routes
//! deliveries to the mailboxes it knows, and a hook that redirects `@name`
//! lines.

use std::any::Any;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bingo_sdk::*;
use serde_json::Value;

use crate::session::Mailbox;

/// A host that reaches the sessions it was told about, by mailbox, and
/// nothing else.
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

    pub fn handle(self: &Arc<Self>) -> HostHandle {
        HostHandle(Arc::clone(self) as Arc<dyn HostApi>)
    }

    fn target(&self, id: &SessionId) -> Result<Mailbox, KernelError> {
        self.targets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .ok_or_else(|| KernelError::new(ErrorCode::SessionNotFound, format!("no session {id}")))
    }

    fn only_routes<T>() -> Result<T, KernelError> {
        Err(KernelError::new(
            ErrorCode::Internal,
            "this double only routes deliveries",
        ))
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
}

#[async_trait]
impl HostApi for RoutingHost {
    async fn sessions(&self, _: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(Vec::new())
    }
    async fn open(
        &self,
        _: SessionSelector,
        _: ClientIdentity,
        _: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        Self::only_routes()
    }
    async fn close(&self, _: &SessionId, _: CloseReason) -> Result<(), KernelError> {
        Self::only_routes()
    }
    async fn delete(&self, _: &SessionId) -> Result<(), KernelError> {
        Self::only_routes()
    }
    async fn deliver(
        &self,
        to: &SessionId,
        intent: IntentId,
        input: Input,
        delivery: Delivery,
    ) -> Result<(), KernelError> {
        self.target(to)?.deliver(intent, input, delivery);
        Ok(())
    }
    async fn extend(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        self.target(session)?
            .extend(plugin.to_string(), kind.to_string(), payload);
        Ok(())
    }

    async fn signal(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        self.target(session)?
            .signal(plugin.to_string(), kind.to_string(), payload);
        Ok(())
    }
    async fn catalog(&self, _: CatalogKind) -> Result<Catalog, KernelError> {
        Self::only_routes()
    }
    fn gateway_events(&self) -> GatewayStream {
        Box::pin(futures::stream::empty())
    }
    fn service_any(&self, _: &str) -> Option<Arc<dyn Any + Send + Sync>> {
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

//! What a running tool can reach: its own session by mail, everything else
//! through the host that owns it.

use std::any::Any;
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use bingo_sdk::*;

use super::Host;
use crate::session::Mailbox;

pub(super) struct SessionToolHost {
    pub(super) mailbox: Mailbox,
    pub(super) host: Weak<Host>,
}

impl SessionToolHost {
    fn host(&self) -> Result<Arc<Host>, KernelError> {
        self.host
            .upgrade()
            .ok_or_else(|| KernelError::new(ErrorCode::SessionClosed, "the host is gone"))
    }
}

#[async_trait]
impl Prompter for SessionToolHost {
    async fn ask(
        &self,
        kind: InteractionKind,
        answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        self.mailbox.ask(None, kind, answers).await
    }
}

#[async_trait]
impl ToolHost for SessionToolHost {
    fn progress(&self, item: &ItemId, tail: String) {
        self.mailbox.progress(item.clone(), tail);
    }

    async fn record(&self, body: ItemBody) -> Result<ItemId, KernelError> {
        self.mailbox.record(body).await
    }

    async fn spawn_session(&self, spec: SessionSpec) -> Result<SessionId, KernelError> {
        let host = self.host()?;
        let mailbox = host.create(spec).await?;
        Ok(mailbox.id().clone())
    }

    fn submit(&self, to: &SessionId, intent: IntentId, input: Input) {
        if let Some(host) = self.host.upgrade()
            && let Ok(live) = host.live(to)
        {
            live.mailbox.submit(intent, input);
        }
    }

    fn service_any(&self, key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        self.host.upgrade()?.registry.services.get(key).cloned()
    }
}

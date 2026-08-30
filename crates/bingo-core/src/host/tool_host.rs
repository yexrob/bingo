//! What is a running call's own: its session, by mail (ADR-0011 §3). The
//! host it reaches everything else through is in its context.

use async_trait::async_trait;
use bingo_sdk::*;

use crate::session::Mailbox;

pub(super) struct SessionToolHost {
    pub(super) mailbox: Mailbox,
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
}

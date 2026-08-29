//! Reopening a stored session on this host (ADR-0005): take it, replay it,
//! run it on the plugins that are here now.

use std::sync::Arc;

use bingo_sdk::*;

use super::{Host, Live};
use crate::session::{self, Mailbox};

impl Host {
    /// A session that is not live but is in the store: the newest candidate
    /// nobody holds is taken, replayed and run on this host's plugins.
    pub(super) async fn reopen(&self, selector: SessionSelector) -> Result<Mailbox, KernelError> {
        let not_found = || KernelError::new(ErrorCode::SessionNotFound, "no such session");
        let store = self.registry.store.clone().ok_or_else(not_found)?;
        let mut last = not_found();
        for id in self.stored_candidates(store.as_ref(), selector).await? {
            match store.acquire(&id).await {
                Ok(()) => return self.resume_acquired(store, id).await,
                Err(e) if e.code == ErrorCode::SessionLocked => last = e,
                Err(e) => return Err(e),
            }
        }
        Err(last)
    }

    /// Stored sessions a selector may mean, most recently updated first.
    async fn stored_candidates(
        &self,
        store: &dyn SessionStore,
        selector: SessionSelector,
    ) -> Result<Vec<SessionId>, KernelError> {
        let filter = |cwd: Option<std::path::PathBuf>| SessionFilter {
            cwd,
            ..SessionFilter::default()
        };
        Ok(match selector {
            SessionSelector::Create { .. } => Vec::new(),
            SessionSelector::ById { id } => vec![id],
            SessionSelector::ByKey { key } => store
                .list(&filter(None))
                .await?
                .into_iter()
                .filter(|s| s.key.as_deref() == Some(key.as_str()))
                .map(|s| s.id)
                .collect(),
            SessionSelector::Latest { cwd } => store
                .list(&filter(Some(cwd)))
                .await?
                .into_iter()
                .map(|s| s.id)
                .collect(),
        })
    }

    /// The lock is held; anything that fails from here gives it back.
    async fn resume_acquired(
        &self,
        store: Arc<dyn SessionStore>,
        id: SessionId,
    ) -> Result<Mailbox, KernelError> {
        let resumed = self.resume_frames(store.clone(), &id).await;
        if resumed.is_err() {
            let _ = store.release(&id).await;
        }
        resumed
    }

    async fn resume_frames(
        &self,
        store: Arc<dyn SessionStore>,
        id: &SessionId,
    ) -> Result<Mailbox, KernelError> {
        let frames = store.replay(id, Seq::ZERO).await?;
        let head = session::head_summary(&frames)?;
        let spec = spec_of(&head);
        self.check_key_free(spec.key.as_deref())?;
        let thinking = self.settings.kernel.thinking;
        let choice = self.choose_model(&spec, thinking).await?;
        let mailbox = session::resume(frames, Some(store), self.services(), |mailbox| {
            Arc::new(self.turn_config(&spec, &head, choice, mailbox))
        })?;
        let live = Live::new(mailbox, &head, spec, thinking);
        self.lock().insert(head.id.clone(), live.clone());
        let _ = self.gateway.send(GatewayEvent::SessionCreated {
            summary: Box::new(head),
        });
        Ok(live.mailbox)
    }
}

/// A resumed session runs as it was created: same cwd, key, parent,
/// provider and model; the tools and the policy are the running host's.
fn spec_of(summary: &SessionSummary) -> SessionSpec {
    SessionSpec {
        cwd: std::path::PathBuf::from(&summary.cwd),
        key: summary.key.clone(),
        parent: summary.parent.clone(),
        title: summary.title.clone(),
        provider: summary.provider.clone(),
        model: summary.model.clone(),
        system_extra: None,
        tools: None,
    }
}

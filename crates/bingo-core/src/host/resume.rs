//! Reopening a stored session on this host (ADR-0005): take it, replay it,
//! run it on the plugins that are here now — and say what the store knows of
//! its children that this reopening does not change.

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
            // Roots first: a child under the latest root is newer than it,
            // and is not what `--continue` means.
            SessionSelector::Latest { cwd } => {
                let (roots, children): (Vec<_>, Vec<_>) = store
                    .list(&filter(Some(cwd)))
                    .await?
                    .into_iter()
                    .partition(|s| s.parent.is_none());
                roots.into_iter().chain(children).map(|s| s.id).collect()
            }
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
        let choice = self.model_for(&spec, thinking).await?;
        let mailbox = session::resume(frames, Some(store.clone()), self.services(), |mailbox| {
            Arc::new(self.turn_config(&spec, &head, choice, mailbox))
        })?;
        let live = Live::new(mailbox, &head, spec, thinking);
        self.lock().insert(head.id.clone(), live.clone());
        let _ = self.gateway.send(GatewayEvent::SessionCreated {
            summary: Box::new(head),
        });
        report_lost_turns(store.as_ref(), &live.mailbox).await;
        Ok(live.mailbox)
    }
}

/// The code a client reads a lost child's line by.
const LOST_TURN: &str = "CHILD_TURN_LOST";

/// A child whose stored journal ends inside a turn was at work when the last
/// process ended, and the report it owed its parent will never arrive. The
/// session coming back is told, one line each, before it reads anything new.
///
/// The child is not reopened here and its journal is not rewritten: the turn
/// it left open is closed by the child's own `recover` if and when it comes
/// back, and these words must not promise what has not happened.
async fn report_lost_turns(store: &dyn SessionStore, session: &Mailbox) {
    for child in lost_children(store, session.id()).await {
        let body = ItemBody::Notice {
            level: Level::Warn,
            code: LOST_TURN.to_string(),
            text: lost_line(&child),
        };
        if let Err(error) = session.record(body).await {
            tracing::debug!(%error, child = %child.id, "a lost turn reached no transcript");
        }
    }
}

/// The children whose journal ends mid-turn, in id order, so two resumes say
/// the same thing in the same order. A session no model answers never opened
/// a turn (ADR-0011 §1), so it costs no replay.
async fn lost_children(store: &dyn SessionStore, parent: &SessionId) -> Vec<SessionSummary> {
    let filter = SessionFilter {
        parent: Some(parent.clone()),
        ..SessionFilter::default()
    };
    let Ok(children) = store.list(&filter).await else {
        return Vec::new();
    };
    let mut lost = Vec::new();
    for child in children {
        if child.driver != Driver::Log && mid_turn(store, &child.id).await {
            lost.push(child);
        }
    }
    lost.sort_by(|a, b| a.id.cmp(&b.id));
    lost
}

/// Whether the stored journal ends inside a turn, asked of the fold and of
/// nothing else: there is no second record of a journal's tail.
async fn mid_turn(store: &dyn SessionStore, id: &SessionId) -> bool {
    match store.replay(id, Seq::ZERO).await {
        Ok(frames) => session::replayed(&frames).is_ok_and(|state| state.busy()),
        Err(error) => {
            tracing::debug!(%error, session = %id, "a stored child would not replay");
            false
        }
    }
}

/// A fact and an option, and nothing that did not happen.
fn lost_line(child: &SessionSummary) -> String {
    let name = child.title.clone().unwrap_or_else(|| child.id.to_string());
    format!(
        "{name} ({}) was mid-turn when the last process ended: that turn was lost with it, \
         and the report it owed will not arrive. Wake it to take the work up again.",
        child.id
    )
}

/// A resumed session runs as it was created: same cwd, key, parent,
/// provider and model; the tools and the policy are the running host's.
fn spec_of(summary: &SessionSummary) -> SessionSpec {
    SessionSpec {
        driver: summary.driver,
        cwd: std::path::PathBuf::from(&summary.cwd),
        key: summary.key.clone(),
        parent: summary.parent.clone(),
        title: summary.title.clone(),
        provider: summary.provider.clone(),
        model: summary.model.clone(),
        system_extra: summary.system_extra.clone(),
        tools: summary.tools.clone(),
    }
}

//! A one-session host that folds extension frames with the SDK reducer.

use std::any::Any;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use bingo_sdk::testing::NoHost;
use bingo_sdk::{
    Attachment, Catalog, CatalogKind, ClientIdentity, CloseReason, ContextContributor,
    ContextPiece, Delivery, Event, Frame, GatewayStream, HostApi, HostHandle, Input, IntentId,
    KernelError, OpenOptions, Seq, SessionFilter, SessionHandle, SessionId, SessionSelector,
    SessionState, SessionSummary,
};
use serde_json::Value;

use crate::query::Asked;

struct Inner {
    state: Mutex<SessionState>,
    opening: AtomicBool,
}

#[derive(Clone)]
pub(crate) struct Journal(Arc<Inner>);

impl Journal {
    pub(crate) fn at(cwd: &Path) -> Self {
        Self::holding(SessionState::new(Asked::at(cwd).query().session.clone()))
    }

    pub(crate) fn holding(state: SessionState) -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(state),
            opening: AtomicBool::new(false),
        }))
    }

    pub(crate) fn state(&self) -> MutexGuard<'_, SessionState> {
        self.0.state.lock().expect("the journal lock")
    }

    pub(crate) fn opening(&self, opening: bool) {
        self.0.opening.store(opening, Ordering::SeqCst);
    }

    pub(crate) fn handle(&self) -> HostHandle {
        HostHandle(Arc::new(self.clone()))
    }

    pub(crate) fn apply(&self, event: Event) {
        let mut state = self.state();
        let frame = Frame {
            seq: Seq(state.seq.0 + 1),
            ts: jiff::Timestamp::UNIX_EPOCH,
            session: state.summary.id.clone(),
            cause: None,
            event,
        };
        state.apply(&frame);
    }

    pub(crate) async fn contribute(
        &self,
        contributor: &dyn ContextContributor,
        cwd: &Path,
    ) -> Vec<ContextPiece> {
        let asked = Asked::at(cwd);
        let host = self.handle();
        let mut query = asked.query();
        query.host = &host;
        contributor.contribute(query).await.expect("context")
    }
}

#[async_trait]
impl HostApi for Journal {
    async fn sessions(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        NoHost.sessions(filter).await
    }

    async fn open(
        &self,
        selector: SessionSelector,
        _who: ClientIdentity,
        _options: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        assert!(
            !self.0.opening.load(Ordering::SeqCst),
            "startup must never open itself"
        );
        let snapshot = self.state().clone();
        assert_eq!(
            selector,
            SessionSelector::ById {
                id: snapshot.summary.id.clone()
            }
        );
        Ok(Attachment {
            session: snapshot.summary.id.clone(),
            snapshot,
            events: Box::pin(futures::stream::empty()),
            handle: SessionHandle(Arc::new(Deaf)),
        })
    }

    async fn close(&self, session: &SessionId, reason: CloseReason) -> Result<(), KernelError> {
        NoHost.close(session, reason).await
    }

    async fn delete(&self, session: &SessionId) -> Result<(), KernelError> {
        NoHost.delete(session).await
    }

    async fn deliver(
        &self,
        session: &SessionId,
        intent: IntentId,
        input: Input,
        delivery: Delivery,
    ) -> Result<(), KernelError> {
        NoHost.deliver(session, intent, input, delivery).await
    }

    async fn extend(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        assert_eq!(session, &self.state().summary.id);
        self.apply(Event::Extension {
            plugin: plugin.into(),
            kind: kind.into(),
            payload,
        });
        Ok(())
    }

    async fn signal(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        NoHost.signal(session, plugin, kind, payload).await
    }

    async fn catalog(&self, kind: CatalogKind) -> Result<Catalog, KernelError> {
        NoHost.catalog(kind).await
    }

    fn gateway_events(&self) -> GatewayStream {
        NoHost.gateway_events()
    }

    fn service_any(&self, _: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

struct Deaf;

#[async_trait]
impl bingo_sdk::SessionPort for Deaf {
    fn submit(&self, _: IntentId, _: Input) {
        unreachable!("a contributor submits nothing")
    }

    fn interrupt(&self, _: IntentId, _: bingo_sdk::InterruptScope) {
        unreachable!("a contributor interrupts nothing")
    }

    fn answer(
        &self,
        _: IntentId,
        _: bingo_sdk::InteractionId,
        _: bingo_sdk::Answer,
        _: bingo_sdk::Activation,
    ) {
        unreachable!("a contributor answers nothing")
    }

    async fn history(
        &self,
        _: bingo_sdk::HistoryPage,
    ) -> Result<bingo_sdk::HistoryChunk, KernelError> {
        unreachable!("a contributor pages no history")
    }

    async fn events_since(&self, _: Seq) -> Result<bingo_sdk::FrameStream, KernelError> {
        unreachable!("a contributor subscribes to no events")
    }
}

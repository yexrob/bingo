//! The kernel this surface is driven against.
//!
//! A `HostApi` and a `SessionPort` that publish frames on demand and keep
//! what was submitted, so a chat's two directions can both be asserted
//! without a kernel in the way.

use std::any::Any;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bingo_sdk::{
    Activation, Answer, Attachment, Catalog, CatalogEntry, CatalogKind, ClientIdentity,
    CloseReason, Delivery, Env, ErrorCode, Event, Frame, FrameStream, GatewayStream, HistoryChunk,
    HistoryPage, HostApi, HostHandle, Input, IntentId, InteractionId, InterruptScope, KernelError,
    OpenOptions, Seq, SessionFilter, SessionHandle, SessionId, SessionPort, SessionSelector,
    SessionSpec, SessionState, SessionSummary, SurfaceOptions,
};
use tokio::sync::mpsc;

use crate::fixtures;

fn locked<T>(slot: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// One session: every attachment gets its own stream, so a chat and a TUI can
/// both be looking at it, which is what the two-surface race needs.
#[derive(Debug, Default)]
pub struct TestSession {
    key: String,
    seq: AtomicU64,
    watchers: Mutex<Vec<mpsc::UnboundedSender<Frame>>>,
    submitted: Mutex<Vec<Input>>,
    answers: Mutex<Vec<(InteractionId, Answer, Activation)>>,
}

impl TestSession {
    fn attach(&self) -> FrameStream {
        let (publisher, frames) = mpsc::unbounded_channel();
        locked(&self.watchers).push(publisher);
        Box::pin(futures::stream::unfold(frames, |mut frames| async move {
            frames.recv().await.map(|frame| (frame, frames))
        }))
    }

    /// Publish a frame as the kernel would, numbering it as it goes.
    pub fn publish(&self, event: Event) {
        let seq = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        let frame = fixtures::frame(seq, event);
        locked(&self.watchers).retain(|watcher| watcher.send(frame.clone()).is_ok());
    }

    pub fn prompts(&self) -> Vec<String> {
        locked(&self.submitted)
            .iter()
            .filter_map(|input| match input {
                Input::Text { text, .. } => Some(text.clone()),
                Input::Action { .. } => None,
            })
            .collect()
    }

    pub fn origins(&self) -> Vec<bingo_sdk::Origin> {
        locked(&self.submitted)
            .iter()
            .filter_map(|input| match input {
                Input::Text { origin, .. } => Some(origin.clone()),
                Input::Action { .. } => None,
            })
            .collect()
    }

    /// The pictures beside each prompt, in the order they were submitted.
    pub fn pictures(&self) -> Vec<Vec<bingo_sdk::Image>> {
        locked(&self.submitted)
            .iter()
            .filter_map(|input| match input {
                Input::Text { images, .. } => Some(images.clone()),
                Input::Action { .. } => None,
            })
            .collect()
    }

    pub fn answers(&self) -> Vec<(InteractionId, Answer, Activation)> {
        locked(&self.answers).clone()
    }
}

#[async_trait]
impl SessionPort for TestSession {
    fn submit(&self, _intent: IntentId, input: Input) {
        locked(&self.submitted).push(input);
    }

    fn interrupt(&self, _intent: IntentId, _scope: InterruptScope) {}

    fn answer(
        &self,
        _intent: IntentId,
        interaction: InteractionId,
        answer: Answer,
        activation: Activation,
    ) {
        locked(&self.answers).push((interaction, answer, activation));
    }

    async fn history(&self, _page: HistoryPage) -> Result<HistoryChunk, KernelError> {
        Ok(HistoryChunk {
            items: Vec::new(),
            next: None,
            generation: 0,
        })
    }

    async fn events_since(&self, _since: Seq) -> Result<FrameStream, KernelError> {
        Ok(self.attach())
    }
}

#[derive(Debug, Default)]
pub struct TestHost {
    sessions: Mutex<Vec<Arc<TestSession>>>,
    /// Every selector `open` was called with, in order.
    opened: Mutex<Vec<SessionSelector>>,
}

impl TestHost {
    pub fn session(&self, key: &str) -> Option<Arc<TestSession>> {
        locked(&self.sessions)
            .iter()
            .find(|session| session.key == key)
            .cloned()
    }

    pub fn keys(&self) -> Vec<String> {
        locked(&self.sessions)
            .iter()
            .map(|session| session.key.clone())
            .collect()
    }

    pub fn opened(&self) -> Vec<SessionSelector> {
        locked(&self.opened).clone()
    }

    fn attachment(&self, session: Arc<TestSession>) -> Attachment {
        let mut summary = fixtures::summary();
        summary.key = Some(session.key.clone());
        Attachment {
            session: SessionId::from_raw(fixtures::SESSION),
            snapshot: SessionState::new(summary),
            events: session.attach(),
            handle: SessionHandle(session as Arc<dyn SessionPort>),
        }
    }
}

#[async_trait]
impl HostApi for TestHost {
    async fn sessions(&self, _filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(Vec::new())
    }

    async fn open(
        &self,
        selector: SessionSelector,
        _who: ClientIdentity,
        options: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        assert!(options.children, "a chat attaches to the whole tree");
        locked(&self.opened).push(selector.clone());
        match selector {
            SessionSelector::ByKey { key } => self
                .session(&key)
                .map(|session| self.attachment(session))
                .ok_or_else(|| KernelError::new(ErrorCode::SessionNotFound, "no such session")),
            SessionSelector::Create {
                spec: SessionSpec { key: Some(key), .. },
            } => {
                let session = Arc::new(TestSession {
                    key,
                    ..TestSession::default()
                });
                locked(&self.sessions).push(Arc::clone(&session));
                Ok(self.attachment(session))
            }
            other => panic!("a chat never opens by {other:?}"),
        }
    }

    async fn close(&self, _session: &SessionId, _reason: CloseReason) -> Result<(), KernelError> {
        Ok(())
    }

    async fn delete(&self, _session: &SessionId) -> Result<(), KernelError> {
        Ok(())
    }

    async fn deliver(
        &self,
        _to: &SessionId,
        _intent: IntentId,
        _input: Input,
        _delivery: Delivery,
    ) -> Result<(), KernelError> {
        unreachable!("this double delivers nothing")
    }

    async fn extend(
        &self,
        _session: &SessionId,
        _plugin: &str,
        _kind: &str,
        _payload: serde_json::Value,
    ) -> Result<(), KernelError> {
        unreachable!("this double extends nothing")
    }

    async fn signal(
        &self,
        _session: &SessionId,
        _plugin: &str,
        _kind: &str,
        _payload: serde_json::Value,
    ) -> Result<(), KernelError> {
        unreachable!("this double signals nothing")
    }

    async fn catalog(&self, kind: CatalogKind) -> Result<Catalog, KernelError> {
        Ok(Catalog {
            kind,
            entries: Vec::<CatalogEntry>::new(),
        })
    }

    fn gateway_events(&self) -> GatewayStream {
        Box::pin(futures::stream::empty())
    }

    fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

/// A host nothing is ever asked of, for the tests that never get that far.
pub fn nowhere() -> HostHandle {
    HostHandle(Arc::new(TestHost::default()))
}

pub fn options(cwd: &str) -> SurfaceOptions {
    SurfaceOptions {
        cwd: cwd.into(),
        // The channel surface mints its own keys; the selector is a
        // placeholder it never reads.
        selector: SessionSelector::Latest { cwd: cwd.into() },
        prompt: None,
        args: serde_json::Value::Null,
        env: Arc::new(Env::rooted(cwd)),
    }
}

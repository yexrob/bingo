//! The scripted kernel the wire tests run against: a fixed turn of frames, a
//! `SessionPort` that records every write, and a `HostApi` that can refuse.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::any::Any;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use bingo_sdk::{
    Activation, Answer, Attachment, Catalog, CatalogEntry, CatalogKind, ClientIdentity,
    CloseReason, Event, Frame, FrameStream, GatewayEvent, GatewayStream, HistoryChunk, HistoryPage,
    HostApi, HostHandle, Input, IntentId, InteractionId, InterruptScope, Item, ItemBody, ItemId,
    ItemStatus, KernelError, Seq, SessionFilter, SessionHandle, SessionId, SessionPort,
    SessionSelector, SessionState, SessionSummary, TurnId, TurnOrigin, TurnStatus, Usage,
};
use jiff::Timestamp;
use serde_json::Value;

pub fn ts() -> Timestamp {
    Timestamp::from_second(1_700_000_000).expect("a fixed instant")
}

pub fn session_id() -> SessionId {
    SessionId::from_raw("ses_1")
}

pub fn summary() -> SessionSummary {
    SessionSummary {
        id: session_id(),
        key: None,
        title: None,
        cwd: "/tmp".into(),
        parent: None,
        model: Some("fake-1".into()),
        provider: Some("fake".into()),
        created_at: ts(),
        updated_at: ts(),
        usage: Usage::default(),
        busy: false,
    }
}

pub fn frame(seq: u64, event: Event) -> Frame {
    Frame {
        seq: Seq(seq),
        ts: ts(),
        session: session_id(),
        cause: None,
        event,
    }
}

/// A whole turn, with a `Lagged` marker in the middle to prove it travels like
/// any other frame.
pub fn script() -> Vec<Frame> {
    let turn = TurnId::from_raw("trn_1");
    vec![
        frame(
            1,
            Event::TurnStarted {
                turn: turn.clone(),
                inputs: Vec::new(),
                origin: TurnOrigin::Submit,
            },
        ),
        frame(
            2,
            Event::ItemCompleted {
                item: Item {
                    id: ItemId::from_raw("itm_1"),
                    turn: Some(turn.clone()),
                    round: 0,
                    status: ItemStatus::Completed,
                    started_at: ts(),
                    completed_at: Some(ts()),
                    intent: None,
                    body: ItemBody::Assistant {
                        text: "hello".into(),
                    },
                    meta: serde_json::Map::new(),
                },
            },
        ),
        frame(
            3,
            Event::Lagged {
                from: Seq(2),
                to: Seq(3),
            },
        ),
        frame(
            4,
            Event::TurnCompleted {
                turn,
                status: TurnStatus::Completed,
                usage: Usage::default(),
            },
        ),
    ]
}

pub fn last_seq() -> Seq {
    Seq(script().len() as u64)
}

/// The snapshot every `open` answers with, before any frame is applied.
pub fn fresh_state() -> SessionState {
    SessionState::new(summary())
}

#[derive(Default)]
pub struct TestSession {
    pub frames: Vec<Frame>,
    pub submits: Mutex<Vec<(IntentId, Input)>>,
    pub interrupts: Mutex<Vec<(IntentId, InterruptScope)>>,
    pub answers: Mutex<Vec<(IntentId, InteractionId, Answer, Activation)>>,
    pub pages: Mutex<Vec<HistoryPage>>,
}

impl TestSession {
    pub fn stream(&self, since: Seq, durable_only: bool) -> FrameStream {
        let frames: Vec<Frame> = self
            .frames
            .iter()
            .filter(|frame| frame.seq > since && (!durable_only || frame.event.is_durable()))
            .cloned()
            .collect();
        Box::pin(futures::stream::iter(frames))
    }

    pub fn submits(&self) -> MutexGuard<'_, Vec<(IntentId, Input)>> {
        self.submits.lock().expect("the recorder is not poisoned")
    }
}

#[async_trait]
impl SessionPort for TestSession {
    fn submit(&self, intent: IntentId, input: Input) {
        self.submits().push((intent, input));
    }

    fn interrupt(&self, intent: IntentId, scope: InterruptScope) {
        self.interrupts
            .lock()
            .expect("the recorder is not poisoned")
            .push((intent, scope));
    }

    fn answer(
        &self,
        intent: IntentId,
        interaction: InteractionId,
        answer: Answer,
        activation: Activation,
    ) {
        self.answers
            .lock()
            .expect("the recorder is not poisoned")
            .push((intent, interaction, answer, activation));
    }

    async fn history(&self, page: HistoryPage) -> Result<HistoryChunk, KernelError> {
        self.pages
            .lock()
            .expect("the recorder is not poisoned")
            .push(page);
        Ok(HistoryChunk {
            items: Vec::new(),
            next: None,
            generation: 3,
        })
    }

    /// The journal replay: durable frames only, as the kernel's is.
    async fn events_since(&self, since: Seq) -> Result<FrameStream, KernelError> {
        Ok(self.stream(since, true))
    }
}

pub struct TestHost {
    session: Arc<TestSession>,
    /// What `session/list` answers with when the kernel is unhappy.
    refuse: Option<KernelError>,
}

impl TestHost {
    pub fn with(frames: Vec<Frame>) -> (HostHandle, Arc<TestSession>) {
        TestHost::build(frames, None)
    }

    pub fn refusing(error: KernelError) -> HostHandle {
        TestHost::build(Vec::new(), Some(error)).0
    }

    fn build(frames: Vec<Frame>, refuse: Option<KernelError>) -> (HostHandle, Arc<TestSession>) {
        let session = Arc::new(TestSession {
            frames,
            ..Default::default()
        });
        let host = TestHost {
            session: Arc::clone(&session),
            refuse,
        };
        (HostHandle(Arc::new(host)), session)
    }
}

#[async_trait]
impl HostApi for TestHost {
    async fn sessions(&self, _filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        match &self.refuse {
            Some(error) => Err(error.clone()),
            None => Ok(vec![summary()]),
        }
    }

    async fn open(
        &self,
        _selector: SessionSelector,
        _who: ClientIdentity,
    ) -> Result<Attachment, KernelError> {
        Ok(Attachment {
            session: session_id(),
            snapshot: fresh_state(),
            events: self.session.stream(Seq::ZERO, false),
            handle: SessionHandle(Arc::clone(&self.session) as Arc<dyn SessionPort>),
        })
    }

    async fn close(&self, _session: &SessionId, _reason: CloseReason) -> Result<(), KernelError> {
        Ok(())
    }

    async fn delete(&self, _session: &SessionId) -> Result<(), KernelError> {
        Ok(())
    }

    async fn catalog(&self, kind: CatalogKind) -> Result<Catalog, KernelError> {
        Ok(Catalog {
            kind,
            entries: vec![CatalogEntry {
                id: "fake".into(),
                label: "the fake provider".into(),
                meta: Value::Null,
            }],
        })
    }

    fn gateway_events(&self) -> GatewayStream {
        Box::pin(futures::stream::iter([GatewayEvent::CatalogChanged {
            kind: CatalogKind::Tools,
        }]))
    }

    fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

pub fn who() -> ClientIdentity {
    ClientIdentity {
        name: "test".into(),
        surface: "test".into(),
    }
}

pub fn selector() -> SessionSelector {
    SessionSelector::ById { id: session_id() }
}

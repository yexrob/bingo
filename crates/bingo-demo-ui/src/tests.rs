//! What every test in this crate needs: a host whose sessions are their
//! journals — `open` hands back the state folded so far and `extend` folds
//! one more frame into it with the kernel's own reducer — plus the list of
//! what was signalled, which the reducer coalesces away on purpose.

use std::any::Any;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use bingo_sdk::{
    Activation, Answer, AnswerSpec, Attachment, CancellationToken, Catalog, CatalogKind,
    ClientIdentity, CloseReason, CommandContext, Delivery, Env, ErrorCode, Event, Frame,
    FrameStream, GatewayStream, HistoryChunk, HistoryPage, HostApi, HostHandle, Input, IntentId,
    InteractionId, InteractionKind, InterruptScope, ItemBody, ItemId, KernelError, OpenOptions,
    Prompter, Seq, SessionFilter, SessionHandle, SessionId, SessionPort, SessionSelector,
    SessionState, SessionSummary, ToolContext, ToolHost, TurnId, Usage,
};
use jiff::Timestamp;
use serde_json::Value;

#[derive(Default)]
struct Inner {
    sessions: Mutex<Vec<SessionState>>,
    /// Every signal in the order it was published: the reducer keeps only the
    /// latest per kind, which is the point, so a test that watches a bar move
    /// has to be told each frame as it goes.
    signals: Mutex<Vec<(String, Value)>>,
    /// The kernel's frames arrive in one order; the reducer refuses a frame
    /// it has already seen, so a double must number them as it does.
    seq: Mutex<u64>,
}

/// A host that keeps one folded state per session and nothing else.
#[derive(Clone, Default)]
pub(crate) struct Journals(Arc<Inner>);

impl Journals {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn handle(&self) -> HostHandle {
        HostHandle(Arc::new(self.clone()))
    }

    /// A session with an empty journal, as a surface opens one.
    pub(crate) fn session(&self) -> SessionId {
        let id = SessionId::mint();
        self.states().push(SessionState::new(summary(&id)));
        id
    }

    /// Every signal published, kind and payload, in order.
    pub(crate) fn signals(&self) -> Vec<(String, Value)> {
        self.0
            .signals
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }

    /// What the journals hold, which a signal never reaches.
    pub(crate) fn extensions(&self) -> Vec<String> {
        self.states()
            .iter()
            .flat_map(|state| state.extensions.keys().cloned().collect::<Vec<_>>())
            .collect()
    }

    fn states(&self) -> MutexGuard<'_, Vec<SessionState>> {
        self.0
            .sessions
            .lock()
            .unwrap_or_else(|held| held.into_inner())
    }

    fn next_seq(&self) -> Seq {
        let mut seq = self.0.seq.lock().unwrap_or_else(|held| held.into_inner());
        *seq += 1;
        Seq(*seq)
    }

    fn fold(&self, session: &SessionId, event: Event) -> Result<(), KernelError> {
        let frame = Frame {
            seq: self.next_seq(),
            ts: Timestamp::UNIX_EPOCH,
            session: session.clone(),
            cause: None,
            event,
        };
        let mut states = self.states();
        let state = states
            .iter_mut()
            .find(|state| &state.summary.id == session)
            .ok_or_else(|| KernelError::new(ErrorCode::SessionNotFound, "no such session"))?;
        state.apply(&frame);
        Ok(())
    }
}

#[async_trait]
impl HostApi for Journals {
    async fn sessions(&self, _filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        unreachable!("this plugin reads no session list")
    }

    /// The session's own state, cut as an attachment: what a command reads.
    async fn open(
        &self,
        selector: SessionSelector,
        _who: ClientIdentity,
        _options: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        let SessionSelector::ById { id } = selector else {
            unreachable!("this plugin opens a session by id")
        };
        let states = self.states();
        let snapshot = states
            .iter()
            .find(|state| state.summary.id == id)
            .ok_or_else(|| KernelError::new(ErrorCode::SessionNotFound, "no such session"))?
            .clone();
        Ok(Attachment {
            session: id,
            snapshot,
            events: Box::pin(futures::stream::empty()),
            handle: SessionHandle(Arc::new(Deaf)),
        })
    }

    async fn close(&self, _session: &SessionId, _reason: CloseReason) -> Result<(), KernelError> {
        unreachable!("this plugin closes no session")
    }

    async fn delete(&self, _session: &SessionId) -> Result<(), KernelError> {
        unreachable!("this plugin deletes no session")
    }

    async fn deliver(
        &self,
        _to: &SessionId,
        _intent: IntentId,
        _input: Input,
        _delivery: Delivery,
    ) -> Result<(), KernelError> {
        unreachable!("this plugin delivers nothing")
    }

    /// A durable frame into the session's journal, folded as the kernel folds it.
    async fn extend(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        self.fold(
            session,
            Event::Extension {
                plugin: plugin.to_string(),
                kind: kind.to_string(),
                payload,
            },
        )
    }

    /// An ephemeral frame: folded into `signals`, and written down here so a
    /// test can watch every frame the reducer coalesced.
    async fn signal(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        self.0
            .signals
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .push((kind.to_string(), payload.clone()));
        self.fold(
            session,
            Event::Signal {
                plugin: plugin.to_string(),
                kind: kind.to_string(),
                payload,
            },
        )
    }

    async fn catalog(&self, _kind: CatalogKind) -> Result<Catalog, KernelError> {
        unreachable!("this plugin reads no catalog")
    }

    fn gateway_events(&self) -> GatewayStream {
        unreachable!("this plugin watches no gateway")
    }

    fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

/// A handle nothing is written to: this plugin reaches a session through
/// `open`, `extend` and `signal`, never through a client port.
struct Deaf;

#[async_trait]
impl SessionPort for Deaf {
    fn submit(&self, _intent: IntentId, _input: Input) {
        unreachable!("this plugin submits nothing")
    }

    fn interrupt(&self, _intent: IntentId, _scope: InterruptScope) {
        unreachable!("this plugin interrupts nothing")
    }

    fn answer(
        &self,
        _intent: IntentId,
        _interaction: InteractionId,
        _answer: Answer,
        _activation: Activation,
    ) {
        unreachable!("this plugin answers nothing")
    }

    async fn history(&self, _page: HistoryPage) -> Result<HistoryChunk, KernelError> {
        unreachable!("this plugin pages no history")
    }

    async fn events_since(&self, _since: Seq) -> Result<FrameStream, KernelError> {
        unreachable!("this plugin re-subscribes to nothing")
    }
}

/// What a tool is handed as its call's own; this one asks nobody anything and
/// records nothing outside its own result.
struct Silent;

#[async_trait]
impl Prompter for Silent {
    async fn ask(
        &self,
        _kind: InteractionKind,
        _answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        unreachable!("this tool asks nobody anything")
    }
}

#[async_trait]
impl ToolHost for Silent {
    fn progress(&self, _item: &ItemId, _tail: String) {}

    async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
        unreachable!("this tool records nothing of its own")
    }
}

pub(crate) fn tool_context(session: &SessionId, journals: &Journals) -> ToolContext {
    ToolContext {
        call_id: "call_test".into(),
        session: session.clone(),
        turn: TurnId::from_raw("trn_test"),
        item: ItemId::from_raw("itm_test"),
        cwd: PathBuf::from("/work/project"),
        cancel: CancellationToken::new(),
        env: Arc::new(Env::rooted("/nowhere")),
        host: journals.handle(),
        call: Arc::new(Silent),
    }
}

pub(crate) fn command_context(session: &SessionId, journals: &Journals) -> CommandContext {
    CommandContext {
        session: session.clone(),
        cwd: PathBuf::from("/work/project"),
        host: journals.handle(),
    }
}

fn summary(id: &SessionId) -> SessionSummary {
    SessionSummary {
        tools: None,
        system_extra: None,
        id: id.clone(),
        key: None,
        title: None,
        cwd: "/work/project".into(),
        parent: None,
        driver: Default::default(),
        model: None,
        provider: None,
        created_at: Timestamp::UNIX_EPOCH,
        updated_at: Timestamp::UNIX_EPOCH,
        usage: Usage::default(),
        busy: false,
    }
}

//! What every test in this crate needs: a tree of definition files, a host
//! with a fleet of sessions to resolve names against, a tool host that
//! records what a tool asked it to do, and the three contexts the sdk hands a
//! tool, a command and a hook.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use bingo_sdk::{
    Activation, Answer, AnswerSpec, Attachment, CancellationToken, Catalog, CatalogEntry,
    CatalogKind, ClientIdentity, CloseReason, CommandContext, Delivery, Env, ErrorCode, Event,
    Frame, FrameStream, GatewayStream, HistoryChunk, HistoryPage, HookContext, HostApi, HostHandle,
    Input, IntentId, InteractionId, InteractionKind, InterruptScope, Item, ItemBody, ItemId,
    ItemStatus, KernelError, OpenOptions, ParentLink, Prompter, Seq, SessionFilter, SessionHandle,
    SessionId, SessionPort, SessionSelector, SessionSpec, SessionState, SessionSummary,
    ToolContext, ToolHost, TurnId, TurnOrigin, TurnStatus, Usage,
};
use futures::StreamExt;
use jiff::Timestamp;

use crate::handle::LateHost;

/// The turn every scripted frame belongs to.
const TURN: &str = "trn_1";

/// Where the scripted frames start, above anything the snapshot folded.
const SCRIPT_SEQ: u64 = 10;

fn ts() -> Timestamp {
    Timestamp::UNIX_EPOCH
}

/// A machine with agent definitions on it: a home holding the person's own
/// layer, and a working directory to run in.
pub(crate) struct Tree(tempfile::TempDir);

impl Tree {
    pub(crate) fn new() -> Self {
        Self(tempfile::tempdir().expect("a temporary home"))
    }

    pub(crate) fn root(&self) -> PathBuf {
        self.0.path().to_path_buf()
    }

    pub(crate) fn cwd(&self) -> PathBuf {
        self.dir("work")
    }

    pub(crate) fn dir(&self, relative: &str) -> PathBuf {
        let path = self.root().join(relative);
        std::fs::create_dir_all(&path).expect("a directory");
        path
    }

    pub(crate) fn write(&self, path: &Path, source: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a directory");
        }
        std::fs::write(path, source).expect("a file");
    }

    /// The person's own layer, `<home>/.bingo/agents`.
    pub(crate) fn user_layer(&self) -> PathBuf {
        self.root().join(".bingo").join("agents")
    }

    pub(crate) fn user_agent(&self, name: &str, source: &str) {
        self.write(&self.user_layer().join(format!("{name}.md")), source);
    }

    /// A definition in the project layer of `<home>/<at>`, returning that
    /// working directory.
    pub(crate) fn project_agent(&self, at: &str, name: &str, source: &str) -> PathBuf {
        let cwd = self.dir(at);
        self.write(
            &cwd.join(".bingo").join("agents").join(format!("{name}.md")),
            source,
        );
        cwd
    }
}

/// One session the fleet knows about: what it looks like, and what its
/// journal already held when a client attached.
struct Live {
    summary: SessionSummary,
    history: Vec<Event>,
}

#[derive(Default)]
struct Inner {
    sessions: Mutex<Vec<Live>>,
    /// What every attachment yields after its snapshot, and whether the
    /// stream then ends rather than staying open.
    script: Mutex<(Vec<Event>, bool)>,
}

/// A host whose sessions are written down rather than run.
#[derive(Clone, Default)]
pub(crate) struct Fleet(Arc<Inner>);

impl Fleet {
    fn sessions(&self) -> MutexGuard<'_, Vec<Live>> {
        self.0
            .sessions
            .lock()
            .unwrap_or_else(|held| held.into_inner())
    }

    pub(crate) fn handle(&self) -> HostHandle {
        HostHandle(Arc::new(self.clone()))
    }

    /// The plugin's shared host, already started.
    pub(crate) fn late(&self) -> Arc<LateHost> {
        let late = Arc::new(LateHost::default());
        late.set(self.handle());
        late
    }

    /// A session with no parent, as a surface opens one.
    pub(crate) fn root(&self) -> SessionId {
        let id = SessionId::mint();
        self.add(summary(id.as_str(), None, None));
        id
    }

    /// A child of `parent`, titled `name`, as `SpawnAgent` mints one.
    pub(crate) fn child(&self, parent: &SessionId, name: &str) -> SessionId {
        let id = SessionId::mint();
        self.add(summary(id.as_str(), Some(name), Some(parent.clone())));
        id
    }

    fn add(&self, summary: SessionSummary) {
        self.sessions().push(Live {
            summary,
            history: Vec::new(),
        });
    }

    /// A session in the middle of a turn: what a `SpawnAgent` call watches.
    pub(crate) fn set_busy(&self, session: &SessionId, busy: bool) {
        let mut sessions = self.sessions();
        let Some(live) = sessions.iter_mut().find(|l| &l.summary.id == session) else {
            return;
        };
        live.summary.busy = busy;
        if busy {
            live.history.push(turn_started());
        }
    }

    /// A session that has already answered, and is idle.
    pub(crate) fn said(&self, session: &SessionId, text: &str) {
        self.remember(session, [assistant(text), turn_completed()]);
    }

    /// A session whose last turn failed before it said anything.
    pub(crate) fn failed(&self, session: &SessionId, message: &str) {
        self.remember(session, [turn_started(), turn_failed(message)]);
    }

    fn remember(&self, session: &SessionId, events: impl IntoIterator<Item = Event>) {
        let mut sessions = self.sessions();
        let Some(live) = sessions.iter_mut().find(|l| &l.summary.id == session) else {
            return;
        };
        live.history.extend(events);
    }

    /// What every attachment yields after its snapshot. The stream then stays
    /// open, as a live session's does.
    pub(crate) fn script(&self, events: impl IntoIterator<Item = Event>) {
        *self.script_slot() = (events.into_iter().collect(), false);
    }

    /// The same, for a session that closes once it has said it.
    pub(crate) fn script_ending(&self, events: impl IntoIterator<Item = Event>) {
        *self.script_slot() = (events.into_iter().collect(), true);
    }

    fn script_slot(&self) -> MutexGuard<'_, (Vec<Event>, bool)> {
        self.0
            .script
            .lock()
            .unwrap_or_else(|held| held.into_inner())
    }

    /// The session a `SessionSpec` describes, joined to the fleet.
    fn spawn(&self, spec: &SessionSpec) -> SessionId {
        let id = SessionId::mint();
        let mut summary = summary(
            id.as_str(),
            spec.title.as_deref(),
            spec.parent.as_ref().map(|p| p.session.clone()),
        );
        summary.key = spec.key.clone();
        summary.cwd = spec.cwd.display().to_string();
        self.add(summary);
        id
    }

    fn snapshot(&self, live: &Live) -> SessionState {
        let mut state = SessionState::new(live.summary.clone());
        for (n, event) in live.history.iter().enumerate() {
            state.apply(&stamped(n as u64 + 1, event.clone(), &live.summary.id));
        }
        state
    }

    fn frames(&self, session: &SessionId) -> FrameStream {
        let (events, ends) = self.script_slot().clone();
        let frames: Vec<Frame> = events
            .into_iter()
            .enumerate()
            .map(|(n, event)| stamped(SCRIPT_SEQ + n as u64, event, session))
            .collect();
        let scripted = futures::stream::iter(frames);
        match ends {
            true => Box::pin(scripted),
            // A live session's stream does not end when it stops talking.
            false => Box::pin(scripted.chain(futures::stream::pending())),
        }
    }
}

#[async_trait]
impl HostApi for Fleet {
    async fn sessions(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(self
            .sessions()
            .iter()
            .map(|live| live.summary.clone())
            .filter(|summary| match &filter.parent {
                Some(parent) => summary.parent.as_ref().map(|p| &p.session) == Some(parent),
                None => true,
            })
            .collect())
    }

    async fn open(
        &self,
        selector: SessionSelector,
        _who: ClientIdentity,
        _options: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        let SessionSelector::ById { id } = selector else {
            unreachable!("this plugin opens a child by id and nothing else")
        };
        let sessions = self.sessions();
        let live = sessions
            .iter()
            .find(|live| live.summary.id == id)
            .ok_or_else(|| KernelError::new(ErrorCode::SessionNotFound, "no such session"))?;
        let snapshot = self.snapshot(live);
        drop(sessions);
        Ok(Attachment {
            session: id.clone(),
            snapshot,
            events: self.frames(&id),
            handle: SessionHandle(Arc::new(Deaf)),
        })
    }

    async fn close(&self, _session: &SessionId, _reason: CloseReason) -> Result<(), KernelError> {
        unreachable!("this plugin closes no session")
    }

    async fn delete(&self, _session: &SessionId) -> Result<(), KernelError> {
        unreachable!("this plugin deletes no session")
    }

    /// Three tools, one of them the one a child may never have.
    async fn catalog(&self, kind: CatalogKind) -> Result<Catalog, KernelError> {
        Ok(Catalog {
            kind,
            entries: ["Read", "Write", "AskUserQuestion", "SpawnAgent"]
                .map(str::to_string)
                .into_iter()
                .map(|name| CatalogEntry {
                    id: name.clone(),
                    label: name,
                    meta: serde_json::Value::Null,
                })
                .collect(),
        })
    }

    fn gateway_events(&self) -> GatewayStream {
        Box::pin(futures::stream::empty())
    }

    fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

/// A handle nothing is written to: this plugin talks to a child through
/// `deliver`, never through a client port.
struct Deaf;

#[async_trait]
impl SessionPort for Deaf {
    fn submit(&self, _intent: IntentId, _input: Input) {
        unreachable!("an agent is never submitted to as a client")
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

/// The tool host a tool is handed: it records the sessions a tool started and
/// the messages it sent, and the fleet grows with them.
pub(crate) struct Recorder {
    fleet: Fleet,
    spawned: Mutex<Vec<SessionSpec>>,
    delivered: Mutex<Vec<(SessionId, Input, Delivery)>>,
    locked: Mutex<Vec<String>>,
}

impl Recorder {
    pub(crate) fn new(fleet: &Fleet) -> Arc<Recorder> {
        Arc::new(Recorder {
            fleet: fleet.clone(),
            spawned: Mutex::new(Vec::new()),
            delivered: Mutex::new(Vec::new()),
            locked: Mutex::new(Vec::new()),
        })
    }

    /// A key another session already holds, as the kernel reports it.
    pub(crate) fn lock(&self, key: &str) {
        self.locked
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .push(key.to_string());
    }

    pub(crate) fn spawned(&self) -> Vec<SessionSpec> {
        self.spawned
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }

    pub(crate) fn delivered(&self) -> Vec<(SessionId, Input, Delivery)> {
        self.delivered
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }
}

#[async_trait]
impl Prompter for Recorder {
    async fn ask(
        &self,
        _kind: InteractionKind,
        _answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        unreachable!("an agents tool asks nobody anything")
    }
}

#[async_trait]
impl ToolHost for Recorder {
    fn progress(&self, _item: &ItemId, _tail: String) {}

    async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
        unreachable!("an agents tool records nothing of its own")
    }

    async fn spawn_session(&self, spec: SessionSpec) -> Result<SessionId, KernelError> {
        self.spawned
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .push(spec.clone());
        let held = self
            .locked
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .iter()
            .any(|key| Some(key.as_str()) == spec.key.as_deref());
        if held {
            return Err(KernelError::new(
                ErrorCode::SessionLocked,
                "session key is in use",
            ));
        }
        Ok(self.fleet.spawn(&spec))
    }

    fn deliver(
        &self,
        to: &SessionId,
        _intent: IntentId,
        input: Input,
        delivery: Delivery,
    ) -> Result<(), KernelError> {
        self.delivered
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .push((to.clone(), input, delivery));
        Ok(())
    }

    fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

pub(crate) fn tool_context(session: &SessionId, host: Arc<Recorder>) -> ToolContext {
    ToolContext {
        call_id: "call_test".into(),
        session: session.clone(),
        turn: TurnId::from_raw("trn_call"),
        item: ItemId::from_raw("itm_call"),
        cwd: PathBuf::from("/work/project"),
        cancel: CancellationToken::new(),
        env: Arc::new(Env::rooted("/nowhere")),
        host,
    }
}

pub(crate) fn command_context(session: &SessionId, fleet: &Fleet) -> CommandContext {
    CommandContext {
        session: session.clone(),
        cwd: PathBuf::from("/work/project"),
        host: fleet.handle(),
    }
}

pub(crate) fn hook_context(session: &SessionId) -> HookContext {
    HookContext {
        session: session.clone(),
        turn: None,
        cwd: PathBuf::from("/work/project"),
        provider: None,
        model: None,
    }
}

pub(crate) fn summary(id: &str, title: Option<&str>, parent: Option<SessionId>) -> SessionSummary {
    SessionSummary {
        id: SessionId::from_raw(id),
        key: None,
        title: title.map(str::to_string),
        cwd: "/work/project".into(),
        parent: parent.map(|session| ParentLink {
            session,
            item: ItemId::from_raw("itm_call"),
        }),
        model: None,
        provider: None,
        created_at: ts(),
        updated_at: ts(),
        usage: Usage::default(),
        busy: false,
    }
}

pub(crate) fn stamped(seq: u64, event: Event, session: &SessionId) -> Frame {
    Frame {
        seq: Seq(seq),
        ts: ts(),
        session: session.clone(),
        cause: None,
        event,
    }
}

pub(crate) fn assistant(text: &str) -> Event {
    Event::ItemCompleted {
        item: Item {
            id: ItemId::mint(),
            turn: Some(TurnId::from_raw(TURN)),
            round: 0,
            status: ItemStatus::Completed,
            started_at: ts(),
            completed_at: Some(ts()),
            intent: None,
            body: ItemBody::Assistant { text: text.into() },
            meta: Default::default(),
        },
    }
}

pub(crate) fn turn_started() -> Event {
    Event::TurnStarted {
        turn: TurnId::from_raw(TURN),
        inputs: Vec::new(),
        origin: TurnOrigin::Peer,
    }
}

pub(crate) fn turn_completed() -> Event {
    Event::TurnCompleted {
        turn: TurnId::from_raw(TURN),
        status: TurnStatus::Completed,
        usage: Usage::default(),
    }
}

pub(crate) fn turn_failed(message: &str) -> Event {
    Event::TurnCompleted {
        turn: TurnId::from_raw(TURN),
        status: TurnStatus::Failed {
            error: KernelError::new(ErrorCode::AuthRequired, message),
        },
        usage: Usage::default(),
    }
}

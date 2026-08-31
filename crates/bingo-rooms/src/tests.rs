//! What every test in this crate needs: a host whose sessions are written down
//! rather than run, folding the extensions a plugin published into them and
//! recording the messages it delivered, plus the two contexts the sdk hands a
//! command and a hook.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use bingo_sdk::{
    Activation, Answer, AnswerSpec, Attachment, CancellationToken, Catalog, CatalogKind,
    ClientIdentity, CloseReason, CommandContext, ContentPart, Delivery, Driver, Env, ErrorCode,
    Event, Frame, FrameStream, GatewayStream, HistoryChunk, HistoryPage, HookContext, HostApi,
    HostHandle, Input, IntentId, InteractionId, InteractionKind, InterruptScope, Item, ItemBody,
    ItemId, ItemStatus, KernelError, OpenOptions, Origin, ParentLink, Prompter, Seq, SessionFilter,
    SessionHandle, SessionId, SessionPort, SessionSelector, SessionSpec, SessionState,
    SessionSummary, ToolContext, ToolHost, TurnId, Usage,
};
use jiff::Timestamp;
use serde_json::Value;

use crate::{PLUGIN, room};

/// Long ago: what a room's journal was stamped with before this process.
pub(crate) fn ts() -> Timestamp {
    Timestamp::UNIX_EPOCH
}

/// One session the fleet knows about: what it looks like, and the durable
/// frames its journal already holds.
struct Live {
    summary: SessionSummary,
    history: Vec<Event>,
}

#[derive(Default)]
struct Inner {
    sessions: Mutex<Vec<Live>>,
    /// The sessions this plugin asked for, and the messages it sent.
    created: Mutex<Vec<SessionSpec>>,
    delivered: Mutex<Vec<(SessionId, Input, Delivery)>>,
}

fn locked<T>(slot: &Mutex<T>) -> MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(|held| held.into_inner())
}

/// A host whose sessions are written down rather than run.
#[derive(Clone, Default)]
pub(crate) struct Fleet(Arc<Inner>);

impl Fleet {
    pub(crate) fn handle(&self) -> HostHandle {
        HostHandle(Arc::new(self.clone()))
    }

    /// A session with no parent, as a surface opens one.
    pub(crate) fn root(&self) -> SessionId {
        let id = SessionId::mint();
        self.add(summary(id.as_str(), None, None));
        id
    }

    /// A child a model answers in, titled `name`.
    pub(crate) fn child(&self, parent: &SessionId, name: &str) -> SessionId {
        let id = SessionId::mint();
        self.add(summary(id.as_str(), Some(name), Some(parent.clone())));
        id
    }

    /// A room that already stands under `parent`, as `seat` would have left it.
    pub(crate) fn room(&self, parent: &SessionId, name: &str) -> SessionId {
        let id = SessionId::mint();
        self.add(room_summary(id.as_str(), parent, name));
        id
    }

    fn add(&self, summary: SessionSummary) {
        locked(&self.0.sessions).push(Live {
            summary,
            history: Vec::new(),
        });
    }

    pub(crate) fn created(&self) -> Vec<SessionSpec> {
        locked(&self.0.created).clone()
    }

    pub(crate) fn delivered(&self) -> Vec<(SessionId, Input, Delivery)> {
        locked(&self.0.delivered).clone()
    }

    pub(crate) fn summary(&self, session: &SessionId) -> SessionSummary {
        locked(&self.0.sessions)
            .iter()
            .find(|live| &live.summary.id == session)
            .map(|live| live.summary.clone())
            .expect("a session the fleet knows")
    }

    /// The one session of that title, for a test that named a room but never
    /// held its id.
    pub(crate) fn titled(&self, title: &str) -> Option<SessionId> {
        locked(&self.0.sessions)
            .iter()
            .find(|live| live.summary.title.as_deref() == Some(title))
            .map(|live| live.summary.id.clone())
    }

    /// A room's membership as its own journal has it, folded the way any
    /// client would fold it.
    pub(crate) fn members(&self, session: &SessionId) -> Vec<String> {
        room::members_of(&self.snapshot(session))
    }

    /// A post into a room, written into its journal the way the kernel writes
    /// one and handed back as the frame the hook would see. One call, so a
    /// test cannot tell the hook one thing and the room another.
    pub(crate) fn post(
        &self,
        room: &SessionId,
        text: &str,
        who: Option<&str>,
        at: Timestamp,
    ) -> Event {
        let event = Event::ItemCompleted {
            item: Item {
                started_at: at,
                completed_at: Some(at),
                ..posted_item(text, who)
            },
        };
        self.remember(room, event.clone());
        event
    }

    /// Every payload this plugin has signalled on a session, in order: what
    /// the card said, and when it was taken away.
    pub(crate) fn signalled(&self, session: &SessionId, kind: &str) -> Vec<Value> {
        let sessions = locked(&self.0.sessions);
        let Some(live) = sessions.iter().find(|live| &live.summary.id == session) else {
            return Vec::new();
        };
        live.history
            .iter()
            .filter_map(|event| match event {
                Event::Signal {
                    plugin,
                    kind: said,
                    payload,
                } if plugin == PLUGIN && said == kind => Some(payload.clone()),
                _ => None,
            })
            .collect()
    }

    fn snapshot(&self, session: &SessionId) -> SessionState {
        let sessions = locked(&self.0.sessions);
        let live = sessions
            .iter()
            .find(|live| &live.summary.id == session)
            .expect("a session the fleet knows");
        let mut state = SessionState::new(live.summary.clone());
        for (n, event) in live.history.iter().enumerate() {
            state.apply(&stamped(n as u64 + 1, event.clone(), session));
        }
        state
    }

    fn remember(&self, session: &SessionId, event: Event) {
        let mut sessions = locked(&self.0.sessions);
        if let Some(live) = sessions.iter_mut().find(|l| &l.summary.id == session) {
            live.history.push(event);
        }
    }

    /// The session a `SessionSpec` describes, joined to the fleet.
    fn mint(&self, spec: &SessionSpec) -> SessionId {
        let id = SessionId::mint();
        let mut summary = summary(
            id.as_str(),
            spec.title.as_deref(),
            spec.parent.as_ref().map(|p| p.session.clone()),
        );
        summary.key = spec.key.clone();
        summary.driver = spec.driver;
        summary.cwd = spec.cwd.display().to_string();
        self.add(summary);
        id
    }
}

#[async_trait]
impl HostApi for Fleet {
    async fn sessions(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        Ok(locked(&self.0.sessions)
            .iter()
            .map(|live| live.summary.clone())
            .filter(|summary| match &filter.parent {
                Some(parent) => summary.parent.as_ref().map(|p| &p.session) == Some(parent),
                None => true,
            })
            .collect())
    }

    /// A session by id, or a new one from its spec.
    async fn open(
        &self,
        selector: SessionSelector,
        _who: ClientIdentity,
        _options: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        let id = match selector {
            SessionSelector::ById { id } => id,
            SessionSelector::Create { spec } => {
                locked(&self.0.created).push(spec.clone());
                self.mint(&spec)
            }
            _ => unreachable!("this plugin opens a session by id or by spec"),
        };
        let known = locked(&self.0.sessions)
            .iter()
            .any(|live| live.summary.id == id);
        if !known {
            return Err(KernelError::new(
                ErrorCode::SessionNotFound,
                "no such session",
            ));
        }
        Ok(Attachment {
            snapshot: self.snapshot(&id),
            session: id,
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
        to: &SessionId,
        _intent: IntentId,
        input: Input,
        delivery: Delivery,
    ) -> Result<(), KernelError> {
        locked(&self.0.delivered).push((to.clone(), input, delivery));
        Ok(())
    }

    async fn extend(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        self.remember(
            session,
            Event::Extension {
                plugin: plugin.to_string(),
                kind: kind.to_string(),
                payload,
            },
        );
        Ok(())
    }

    async fn signal(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        self.remember(
            session,
            Event::Signal {
                plugin: plugin.to_string(),
                kind: kind.to_string(),
                payload,
            },
        );
        Ok(())
    }

    async fn catalog(&self, _kind: CatalogKind) -> Result<Catalog, KernelError> {
        unreachable!("this plugin reads no catalogue")
    }

    fn gateway_events(&self) -> GatewayStream {
        Box::pin(futures::stream::empty())
    }

    fn service_any(&self, _key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

/// A handle nothing is written to: this plugin posts through `deliver`, never
/// through a client port.
struct Deaf;

#[async_trait]
impl SessionPort for Deaf {
    fn submit(&self, _intent: IntentId, _input: Input) {
        unreachable!("a room is never submitted to as a client")
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

/// The call's own side of the host. This plugin's tool opens a room and says
/// so in its result; it asks nobody anything and records nothing else.
struct Call;

#[async_trait]
impl Prompter for Call {
    async fn ask(
        &self,
        _kind: InteractionKind,
        _answers: Vec<AnswerSpec>,
    ) -> Result<Answer, KernelError> {
        unreachable!("this plugin's tool asks nobody anything")
    }
}

#[async_trait]
impl ToolHost for Call {
    fn progress(&self, _item: &ItemId, _tail: String) {}

    async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
        unreachable!("this plugin's tool records nothing of its own")
    }
}

pub(crate) fn tool_context(session: &SessionId, fleet: &Fleet) -> ToolContext {
    ToolContext {
        call_id: "call_test".into(),
        session: session.clone(),
        turn: TurnId::from_raw("trn_call"),
        item: ItemId::from_raw("itm_call"),
        cwd: PathBuf::from("/work/project"),
        cancel: CancellationToken::new(),
        env: Arc::new(Env::rooted("/nowhere")),
        host: fleet.handle(),
        call: Arc::new(Call),
    }
}

pub(crate) fn command_context(session: &SessionId, fleet: &Fleet) -> CommandContext {
    CommandContext {
        session: session.clone(),
        cwd: PathBuf::from("/work/project"),
        host: fleet.handle(),
    }
}

pub(crate) fn hook_context(session: &SessionId, fleet: &Fleet, cwd: &Path) -> HookContext {
    HookContext {
        session: session.clone(),
        turn: None,
        cwd: cwd.to_path_buf(),
        provider: None,
        model: None,
        host: fleet.handle(),
    }
}

pub(crate) fn summary(id: &str, title: Option<&str>, parent: Option<SessionId>) -> SessionSummary {
    SessionSummary {
        tools: None,
        system_extra: None,
        id: SessionId::from_raw(id),
        key: None,
        title: title.map(str::to_string),
        cwd: "/work/project".into(),
        parent: parent.map(|session| ParentLink {
            session,
            item: None,
        }),
        driver: Driver::Model,
        model: None,
        provider: None,
        created_at: ts(),
        updated_at: ts(),
        usage: Usage::default(),
        busy: false,
    }
}

/// A room as `seat` leaves one: a `Log` session titled `#name`, keyed under
/// this plugin.
pub(crate) fn room_summary(id: &str, parent: &SessionId, name: &str) -> SessionSummary {
    SessionSummary {
        key: Some(format!("{}{parent}/{name}", room::KEY)),
        driver: Driver::Log,
        ..summary(id, Some(&crate::name::title(name)), Some(parent.clone()))
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

/// The frame a session opens with.
pub(crate) fn updated(summary: &SessionSummary) -> Event {
    Event::SessionUpdated {
        summary: summary.clone(),
    }
}

/// This plugin's own membership frame.
pub(crate) fn extension(payload: Value) -> Event {
    Event::Extension {
        plugin: PLUGIN.into(),
        kind: room::MEMBERS.into(),
        payload,
    }
}

/// A post, as a `Log` session records one.
pub(crate) fn posted(text: &str, principal: Option<&str>) -> Event {
    Event::ItemCompleted {
        item: posted_item(text, principal),
    }
}

/// The item under one, for a test that reads a post rather than a frame.
pub(crate) fn posted_item(text: &str, principal: Option<&str>) -> Item {
    item(ItemBody::User {
        parts: vec![ContentPart::text(text)],
        origin: Origin {
            surface: "test".into(),
            principal: principal.map(str::to_string),
            conversation: None,
        },
    })
}

/// A completed item of any body, stamped at the epoch.
pub(crate) fn item(body: ItemBody) -> Item {
    Item {
        id: ItemId::mint(),
        turn: None,
        round: 0,
        status: ItemStatus::Completed,
        started_at: ts(),
        completed_at: Some(ts()),
        intent: None,
        body,
        meta: Default::default(),
    }
}

//! One adapter child per bingo session, and the conversation inside it.
//!
//! An ACP session is stateful and holds the history, so the child lives as
//! long as the bingo session does and dies with it — dropping a [`Link`] takes
//! the process group (ADR-0035 §3, `child::Adapter`).
//!
//! The agent's own session id is journaled once as an extension and never
//! copied: `Event::Extension` is re-stated onto the stream every time a
//! session starts, so the way this plugin reads the id back is by listening,
//! not by asking. Asking would mean opening the session from inside its own
//! turn, and a session serves nothing but its summary while its start hooks
//! run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use agent_client_protocol_schema::v1::{
    AgentCapabilities, CreateElicitationRequest, CreateElicitationResponse, Error as RpcError,
    InitializeRequest, LoadSessionRequest, NewSessionRequest, RequestPermissionRequest,
    RequestPermissionResponse, ResumeSessionRequest, SessionNotification,
};
use async_trait::async_trait;
use bingo_sdk::{Env, HostHandle, KernelError, Level, Message, ProviderError, SessionId};
use serde_json::json;
use tokio::sync::{Mutex, mpsc};

use crate::child::{self, Spawned};
use crate::config::Adapter;
use crate::connection::{Client, Connection};
use crate::error::AcpError;
use crate::ladder::{self, Opening};
use crate::{refusal, transcript};

/// The plugin id the extension is journaled under (ADR-0011 §2).
pub const PLUGIN: &str = "bingo.acp";

/// One extension kind per adapter, so two adapters on one session do not
/// overwrite each other's pointer.
pub const KIND_PREFIX: &str = "session:";

/// Where a running turn's stream goes.
type Sink = mpsc::UnboundedSender<SessionNotification>;

/// The client half of one link: where updates go, whether they are being
/// swallowed, and what the agent is told when it asks a question whose answer
/// is already written on its own row.
struct Inbox {
    adapter: String,
    sink: Mutex<Option<Sink>>,
    /// True while `session/load` replays. The journal already holds those
    /// turns; writing them again would be the conversation twice.
    loading: AtomicBool,
    /// Whether this adapter has already been told, once, where its permissions
    /// are configured.
    told: AtomicBool,
    host: Option<HostHandle>,
}

#[async_trait]
impl Client for Inbox {
    async fn update(&self, notification: SessionNotification) {
        if self.loading.load(Ordering::Acquire) {
            return;
        }
        if let Some(sink) = self.sink.lock().await.as_ref() {
            let _ = sink.send(notification);
        }
    }

    /// ADR-0035 §5: an ACP agent brings its own permission machinery, and the
    /// row that spawned it says what it may do. A question that arrives anyway
    /// is refused in the agent's own words, and the turn goes on.
    async fn permission(
        &self,
        request: RequestPermissionRequest,
    ) -> Result<RequestPermissionResponse, RpcError> {
        self.say_where_the_answer_lives().await;
        Ok(refusal::refused(&request))
    }

    async fn elicitation(
        &self,
        _request: CreateElicitationRequest,
    ) -> Result<CreateElicitationResponse, RpcError> {
        self.say_where_the_answer_lives().await;
        Ok(refusal::declined())
    }
}

impl Inbox {
    /// The one thing this plugin must not do silently: an agent asked, and was
    /// refused by a rule the person never sees unless it is said. Said once —
    /// an agent may ask on every call it makes.
    async fn say_where_the_answer_lives(&self) {
        if self.told.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(host) = self.host.as_ref() else {
            return;
        };
        let (level, code, text) = refusal::told(&self.adapter);
        let _ = host.notice(level, &code, &text).await;
    }
}

/// One live conversation: the child, the wire to it, and which session on the
/// far side it is.
pub struct Link {
    pub connection: Connection,
    pub acp: String,
    pub capabilities: AgentCapabilities,
    /// Spent by the first prompt: the file a freshly opened agent must read
    /// before it can answer.
    preamble: Mutex<Option<PathBuf>>,
    inbox: Arc<Inbox>,
    /// Dropping this ends the process group.
    _adapter: child::Adapter,
}

impl Link {
    /// Take the stream of the turn that is starting. The previous turn's sink,
    /// if any, is dropped with it.
    pub async fn listen(&self) -> mpsc::UnboundedReceiver<SessionNotification> {
        let (sink, updates) = mpsc::unbounded_channel();
        *self.inbox.sink.lock().await = Some(sink);
        updates
    }

    /// The file a restored agent must read, named once and then forgotten.
    pub async fn take_preamble(&self) -> Option<PathBuf> {
        self.preamble.lock().await.take()
    }
}

/// Every adapter's every session. One registry, shared by the providers, the
/// contributor that hands them their session, and the hook that hears the
/// journal.
pub struct Sessions {
    env: Env,
    host: Mutex<Option<HostHandle>>,
    links: Mutex<BTreeMap<(String, SessionId), Arc<Link>>>,
    /// What the journal says the agent called this session, by adapter.
    known: Mutex<BTreeMap<SessionId, BTreeMap<String, String>>>,
}

impl Sessions {
    pub fn new(env: Env) -> Arc<Self> {
        Arc::new(Self {
            env,
            host: Mutex::new(None),
            links: Mutex::new(BTreeMap::new()),
            known: Mutex::new(BTreeMap::new()),
        })
    }

    pub async fn set_host(&self, host: HostHandle) {
        *self.host.lock().await = Some(host);
    }

    /// What the journal just said, heard rather than asked for.
    pub async fn remember(&self, session: &SessionId, adapter: &str, acp: &str) {
        self.known
            .lock()
            .await
            .entry(session.clone())
            .or_default()
            .insert(adapter.to_string(), acp.to_string());
    }

    /// The process is going: every adapter child goes with it. Dropping a
    /// link takes its process group, so letting go of them all is the whole
    /// of a shutdown.
    pub async fn close(&self) {
        self.links.lock().await.clear();
        self.known.lock().await.clear();
    }

    /// The session ended: its children end with it.
    pub async fn forget(&self, session: &SessionId) {
        self.links.lock().await.retain(|(_, id), _| id != session);
        self.known.lock().await.remove(session);
    }

    pub async fn link(&self, adapter: &str, session: &SessionId) -> Option<Arc<Link>> {
        self.links
            .lock()
            .await
            .get(&(adapter.to_string(), session.clone()))
            .cloned()
    }

    /// Make sure this session has a conversation with this adapter, climbing
    /// the ladder if it has to. Called at the start of every round; after the
    /// first it is a lookup.
    pub async fn prepare(
        &self,
        name: &str,
        adapter: &Adapter,
        session: &SessionId,
        history: &[Message],
    ) -> Result<Arc<Link>, ProviderError> {
        match self.link(name, session).await {
            Some(link) if link.connection.is_alive() => return Ok(link),
            // A child that died between turns is replaced rather than asked:
            // the next call would only fail as transport. The ladder is
            // climbed again from the journal's own pointer, so what it lost
            // is at most one rung, and the person is told (ADR-0035 §3).
            Some(_) => self.bury(name, session).await,
            None => {}
        }
        let cwd = self.cwd(session).await?;
        let link = self.open(name, adapter, session, &cwd, history).await?;
        self.links
            .lock()
            .await
            .insert((name.to_string(), session.clone()), link.clone());
        Ok(link)
    }

    /// Let go of a dead adapter — dropping the link takes what is left of its
    /// process group — and say that it went.
    async fn bury(&self, name: &str, session: &SessionId) {
        self.links
            .lock()
            .await
            .remove(&(name.to_string(), session.clone()));
        let Some(host) = self.host.lock().await.clone() else {
            return;
        };
        let _ = host
            .notice(
                Level::Warn,
                "ACP_RESPAWN",
                &format!("{name} stopped between turns; a new one was started for this session."),
            )
            .await;
    }

    /// Where the agent works. A session answers its summary even while it is
    /// busy, which is why this is the one thing asked for rather than carried.
    async fn cwd(&self, session: &SessionId) -> Result<PathBuf, ProviderError> {
        let host = self
            .host
            .lock()
            .await
            .clone()
            .ok_or_else(|| ProviderError::Config {
                message: "the ACP plugin has no host to ask; it was never started".into(),
            })?;
        let summaries = host
            .sessions(bingo_sdk::SessionFilter::default())
            .await
            .map_err(config)?;
        summaries
            .into_iter()
            .find(|summary| &summary.id == session)
            .map(|summary| PathBuf::from(summary.cwd))
            .ok_or_else(|| ProviderError::Config {
                message: format!("no live session {session} to run an adapter for"),
            })
    }

    /// Spawn, shake hands, and get in through the highest door on offer.
    async fn open(
        &self,
        name: &str,
        adapter: &Adapter,
        session: &SessionId,
        cwd: &Path,
        history: &[Message],
    ) -> Result<Arc<Link>, ProviderError> {
        let inbox = self.inbox(name).await;
        let (connection, handle) = self.spawn(adapter, cwd, inbox.clone())?;
        let hello = connection.call(handshake()).await.map_err(from_acp)?;
        let known = self.known_id(session, name).await;
        let opening = ladder::opening(
            &hello.agent_capabilities,
            known.as_deref(),
            !history.is_empty(),
        );
        let entered = self
            .climb(&connection, &inbox, session, cwd, history, opening)
            .await?;
        self.journal(session, name, &entered.acp).await;
        Ok(Arc::new(Link {
            connection,
            acp: entered.acp,
            capabilities: hello.agent_capabilities,
            preamble: Mutex::new(entered.preamble),
            inbox,
            _adapter: handle,
        }))
    }

    async fn inbox(&self, name: &str) -> Arc<Inbox> {
        Arc::new(Inbox {
            adapter: name.to_string(),
            sink: Mutex::new(None),
            loading: AtomicBool::new(false),
            told: AtomicBool::new(false),
            host: self.host.lock().await.clone(),
        })
    }

    fn spawn(
        &self,
        adapter: &Adapter,
        cwd: &Path,
        inbox: Arc<Inbox>,
    ) -> Result<(Connection, child::Adapter), ProviderError> {
        let Spawned {
            adapter: handle,
            reader,
            writer,
        } = child::spawn(&adapter.command, &adapter.args, &adapter.env, cwd).map_err(from_acp)?;
        Ok((Connection::spawn(reader, writer, inbox), handle))
    }

    pub async fn known_id(&self, session: &SessionId, adapter: &str) -> Option<String> {
        self.known
            .lock()
            .await
            .get(session)
            .and_then(|by_adapter| by_adapter.get(adapter))
            .cloned()
    }

    /// One rung at a time, dropping to the next when a door the agent
    /// advertised refuses at the moment it is used.
    async fn climb(
        &self,
        connection: &Connection,
        inbox: &Arc<Inbox>,
        session: &SessionId,
        cwd: &Path,
        history: &[Message],
        opening: Opening,
    ) -> Result<Entered, ProviderError> {
        let mut rung = opening;
        loop {
            match self
                .enter(connection, inbox, session, cwd, history, &rung)
                .await
            {
                Ok(entered) => {
                    self.say(&rung, &inbox.adapter).await;
                    return Ok(entered);
                }
                Err(refused) => match ladder::below(&rung, !history.is_empty()) {
                    Some(next) => rung = next,
                    None => return Err(from_acp(refused)),
                },
            }
        }
    }

    async fn enter(
        &self,
        connection: &Connection,
        inbox: &Arc<Inbox>,
        session: &SessionId,
        cwd: &Path,
        history: &[Message],
        rung: &Opening,
    ) -> Result<Entered, AcpError> {
        match rung {
            Opening::Resume(id) => {
                connection.call(resume(id, cwd)).await?;
                Ok(Entered::at(id))
            }
            Opening::Load(id) => {
                self.load(connection, inbox, id, cwd).await?;
                Ok(Entered::at(id))
            }
            Opening::New => {
                let opened = connection.call(new_session(cwd)).await?;
                Ok(Entered::at(opened.session_id.0.as_ref()))
            }
            Opening::Fresh { transcript } => {
                let opened = connection.call(new_session(cwd)).await?;
                let mut entered = Entered::at(opened.session_id.0.as_ref());
                if *transcript {
                    entered.preamble = self.write_transcript(session, history);
                }
                Ok(entered)
            }
        }
    }

    /// A load replays the history it holds. Nothing of it reaches the journal:
    /// those turns are already there, and a second copy is not a restore.
    async fn load(
        &self,
        connection: &Connection,
        inbox: &Arc<Inbox>,
        id: &str,
        cwd: &Path,
    ) -> Result<(), AcpError> {
        inbox.loading.store(true, Ordering::Release);
        let outcome = connection.call(load_session(id, cwd)).await;
        inbox.loading.store(false, Ordering::Release);
        outcome.map(|_| ())
    }

    /// The transcript is a projection of the conversation at this moment. A
    /// file it could not write is not worth failing a turn over: the agent is
    /// told less, and the notice already says it kept nothing.
    fn write_transcript(&self, session: &SessionId, history: &[Message]) -> Option<PathBuf> {
        transcript::write(&self.env.data_dir.join("acp"), session, history).ok()
    }

    /// The pointer is written when it is news. A restore that got back into
    /// the session the journal already named has nothing to add, and a second
    /// copy of a fact the stream already carries is not a record, it is noise.
    async fn journal(&self, session: &SessionId, adapter: &str, acp: &str) {
        let known = self.known_id(session, adapter).await;
        self.remember(session, adapter, acp).await;
        if known.as_deref() == Some(acp) {
            return;
        }
        let Some(host) = self.host.lock().await.clone() else {
            return;
        };
        let _ = host
            .extend(
                session,
                PLUGIN,
                &kind(adapter),
                json!({ "sessionId": acp, "adapter": adapter }),
            )
            .await;
    }

    async fn say(&self, rung: &Opening, adapter: &str) {
        let Some((level, code, text)) = ladder::notice(rung, adapter) else {
            return;
        };
        let Some(host) = self.host.lock().await.clone() else {
            return;
        };
        let _ = host.notice(level, &code, &text).await;
    }
}

fn config(error: KernelError) -> ProviderError {
    ProviderError::Config {
        message: error.to_string(),
    }
}

/// What the extension is filed under.
pub fn kind(adapter: &str) -> String {
    format!("{KIND_PREFIX}{adapter}")
}

/// The agent-side session id an extension payload holds, and nothing else.
pub fn session_id_from(payload: &serde_json::Value) -> Option<&str> {
    payload["sessionId"].as_str()
}

/// Where a rung landed.
struct Entered {
    acp: String,
    preamble: Option<PathBuf>,
}

impl Entered {
    fn at(acp: &str) -> Self {
        Self {
            acp: acp.to_string(),
            preamble: None,
        }
    }
}

fn from_acp(error: AcpError) -> ProviderError {
    error.into()
}

/// ADR-0035 §6: no filesystem, no terminal. The agent works on its own
/// machine with its own tools, and is told so in the handshake rather than
/// discovering it at the first `fs/read_text_file`.
fn handshake() -> InitializeRequest {
    InitializeRequest::new(agent_client_protocol_schema::ProtocolVersion::V1)
        .client_info(implementation())
}

fn implementation() -> agent_client_protocol_schema::v1::Implementation {
    agent_client_protocol_schema::v1::Implementation::new("bingo", env!("CARGO_PKG_VERSION"))
}

/// `mcpServers` stays empty: our tools do not cross (ADR-0035 §6).
fn new_session(cwd: &std::path::Path) -> NewSessionRequest {
    NewSessionRequest::new(cwd.to_path_buf())
}

fn resume(id: &str, cwd: &std::path::Path) -> ResumeSessionRequest {
    ResumeSessionRequest::new(
        agent_client_protocol_schema::v1::SessionId::new(id),
        cwd.to_path_buf(),
    )
}

fn load_session(id: &str, cwd: &std::path::Path) -> LoadSessionRequest {
    LoadSessionRequest::new(
        agent_client_protocol_schema::v1::SessionId::new(id),
        cwd.to_path_buf(),
    )
}

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

use agent_client_protocol_schema::v1::{
    AgentCapabilities, InitializeRequest, LoadSessionRequest, McpServer, NewSessionRequest,
    ResumeSessionRequest, SessionNotification,
};
use bingo_sdk::{
    Env, HostHandle, KernelError, Level, Message, ModelInfo, ProviderError, SessionId, ToolSpec,
};
use serde_json::json;
use tokio::sync::{Mutex, mpsc};

use crate::bridge::Bridge;
use crate::child::{self, Spawned};
use crate::config::Adapter;
use crate::connection::Connection;
use crate::crossing::{self, Crossing};
use crate::error::AcpError;
use crate::inbox::Inbox;
use crate::knobs::{Declared, Knobs, Wanted, Wire};
use crate::ladder::{self, Opening};
use crate::{probe, transcript};

/// The plugin id the extension is journaled under (ADR-0011 §2).
pub const PLUGIN: &str = "bingo.acp";

/// One extension kind per adapter, so two adapters on one session do not
/// overwrite each other's pointer.
pub const KIND_PREFIX: &str = "session:";

/// One live conversation: the child, the wire to it, and which session on the
/// far side it is.
pub struct Link {
    pub connection: Connection,
    pub acp: String,
    pub capabilities: AgentCapabilities,
    /// Spent by the first prompt: what a freshly opened agent must be told
    /// before it can answer — the file it has to read, the tools it now has.
    preamble: Mutex<Option<String>>,
    /// This conversation's way back into bingo, if a bridge was opened for it.
    /// Dropping it dismisses the token, which is why it is held here: the link
    /// and the conversation end together.
    crossing: Option<Crossing>,
    /// What this agent said its knobs are, and where bingo has turned them
    /// (ADR-0037). They belong to the conversation and end with it.
    pub knobs: Knobs,
    inbox: Arc<Inbox>,
    /// Dropping this ends the process group.
    _adapter: child::Adapter,
}

impl Link {
    /// Take the stream of the turn that is starting. The previous turn's sink,
    /// if any, is dropped with it.
    pub async fn listen(&self) -> mpsc::UnboundedReceiver<SessionNotification> {
        self.inbox.listen().await
    }

    /// What the agent must be told first, said once and then forgotten.
    pub async fn take_preamble(&self) -> Option<String> {
        self.preamble.lock().await.take()
    }

    /// Hand the tool list of the request about to be served to the doors, and
    /// say whether what the agent may call has moved (ADR-0036 §1).
    pub async fn observe(&self, tools: &[ToolSpec]) -> bool {
        match &self.crossing {
            Some(crossing) => crossing.doors.observe(tools).await,
            None => false,
        }
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
    /// This run's tool bridge, opened by the first session that needs one and
    /// dropped when the plugin stops. One per run: the address carries the
    /// pid, and every conversation on it is a token (ADR-0036 §3).
    bridge: Mutex<Option<Arc<Bridge>>>,
    /// What each adapter said it serves when it was asked cold — an opening
    /// nobody is having a conversation in (`crate::probe`).
    cold: probe::Cold,
}

impl Sessions {
    pub fn new(env: Env) -> Arc<Self> {
        Arc::new(Self {
            env,
            host: Mutex::new(None),
            links: Mutex::new(BTreeMap::new()),
            known: Mutex::new(BTreeMap::new()),
            bridge: Mutex::new(None),
            cold: probe::Cold::default(),
        })
    }

    /// Where this run keeps what is its own. The probe opens its throwaway
    /// session here, having no person's session to take a directory from.
    pub(crate) fn env(&self) -> &Env {
        &self.env
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
    /// of a shutdown — and dropping the bridge takes the socket.
    pub async fn close(&self) {
        self.links.lock().await.clear();
        self.known.lock().await.clear();
        *self.bridge.lock().await = None;
    }

    /// The house's tool set moved: every live conversation is told to ask
    /// again. Nothing to do when no bridge was ever opened.
    pub async fn offer_changed(&self) {
        if let Some(bridge) = self.bridge.lock().await.as_ref() {
            bridge.offer_changed();
        }
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
            //
            // This is where most of those deaths are found, not all of them:
            // a child can go between this check and the write, and the turn
            // that discovers it there buries it and asks here again
            // (`provider::Asking::afresh`).
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
    /// process group — and say that it went. One death is one notice: whoever
    /// found it buries it, and [`Self::prepare`] afterwards finds nothing left
    /// to bury.
    pub(crate) async fn bury(&self, name: &str, session: &SessionId) {
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
        let inbox = self.inbox(name, Some(session)).await;
        let (connection, handle) = self.spawn(adapter, cwd, inbox.clone())?;
        let hello = connection.call(handshake()).await.map_err(from_acp)?;
        let crossing = self
            .crossing(session, (name, adapter), &hello.agent_capabilities)
            .await;
        let known = self.known_id(session, name).await;
        let opening = ladder::opening(
            &hello.agent_capabilities,
            known.as_deref(),
            !history.is_empty(),
        );
        let place = Where {
            cwd,
            servers: crossing
                .as_ref()
                .map(|c| c.servers.as_slice())
                .unwrap_or(&[]),
        };
        let entered = self
            .climb(&connection, &inbox, session, &place, history, opening)
            .await?;
        self.journal(session, name, &entered.acp).await;
        let link = Arc::new(Link {
            connection,
            acp: entered.acp,
            capabilities: hello.agent_capabilities,
            preamble: Mutex::new(prelude(entered.preamble.as_deref(), crossing.is_some())),
            crossing,
            knobs: Knobs::new(entered.declared),
            inbox,
            _adapter: handle,
        });
        self.preset(name, adapter, &link).await;
        Ok(link)
    }

    /// What the adapter's own row asks of every session with it, said once per
    /// opening and before any prompt (`config::Adapter::options`). Here rather
    /// than beside a turn because this is what an opening *is*: a respawned
    /// child climbs back in through here too, and comes back set the way the
    /// row says rather than the way its predecessor was left.
    async fn preset(&self, name: &str, adapter: &Adapter, link: &Link) {
        if adapter.options.is_empty() {
            return;
        }
        let host = self.host.lock().await.clone();
        link.knobs
            .preset(&wire(name, link, &host), &adapter.options)
            .await;
    }

    /// The knobs this request asks for, turned before its prompt goes out
    /// (ADR-0037 §4). It answers nothing: a knob is the agent's, and one bingo
    /// could not turn is a notice and a turn that still runs.
    pub async fn tune(&self, name: &str, link: &Link, wanted: Wanted<'_>) {
        let host = self.host.lock().await.clone();
        link.knobs.apply(&wire(name, link, &host), wanted).await;
    }

    /// The models this adapter says it has. A live conversation's declaration
    /// is the freshest there is — it is the one a `set` answer keeps moving —
    /// so it is read first and a cold ask's harvest only stands in for it
    /// (ADR-0037 §2, `crate::probe`).
    pub async fn models(&self, name: &str, adapter: &Adapter) -> Vec<ModelInfo> {
        match self.live(name).await {
            Some(link) => link.knobs.models().await,
            None => self.cold.models(self, name, adapter).await,
        }
    }

    /// Any conversation with this adapter. Which one does not matter: they all
    /// speak to the same agent, and it declares the same list to each.
    async fn live(&self, adapter: &str) -> Option<Arc<Link>> {
        self.links
            .lock()
            .await
            .iter()
            .find(|((name, _), _)| name == adapter)
            .map(|(_, link)| link.clone())
    }

    /// This session's way back into bingo, if there is a bridge to open it on.
    ///
    /// A bridge that will not listen is not worth failing a turn over: the
    /// agent still answers, it just cannot act, and the person is told which
    /// of the two it got.
    async fn crossing(
        &self,
        session: &SessionId,
        named: (&str, &Adapter),
        capabilities: &AgentCapabilities,
    ) -> Option<Crossing> {
        let host = self.host.lock().await.clone()?;
        let bridge = self.bridge().await?;
        let exe = std::env::current_exe().ok()?;
        match crossing::open(&bridge, &host, session, named, capabilities, &exe).await {
            Ok(crossing) => Some(crossing),
            Err(why) => {
                let (name, _) = named;
                self.warn(
                    "ACP_BRIDGE",
                    &format!("{name} was opened without bingo's own tools: {why}"),
                )
                .await;
                None
            }
        }
    }

    /// This run's bridge, opened by the first session that asks for one.
    async fn bridge(&self) -> Option<Arc<Bridge>> {
        let mut held = self.bridge.lock().await;
        if held.is_none() {
            match Bridge::open(&self.env) {
                Ok(bridge) => *held = Some(Arc::new(bridge)),
                Err(why) => {
                    self.warn(
                        "ACP_BRIDGE",
                        &format!("bingo's tools are not reachable from this run's agents: {why}"),
                    )
                    .await;
                }
            }
        }
        held.clone()
    }

    async fn warn(&self, code: &str, text: &str) {
        self.heard(code, text).await;
    }

    /// Say it, and answer whether anybody was there. A notice said while no
    /// session is open reaches nobody — the kernel says as much — so a caller
    /// whose word is owed to a person keeps it and says it again later
    /// (`crate::probe`).
    pub(crate) async fn heard(&self, code: &str, text: &str) -> bool {
        let Some(host) = self.host.lock().await.clone() else {
            return false;
        };
        host.notice(Level::Warn, code, text).await.is_ok()
    }

    /// The client half every conversation with this adapter is read through,
    /// a cold ask's included: it answers the two questions an agent may put
    /// before it is prompted, and a probe that left them unanswered would
    /// leave an agent waiting on the way in. A cold ask names no session,
    /// because it is nobody's (`crate::inbox`).
    pub(crate) async fn inbox(&self, name: &str, session: Option<&SessionId>) -> Arc<Inbox> {
        Arc::new(Inbox::new(name, self.host.lock().await.clone(), session))
    }

    /// One adapter process and the wire to it. The one place a child is
    /// started: a cold ask spawns through here too, so there is a single
    /// answer to "how is an adapter run".
    pub(crate) fn spawn(
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
        place: &Where<'_>,
        history: &[Message],
        opening: Opening,
    ) -> Result<Entered, ProviderError> {
        let mut rung = opening;
        loop {
            match self
                .enter(connection, inbox, session, place, history, &rung)
                .await
            {
                Ok(entered) => {
                    self.say(&rung, inbox.adapter()).await;
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
        place: &Where<'_>,
        history: &[Message],
        rung: &Opening,
    ) -> Result<Entered, AcpError> {
        match rung {
            Opening::Resume(id) => {
                let (answer, body) = connection.call_seen(resume(id, place)).await?;
                Ok(Entered::at(id, Declared::of(answer.config_options, &body)))
            }
            Opening::Load(id) => {
                let declared = self.load(connection, inbox, id, place).await?;
                Ok(Entered::at(id, declared))
            }
            Opening::New => self.fresh(connection, place).await,
            Opening::Fresh { transcript } => {
                let mut entered = self.fresh(connection, place).await?;
                if *transcript {
                    entered.preamble = self.write_transcript(session, history);
                }
                Ok(entered)
            }
        }
    }

    /// A session the agent has never seen before. Every rung answers with the
    /// knobs it is offering, and this is the one that also names the session.
    /// It is also the whole of a cold ask (`crate::probe`): the one door in
    /// the protocol that says what an agent serves is a session opening.
    pub(crate) async fn fresh(
        &self,
        connection: &Connection,
        place: &Where<'_>,
    ) -> Result<Entered, AcpError> {
        let (opened, body) = connection.call_seen(new_session(place)).await?;
        Ok(Entered::at(
            opened.session_id.0.as_ref(),
            Declared::of(opened.config_options, &body),
        ))
    }

    /// A load replays the history it holds. Nothing of it reaches the journal:
    /// those turns are already there, and a second copy is not a restore.
    async fn load(
        &self,
        connection: &Connection,
        inbox: &Arc<Inbox>,
        id: &str,
        place: &Where<'_>,
    ) -> Result<Declared, AcpError> {
        inbox.replaying(true);
        let outcome = connection.call_seen(load_session(id, place)).await;
        inbox.replaying(false);
        let (answer, body) = outcome?;
        Ok(Declared::of(answer.config_options, &body))
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

/// Where a knob's message goes and who hears about it. The host is the
/// caller's to hold: it is a clone taken out from under a lock, and the wire
/// only borrows it.
fn wire<'a>(name: &'a str, link: &'a Link, host: &'a Option<HostHandle>) -> Wire<'a> {
    Wire {
        connection: &link.connection,
        session: &link.acp,
        adapter: name,
        host: host.as_ref(),
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

/// Where a rung landed, and what the agent said on the way in.
pub(crate) struct Entered {
    acp: String,
    preamble: Option<PathBuf>,
    /// The knobs the agent offered, which is also the catalogue it serves.
    pub(crate) declared: Declared,
}

impl Entered {
    fn at(acp: &str, declared: Declared) -> Self {
        Self {
            acp: acp.to_string(),
            preamble: None,
            declared,
        }
    }
}

fn from_acp(error: AcpError) -> ProviderError {
    error.into()
}

/// ADR-0035 §6: no filesystem, no terminal. The agent works on its own
/// machine with its own tools, and is told so in the handshake rather than
/// discovering it at the first `fs/read_text_file`.
///
/// The same words whoever is asking: a cold ask (`crate::probe`) introduces
/// itself as this client too, because it is this client.
pub(crate) fn handshake() -> InitializeRequest {
    InitializeRequest::new(agent_client_protocol_schema::ProtocolVersion::V1)
        .client_info(implementation())
}

fn implementation() -> agent_client_protocol_schema::v1::Implementation {
    agent_client_protocol_schema::v1::Implementation::new("bingo", env!("CARGO_PKG_VERSION"))
}

/// Where a conversation is opened, and what it is opened with.
///
/// The rows travel with every rung, not only the new one: a resumed or loaded
/// session is handed its `mcpServers` afresh, and one that was not would come
/// back with the tools of a run that has ended.
pub(crate) struct Where<'a> {
    cwd: &'a Path,
    servers: &'a [McpServer],
}

impl<'a> Where<'a> {
    /// A place with nothing to offer: where a cold ask opens its session
    /// (`crate::probe`). An agent that will never be prompted has nothing to
    /// call, so it is handed no servers — and no bridge is opened to hold.
    pub(crate) fn bare(cwd: &'a Path) -> Self {
        Self { cwd, servers: &[] }
    }
}

/// `mcpServers` carries the bridge and whatever a person's own rows forward
/// (ADR-0036 §§3–4; ADR-0035 §6 said no tools cross, and this amends it).
fn new_session(place: &Where<'_>) -> NewSessionRequest {
    NewSessionRequest::new(place.cwd.to_path_buf()).mcp_servers(place.servers.to_vec())
}

fn resume(id: &str, place: &Where<'_>) -> ResumeSessionRequest {
    ResumeSessionRequest::new(
        agent_client_protocol_schema::v1::SessionId::new(id),
        place.cwd.to_path_buf(),
    )
    .mcp_servers(place.servers.to_vec())
}

fn load_session(id: &str, place: &Where<'_>) -> LoadSessionRequest {
    LoadSessionRequest::new(
        agent_client_protocol_schema::v1::SessionId::new(id),
        place.cwd.to_path_buf(),
    )
    .mcp_servers(place.servers.to_vec())
}

/// What the first prompt of a conversation carries in front of what the person
/// said: the transcript a restored agent must read, the tools a bridged one
/// now has, or nothing at all.
fn prelude(transcript: Option<&Path>, bridged: bool) -> Option<String> {
    let mut said = Vec::new();
    if let Some(path) = transcript {
        said.push(transcript::first_prompt(path));
    }
    if bridged {
        said.push(crossing::SAYS.to_string());
    }
    (!said.is_empty()).then(|| said.join("\n\n"))
}

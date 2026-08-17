//! Named agent definitions and the subagent instance registry (D29).
//!
//! Definitions (AgentDef): on-disk persona templates — frontmatter metadata plus a
//! system prompt body, mirroring the directory convention of skills. Instances
//! (AgentRegistry entries): live sessions produced by one spawn — they hold a child
//! Session with the full message history, and the main agent resumes the conversation
//! via SendMessage (hub-and-spoke: only the main session has the management tools).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::api::types::Message;
use crate::query::Session;

/// Definition source layer (D31 `/team list` badge; same-name first-wins across layers picks the project layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDefSource {
    Project,
    User,
    /// Explicit default for legacy data/config without a source (no guessing).
    Unknown,
}

/// A named agent definition: `<name>.md` (YAML frontmatter + body system prompt).
#[derive(Debug, Clone)]
pub struct AgentDef {
    pub name: String,
    /// Catalog description (models are chosen based on this).
    pub description: String,
    /// Default model (instance params > definition > inherited from parent session).
    pub model: Option<String>,
    /// Default provider (same precedence as above).
    pub provider: Option<String>,
    /// Default thinking level (same precedence; None = inherit the parent session's current level).
    pub thinking: Option<String>,
    /// Body = the subagent's system prompt (empty means inherit the parent's unchanged).
    pub system: String,
    /// Whether the body is appended to the parent's system blocks (default) or replaces them.
    /// Replacing also drops the environment info, CLAUDE.md/AGENTS.md and project memory, so it
    /// is opt-in: `inherit_system: false` in the frontmatter.
    pub inherit_system: bool,
    /// First origin (the loading layer before first-wins dedup).
    pub source: AgentDefSource,
}

/// User-level definitions directory: `$XDG_CONFIG_HOME/bingo/agents` (mirrors the skills convention).
/// Tests must not depend on the ambient XDG_CONFIG_HOME (CI runners may set it): the home
/// parameter is the sole source of truth under test.
fn user_agents_dir(home: &Path) -> PathBuf {
    #[cfg(not(test))]
    let config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".config"));
    #[cfg(test)]
    let config = home.join(".config");
    config.join("bingo").join("agents")
}

/// Walk up from cwd, looking for `.bingo/agents` at each level.
fn project_agents_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        dirs.push(d.join(".bingo").join("agents"));
        dir = d.parent();
    }
    dirs
}

fn load_dir(dir: &Path, source: AgentDefSource, out: &mut Vec<AgentDef>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();
    for path in files {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (pairs, body) = crate::skills::parse_frontmatter_pairs(&raw);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut def = AgentDef {
            name: stem,
            description: String::new(),
            model: None,
            provider: None,
            thinking: None,
            system: body.trim_end().to_string(),
            inherit_system: true,
            source,
        };
        for (key, value) in pairs {
            match key.as_str() {
                "name" => def.name = value,
                "description" => def.description = value,
                "model" => def.model = Some(value),
                "provider" => def.provider = Some(value),
                "thinking" => def.thinking = Some(value),
                "inherit_system" => {
                    def.inherit_system = !matches!(
                        value.trim().to_ascii_lowercase().as_str(),
                        "false" | "no" | "off" | "0"
                    )
                }
                _ => {}
            }
        }
        if def.description.is_empty() {
            def.description = crate::skills::first_line(&def.system);
        }
        if !def.name.is_empty() {
            out.push(def);
        }
    }
}

/// Load all definitions: project layers (nearest cwd first) → user layer; same-name
/// first-wins (project overrides user). Definitions are usually few; no mtime caching.
pub fn load_agent_defs(home: &Path, cwd: &Path) -> Vec<AgentDef> {
    let mut defs = Vec::new();
    for dir in project_agents_dirs(cwd) {
        load_dir(&dir, AgentDefSource::Project, &mut defs);
    }
    load_dir(&user_agents_dir(home), AgentDefSource::User, &mut defs);
    let mut seen = std::collections::HashSet::new();
    defs.retain(|d| seen.insert(d.name.clone()));
    defs
}

/// Instance lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Turn in progress (new messages queue and are absorbed between tool rounds).
    Running,
    /// Waiting for a command (SendMessage wakes it immediately; history is kept).
    Idle,
    /// Stopped (aborted; history is kept and a direct message resumes it — only delete releases the name).
    Stopped,
}

impl AgentState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Idle => "idle",
            Self::Stopped => "stopped",
        }
    }
}

/// What an instance is to this project (D53): a standing member of the crew pinned in
/// `.bingo/team.json`, or someone hired for one task because no member covered it.
///
/// The distinction is not cosmetic. A member is a commitment the user made in a committed
/// file and outlives every task; a hire is spawned by the model, never enters that file,
/// and is released once its task is done — so the two cannot be the same row in a listing
/// or the same lifetime in the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    /// Spawned from the blueprint by `spawn_team`.
    Crew,
    /// Spawned ad hoc by the Agent tool for a single task.
    Hire,
}

impl AgentKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Crew => "crew",
            Self::Hire => "hire",
        }
    }
}

/// What a start did to an instance that was already up (D69). A team's blueprint and its
/// agent definitions are files the user edits between runs; start is where an instance
/// that is not busy catches up with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// The definition had moved: the instance now runs under the new one, history intact.
    Refreshed,
    /// Same definition — nothing was touched.
    Unchanged,
    /// Mid-turn: definitions are swapped between turns, never under one.
    Busy,
    /// The name is held by a temporary hire that took it first. A hire is not a member
    /// (D53) and its persona is not the blueprint's to rewrite.
    Hired,
    /// No such instance.
    Missing,
}

/// Sweeps a finished hire survives before it is released. One is not enough: a hire that
/// finishes during main round N has its result reported in round N+1, which is the round
/// main can first act on it — releasing at the end of N+1's own sweep would take the
/// instance away in the same round its result arrived. Two gives main exactly one round
/// to send a follow-up (which refills the inbox and resets the count) before the name goes.
const HIRE_LEASE: u8 = 2;

/// Snapshot for list.
#[derive(Debug, Clone)]
pub struct AgentStatus {
    pub name: String,
    pub def: Option<String>,
    pub description: String,
    pub prompt: String,
    pub state: AgentState,
    /// Crew member or temporary hire (D53).
    pub kind: AgentKind,
    /// Messages waiting in the inbox for the receiver to claim.
    pub pending: usize,
    /// Messages the sender has had no reply to yet — queued, or read and left unanswered.
    pub unacked: usize,
    /// The engine this instance actually runs on. Worth reporting because it need
    /// not be the session's: a definition or a team blueprint can pin a different
    /// one per instance, and "which member is on which model" is otherwise
    /// invisible until the bill arrives.
    pub model: String,
    pub provider: String,
    /// Elapsed time of the current run; absent while idle or stopped.
    pub elapsed: Option<Duration>,
    /// Cumulative output tokens reported by the current model run.
    pub output_tokens: u64,
    /// Tool calls observed in the current run.
    pub tool_uses: usize,
    /// Most recent tool activity in this run, oldest first.
    pub recent_activity: Vec<String>,
    /// When this instance last did something real (inbox receipt, turn start/end, activity).
    pub last_active: Instant,
}

/// Message identifier, unique per registry. Handed back to the sender so it can check later
/// whether the message actually reached the receiver's context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MsgId(pub u64);

impl std::fmt::Display for MsgId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What became of a sent message. Two of these look like success and are not: `Queued` only means
/// the message is sitting in the inbox, and `Delivered` only means it was read into the receiver's
/// prompt — a receiver that takes a message and says nothing leaves it there. `Answered` is the
/// acknowledgement: the run that carried the message ended with something to say back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AckState {
    Queued,
    /// Folded into the prompt of the instance's run #N, with nothing back from that run yet.
    Delivered {
        run: u64,
    },
    /// Run #N ended with a reply for main, which answers this message (not necessarily the run
    /// that first read it: a message read during a silent run is answered by the one that speaks).
    Answered {
        run: u64,
    },
    /// Never delivered, and never will be (instance stopped or removed).
    Dropped {
        reason: String,
    },
}

impl AckState {
    /// Whether the sender is still owed something. Both waiting states — nobody picked the message
    /// up, and somebody did but stayed silent — are the same thing from the sender's side.
    pub fn is_outstanding(&self) -> bool {
        matches!(self, Self::Queued | Self::Delivered { .. })
    }
}

/// One message's delivery record, kept after the fact so the sender can audit it.
#[derive(Debug, Clone)]
pub struct Ack {
    pub id: MsgId,
    /// First line of the message, for identifying it in a listing.
    pub excerpt: String,
    pub state: AckState,
    /// When the message was accepted (age drives the "still not acknowledged?" check).
    pub queued_at: Instant,
    /// Wait the sender allowed before the acknowledgement is chased automatically
    /// (None = no watchdog; the sender never asked for one).
    pub timeout: Option<Duration>,
    /// Follow-ups already spent chasing this message (0..=MAX_FOLLOW_UPS).
    pub follow_ups: u8,
    /// The output offset when this message entered the running query. A reply only acknowledges
    /// it if the query produced text after that point.
    delivered_after_chars: Option<usize>,
}

/// Retained delivery records per instance. Bounded: acks are an audit trail, not storage.
const MAX_ACKS: usize = 64;

/// Follow-up budget per message. Chasing an acknowledgement forever is not a mechanism, it is a
/// loop: after this many rounds the watchdog stops nudging and reports instead.
pub const MAX_FOLLOW_UPS: u8 = 3;

/// What chasing one message's acknowledgement found — the same delivery record
/// `AgentControl(action=messages)` reports, read at the moment the sender's wait elapsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowUp {
    /// The message left the queue (delivered into a run, or dropped): nothing left to chase.
    Settled(AckState),
    /// Still queued, and this round's follow-up is now in the receiver's inbox.
    Sent { round: u8 },
    /// Still queued with the follow-up budget spent.
    Exhausted,
    /// The instance, or its delivery record, no longer exists.
    Gone,
}

/// Inbox item: a direct main command, or a channel message (injected in batch on wake, in order).
#[derive(Debug, Clone)]
pub enum InboxItem {
    Direct {
        id: MsgId,
        /// Who sent it: [`crate::channels::MAIN_NAME`] for main's SendMessage,
        /// [`crate::channels::USER_NAME`] when the human wrote it (DM window, `/team assign`).
        /// Main is the default voice of direct instructions and stays untagged in the
        /// prompt; the user is the exception worth marking (D64).
        from: String,
        text: String,
        /// Images the `#[image N]` markers in `text` resolved to at send time. Carried with the
        /// message so a queued instruction still has them when it is finally delivered.
        images: Vec<crate::api::types::ImageAttachment>,
    },
    Channel {
        channel: String,
        from: String,
        text: String,
        seq: u64,
    },
    /// Automatic chase for a direct message main never got an answer to. It carries no new
    /// instruction — only the fact that the sender is still waiting.
    FollowUp {
        original: MsgId,
        /// 1-based, out of MAX_FOLLOW_UPS.
        round: u8,
        excerpt: String,
        waited: Duration,
        /// Whether the message had already been read into a prompt. The two silences need
        /// different words: nobody picked it up, versus you read it and said nothing.
        delivered: bool,
    },
    /// The room half of the same chase (v7 batch 3). An `@` is the only thing a
    /// room post can owe (R1), and until the ledger it was the only obligation
    /// nothing followed up on — a direct message has been chased since D44 while
    /// a mention in a room ran bare.
    Unanswered {
        channel: String,
        /// The post that named this member.
        seq: u64,
        from: String,
        excerpt: String,
        /// 1-based, out of MAX_FOLLOW_UPS.
        round: u8,
        waited: Duration,
    },
}

/// A run the caller should start: the instance was idle with a non-empty inbox, and this call
/// claimed it (state is already Running, inbox already drained) — so two flushes can't
/// double-start the same instance.
/// What [`AgentRegistry::view_of`] samples: history, the landing-time stamp of
/// each history message (unix seconds, 0 = unknown), and the instance state.
///
/// The live tail left with D134. A running turn reaches the console as events
/// now, the way main's always has, so there is nothing here to poll for it.
pub type AgentView = (Vec<Message>, Vec<u64>, AgentState);

pub struct Wake {
    pub name: String,
    pub session: Arc<Session>,
    pub history: Vec<Message>,
    pub items: Vec<InboxItem>,
    /// Sequence number of the run these items were folded into.
    pub run: u64,
}

/// Continuation of a finished turn: the inbox refilled while it was running.
pub struct Continuation {
    pub history: Vec<Message>,
    pub items: Vec<InboxItem>,
    pub run: u64,
}

struct Entry {
    def: Option<String>,
    description: String,
    prompt: String,
    state: AgentState,
    kind: AgentKind,
    last_active: Instant,
    /// Sweeps a finished hire has left before release, refilled whenever it has work
    /// again. Meaningless for a crew member, which no sweep touches.
    lease: u8,
    /// Full message history since the last completed turn (continuation context).
    history: Vec<Message>,
    /// Wall-clock landing time of each history message, unix seconds, index-aligned
    /// with `history` (0 = unknown). Display metadata only — never sent to the model.
    stamps: Vec<u64>,
    /// Inbox accumulated since the last drain (commands + channel messages, claimed as one
    /// batch when the receiver is ready).
    inbox: Vec<InboxItem>,
    /// Direct messages drained into the current run and not yet landed in `history`,
    /// each with the sender it came from.
    /// Without this record a sent message vanishes for the whole turn: the inbox is
    /// emptied at the claim point and `history` only catches up at [`AgentRegistry::finish`].
    /// The DM view bridges that window from here; cleared when the history lands, and
    /// pruned when a failed run puts its batch back in the inbox. The sender is kept
    /// because the pair view is one conversation (D99): main's instruction in flight
    /// is not the user's message and must not render as one.
    in_flight: Vec<(MsgId, String, String)>,
    /// Delivery records for direct messages, oldest first, capped at MAX_ACKS.
    acks: Vec<Ack>,
    session: Arc<Session>,
    abort: Option<tokio::task::AbortHandle>,
    /// Cumulative run count (watch lines are labeled `#N`).
    runs: u64,
    /// Watch line of the current turn (used to set Cancelled on stop/delete).
    watch_id: Option<crate::watch::WatchId>,
    /// Progress sampled by the main TUI's background-task manager.
    progress: Option<Arc<Mutex<AgentProgress>>>,
}

const RECENT_AGENT_ACTIVITIES: usize = 5;

#[derive(Debug, Clone, Default)]
pub struct AgentProgress {
    pub started_at: Option<Instant>,
    pub output_tokens: u64,
    pub tool_uses: usize,
    pub recent_activity: Vec<String>,
}

impl AgentProgress {
    pub fn start_run(&mut self) {
        self.started_at = Some(Instant::now());
        self.output_tokens = 0;
        self.tool_uses = 0;
        self.recent_activity.clear();
    }

    pub fn add_output_tokens(&mut self, tokens: u64) {
        self.output_tokens = self.output_tokens.saturating_add(tokens);
    }

    pub fn record_tool(&mut self, activity: String) {
        self.tool_uses += 1;
        self.recent_activity.push(activity);
        if self.recent_activity.len() > RECENT_AGENT_ACTIVITIES {
            self.recent_activity.remove(0);
        }
    }

    pub fn restore_attempt(
        &mut self,
        output_tokens: u64,
        tool_uses: usize,
        recent_activity: Vec<String>,
    ) {
        self.output_tokens = output_tokens;
        self.tool_uses = tool_uses;
        self.recent_activity = recent_activity;
    }
}

/// What one call came back with: the two facts the protocol keeps, and the two
/// the console shows.
///
/// One type for both halves of a page (D132). The committed history reads it out
/// of the `tool_result` block; a run still going gets it from `ToolCallDone`.
/// They are the same fact, so a page cannot render them two different ways.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolAnswer {
    pub output: String,
    pub is_error: bool,
}

/// Session-level instance registry (Session holds the Arc; shared by child sessions).
/// A single lock carries the state machine + inbox: the check-and-claim of delivery
/// (deposit/deliver) and turn finalization (finish) happen atomically under one lock,
/// so no wakeup is ever lost.
pub struct AgentRegistry {
    inner: Mutex<HashMap<String, Entry>>,
    /// Share persistence (Option semantics: behavior is unchanged when not attached; once attached, insert/finish/stop sync snapshots).
    share: Mutex<Option<Arc<crate::share::ShareStore>>>,
    /// Permission prompt of the session that owns the UI. Subagents have none of their own, so
    /// they borrow this one; the registry is the single place every spawn path can reach it from
    /// (the Agent tool, channel delivery, and the TUI channel room alike).
    ask: Mutex<Option<Arc<crate::query::AskFn>>>,
    /// The console's event channel, attached by whichever front end owns the
    /// screen. Every spawn path reaches a run's UI through the registry already
    /// (`ask`), and a run's stream is the same kind of borrowing: the instance
    /// has no surface of its own, so it writes onto the surface that does.
    events: Mutex<Option<crate::ui::EventSink>>,
    /// Monotonic message id source (registry-wide, so ids never collide across instances).
    next_msg: std::sync::atomic::AtomicU64,
    /// Inbox generation: every accepted item advances this watch channel. Receivers wait on it
    /// between tool rounds so a busy agent does not depend on the sender reaching a boundary.
    inbox_tx: tokio::sync::watch::Sender<u64>,
}

impl AgentRegistry {
    pub fn new() -> Arc<Self> {
        let (inbox_tx, _) = tokio::sync::watch::channel(0);
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            share: Mutex::new(None),
            ask: Mutex::new(None),
            events: Mutex::new(None),
            next_msg: std::sync::atomic::AtomicU64::new(1),
            inbox_tx,
        })
    }

    pub fn subscribe_inbox(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inbox_tx.subscribe()
    }

    fn notify_inbox(&self) {
        self.inbox_tx.send_modify(|generation| *generation += 1);
    }

    fn mint_msg_id(&self) -> MsgId {
        MsgId(
            self.next_msg
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Replace share persistence for future instance changes.
    pub fn attach_share(&self, store: Arc<crate::share::ShareStore>) {
        *self.share.lock().unwrap_or_else(|e| e.into_inner()) = Some(store);
    }

    pub fn detach_share(&self) {
        *self.share.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    #[cfg(test)]
    pub fn has_share(&self) -> bool {
        self.share
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_some()
    }

    /// Attach the prompt surface subagents borrow (called once by whoever owns the UI).
    pub fn attach_ask(&self, ask: Arc<crate::query::AskFn>) {
        *self.ask.lock().unwrap_or_else(|e| e.into_inner()) = Some(ask);
    }

    pub fn ask_fn(&self) -> Option<Arc<crate::query::AskFn>> {
        self.ask.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Attach the console's event channel (the front end does this once).
    pub fn set_events(&self, events: crate::ui::EventSink) {
        *self.events.lock().unwrap_or_else(|e| e.into_inner()) = Some(events);
    }

    /// A sink bound to `name`'s conversation, or `None` with no front end
    /// attached — an embedded or headless run, whose turns nobody is watching.
    pub fn sink_for(&self, name: &str) -> Option<crate::ui::EventSink> {
        self.events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|sink| sink.bound_to(crate::ui::ConvKey::Agent(name.to_string())))
    }

    /// Write an instance's latest snapshot into the share document (no-op without a store).
    fn sync_share(&self, name: &str) {
        let Some(store) = self.share.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return;
        };
        let inner = self.lock();
        let Some(entry) = inner.get(name) else {
            return;
        };
        store.upsert_agent(
            name,
            entry.def.clone(),
            entry.description.clone(),
            entry.state,
            entry.history.clone(),
        );
        store.persist();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Claim an instance name: use the base name when free, otherwise append `-2`/`-3`…
    /// (so parallel same-name instances stay distinguishable).
    /// `main`/`user` are reserved for main and the user (channel member names),
    /// `all` for the broadcast mention token, and none are ever handed out.
    pub fn claim_name(&self, base: &str) -> String {
        let base = if base.trim().is_empty() {
            "agent"
        } else {
            base.trim()
        };
        let taken = |inner: &HashMap<String, Entry>, name: &str| {
            name == crate::channels::MAIN_NAME
                || name == crate::channels::USER_NAME
                || name == crate::channels::ALL_NAME
                || inner.contains_key(name)
        };
        let inner = self.lock();
        if !taken(&inner, base) {
            return base.to_string();
        }
        let mut n = 2;
        loop {
            let candidate = format!("{base}-{n}");
            if !taken(&inner, &candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Register a new instance (state=Running). The name must first go through claim_name.
    pub fn insert(
        &self,
        name: &str,
        kind: AgentKind,
        def: Option<String>,
        description: String,
        session: Arc<Session>,
    ) {
        self.lock().insert(
            name.to_string(),
            Entry {
                def,
                description,
                prompt: String::new(),
                state: AgentState::Running,
                kind,
                last_active: Instant::now(),
                lease: HIRE_LEASE,
                history: Vec::new(),
                stamps: Vec::new(),
                inbox: Vec::new(),
                in_flight: Vec::new(),
                acks: Vec::new(),
                session,
                abort: None,
                runs: 0,
                watch_id: None,
                progress: None,
            },
        );
        self.sync_share(name);
    }

    /// Re-point an existing instance at a freshly built session: the definition on disk
    /// moved, and the instance picks it up without losing what it has already done (D69).
    ///
    /// The swap is the whole mechanism. A turn takes its session at wake
    /// ([`AgentRegistry::flush_pending`]), while the history, the inbox, the ack trail and
    /// the run count live on the entry beside it — replacing the one leaves the others
    /// alone, which is exactly "new prompt, same memory". Before this, the only way to
    /// re-read a member's definition was to delete the instance, which threw its past away
    /// with it.
    ///
    /// A running instance is left alone: its turn is already holding the old session, and
    /// swapping under it would be a persona change mid-sentence. A stopped one comes back
    /// idle — `/team stop` promises start brings it back, and a definition that can never
    /// run again is not one that was refreshed.
    pub fn refresh(
        &self,
        name: &str,
        def: Option<String>,
        description: String,
        session: Arc<Session>,
    ) -> Refresh {
        let outcome = {
            let mut inner = self.lock();
            let Some(entry) = inner.get_mut(name) else {
                return Refresh::Missing;
            };
            if entry.state == AgentState::Running {
                return Refresh::Busy;
            }
            if entry.kind != AgentKind::Crew {
                return Refresh::Hired;
            }
            let changed = entry.def != def
                || entry.description != description
                || !same_definition(&entry.session, &session);
            if changed {
                entry.def = def;
                entry.description = description;
                entry.session = session;
            }
            if entry.state == AgentState::Stopped {
                entry.state = AgentState::Idle;
            }
            entry.last_active = Instant::now();
            if changed {
                Refresh::Refreshed
            } else {
                Refresh::Unchanged
            }
        };
        self.sync_share(name);
        outcome
    }

    /// Release the hires whose task is done, returning the names taken away.
    ///
    /// Only fires while a crew member is actually up: a hire is "temporary" relative to a
    /// standing crew, and in a project with none, an ad-hoc subagent is the ordinary way to
    /// work — sweeping those would delete instances main still expects to address.
    ///
    /// Done means the instance is idle with nothing waiting: no inbox, no message main is
    /// still owed an answer to, and at least one run behind it (a hire the loop has not
    /// picked up yet is not finished, it is unstarted). A hire main stopped is released on
    /// the spot — it will never run again, and holding the name serves nobody.
    pub fn release_hires(&self) -> Vec<String> {
        let mut inner = self.lock();
        if !inner
            .values()
            .any(|e| e.kind == AgentKind::Crew && e.state != AgentState::Stopped)
        {
            return Vec::new();
        }
        let mut released = Vec::new();
        inner.retain(|name, e| {
            if e.kind != AgentKind::Hire {
                return true;
            }
            if e.state == AgentState::Stopped {
                released.push(name.clone());
                return false;
            }
            let waiting = e.state == AgentState::Running
                || !e.inbox.is_empty()
                || e.acks.iter().any(|a| a.state.is_outstanding())
                || e.runs == 0;
            if waiting {
                e.lease = HIRE_LEASE;
                return true;
            }
            e.lease = e.lease.saturating_sub(1);
            if e.lease > 0 {
                return true;
            }
            released.push(name.clone());
            false
        });
        released.sort();
        released
    }

    pub fn set_prompt(&self, name: &str, prompt: String) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.prompt = prompt;
        }
    }

    pub fn set_progress(&self, name: &str, progress: Option<Arc<Mutex<AgentProgress>>>) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.progress = progress;
        }
    }

    #[cfg(test)]
    pub fn set_progress_snapshot(&self, name: &str, progress: AgentProgress) {
        self.set_progress(name, Some(Arc::new(Mutex::new(progress))));
    }

    pub fn touch(&self, name: &str) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.last_active = Instant::now();
        }
    }

    /// Instance view data (None if the instance doesn't exist).
    pub fn view_of(&self, name: &str) -> Option<AgentView> {
        let inner = self.lock();
        let entry = inner.get(name)?;
        Some((entry.history.clone(), entry.stamps.clone(), entry.state))
    }

    /// Whether an instance belongs to the given project directory.
    pub fn is_in_project(&self, name: &str, cwd: &Path) -> bool {
        self.lock()
            .get(name)
            .is_some_and(|entry| entry.session.cwd() == cwd)
    }

    /// Instance depth (channel cohort check: only direct subagents with depth==1 may join a channel).
    pub fn depth_of(&self, name: &str) -> Option<usize> {
        self.lock().get(name).map(|e| e.session.depth)
    }

    /// The permission mode this instance runs under (D105).
    ///
    /// Inherited from the parent at spawn (`tool::agent`) and cycled per
    /// instance from its zoomed view, the way CC cycles a viewed teammate's own
    /// mode and leaves the leader's alone (`PromptInput.tsx:1410-1447`; the
    /// field is declared "cycled independently via Shift+Tab when viewing",
    /// `InProcessTeammateTask/types.ts:44`).
    pub fn permission_mode_of(&self, name: &str) -> Option<crate::permission::PermissionMode> {
        self.lock().get(name).map(|e| e.session.permission_mode)
    }

    /// Point an instance at a session carrying `mode`, and say whether that
    /// changed anything.
    ///
    /// `Session` is immutable inside its `Arc`, so this is the same derive-a-copy
    /// move the console makes for its own turns (`Chat::session_for_turn`): every
    /// other field is a shared handle, so the registries, the watch board and the
    /// task store still point at the same state. The run **in flight** captured
    /// the old `Arc` and keeps its mode; the next one — a wake, a resume, a
    /// follow-up — reads this.
    pub fn set_permission_mode(&self, name: &str, mode: crate::permission::PermissionMode) -> bool {
        let mut inner = self.lock();
        let Some(entry) = inner.get_mut(name) else {
            return false;
        };
        if entry.session.permission_mode == mode {
            return false;
        }
        let mut session = (*entry.session).clone();
        session.permission_mode = mode;
        entry.session = Arc::new(session);
        true
    }

    /// The session an instance runs on.
    ///
    /// One production caller, and it is the reason the door opened (D135): the
    /// console's `/compact` on an instance's page summarises *that* instance's
    /// context, which means the model, the provider and — crucially — the
    /// transcript of that instance rather than the console's. Compacting
    /// through the console's own session would append the marker to the
    /// console's transcript, which is the wrong record.
    pub fn session_of(&self, name: &str) -> Option<Arc<Session>> {
        self.lock().get(name).map(|e| e.session.clone())
    }

    /// Replace an instance's stored context with a rewritten one — `/compact`
    /// on its page, and nothing else.
    ///
    /// **Refused while it is running**, and the refusal is the point: a turn in
    /// flight carries its own copy of the history and writes it back at
    /// [`AgentRegistry::finish`], so a summary spliced in underneath would be
    /// overwritten by the very next round. The check reads the state under the
    /// same lock the write takes, so a run that starts between the console's
    /// look and its write loses the race rather than the work.
    pub fn replace_history(&self, name: &str, history: Vec<Message>) -> bool {
        let replaced = {
            let mut inner = self.lock();
            let Some(entry) = inner.get_mut(name) else {
                return false;
            };
            if entry.state == AgentState::Running {
                return false;
            }
            // `finish`'s rule: a shorter history was rewritten and the old
            // clocks no longer describe it — better no stamp than a wrong one.
            let refill = if history.len() < entry.stamps.len() {
                entry.stamps.clear();
                0
            } else {
                crate::channels::now_unix()
            };
            entry.stamps.resize(history.len(), refill);
            entry.history = history;
            entry.last_active = Instant::now();
            true
        };
        if replaced {
            self.sync_share(name);
        }
        replaced
    }

    pub fn set_abort_if_running(
        &self,
        name: &str,
        run: u64,
        abort: tokio::task::AbortHandle,
        items: Vec<InboxItem>,
    ) -> bool {
        let mut inner = self.lock();
        let Some(entry) = inner.get_mut(name) else {
            abort.abort();
            return false;
        };
        if entry.state != AgentState::Running {
            abort.abort();
            for item in &items {
                if let InboxItem::Direct { id, .. } = item {
                    entry.in_flight.retain(|(flying, ..)| flying != id);
                    if let Some(ack) = entry.acks.iter_mut().find(|ack| ack.id == *id) {
                        ack.state = AckState::Dropped {
                            reason: "instance stopped".to_string(),
                        };
                        ack.delivered_after_chars = None;
                    }
                }
            }
            return false;
        }
        if entry.runs != run {
            return false;
        }
        entry.abort = Some(abort);
        true
    }

    /// Next run sequence number (starting at 1).
    pub fn next_run(&self, name: &str) -> u64 {
        match self.lock().get_mut(name) {
            Some(entry) => {
                entry.runs += 1;
                entry.last_active = Instant::now();
                entry.runs
            }
            None => 1,
        }
    }

    /// Record the watch line of the current turn.
    pub fn set_run_watch(&self, name: &str, id: crate::watch::WatchId) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.watch_id = Some(id);
        }
    }

    /// Turn finished: store the latest history. Inbox non-empty → stay Running and
    /// return (history copy, drained inbox); empty → switch to Idle.
    /// Stopped (stopped mid-turn) never revives and never returns a continuation.
    ///
    /// `output_chars` is the final amount of text produced by this query. A message is answered
    /// only when text was produced after it entered the query; earlier prose cannot acknowledge a
    /// later instruction.
    pub fn finish(
        &self,
        name: &str,
        history: Vec<Message>,
        output_chars: usize,
    ) -> Option<Continuation> {
        let result = {
            let mut inner = self.lock();
            let entry = inner.get_mut(name)?;
            entry.last_active = Instant::now();
            // Stamp the new tail with now: history normally only grows. Shorter
            // means it was rewritten (compaction) and the old clocks no longer
            // describe it — better no stamp than a wrong one.
            let refill = if history.len() < entry.stamps.len() {
                entry.stamps.clear();
                0
            } else {
                crate::channels::now_unix()
            };
            entry.stamps.resize(history.len(), refill);
            entry.history = history;
            // The stored history now carries the run's messages: the bridge record
            // has done its job (the continuation drain below refills it).
            entry.in_flight.clear();
            answer_acks(entry, output_chars);
            if entry.state == AgentState::Stopped {
                None
            } else if !inbox_wakes(entry) {
                entry.state = AgentState::Idle;
                None
            } else {
                entry.runs += 1;
                let items = drain_inbox(entry, entry.runs, 0);
                entry.state = AgentState::Running;
                Some(Continuation {
                    history: entry.history.clone(),
                    items,
                    run: entry.runs,
                })
            }
        };
        self.sync_share(name);
        result
    }

    /// Claim every idle instance holding mail (v7): a non-empty inbox wakes its
    /// holder and nothing else does. The caller starts each returned run;
    /// draining the inbox in one pass makes the batch boundary the receiver's
    /// actual claim point.
    pub fn flush_pending(&self) -> Vec<Wake> {
        let mut woken = Vec::new();
        {
            let mut inner = self.lock();
            for (name, entry) in inner.iter_mut() {
                if entry.state != AgentState::Idle || !inbox_wakes(entry) {
                    continue;
                }
                entry.runs += 1;
                let items = drain_inbox(entry, entry.runs, 0);
                entry.state = AgentState::Running;
                entry.last_active = Instant::now();
                woken.push(Wake {
                    name: name.clone(),
                    session: entry.session.clone(),
                    history: entry.history.clone(),
                    items,
                    run: entry.runs,
                });
            }
        }
        for wake in &woken {
            self.sync_share(&wake.name);
        }
        woken
    }

    /// Put a direct message into one running instance's current query. Unlike a new run, this
    /// keeps the run number and records the output offset at which the message entered.
    pub fn take_running(&self, name: &str, output_chars: usize) -> Vec<InboxItem> {
        let items = {
            let mut inner = self.lock();
            let Some(entry) = inner.get_mut(name) else {
                return Vec::new();
            };
            if entry.state != AgentState::Running || entry.inbox.is_empty() {
                return Vec::new();
            }
            // v7: a running member absorbs its whole inbox at the tool
            // boundary — steering, at input-token cost and no model call.
            // v6 took only mention-bearing batches, and had to take every
            // queued line with them anyway to keep the seen cursor honest.
            let items = drain_inbox(entry, entry.runs, output_chars);
            if items.is_empty() {
                return Vec::new();
            }
            items
        };
        self.sync_share(name);
        items
    }

    /// Turn failed before the claimed batch completed: restore it to the front of the inbox and
    /// make its direct-message receipts queued again so the recovery dispatcher can retry it.
    pub fn restore_inbox(&self, name: &str, mut items: Vec<InboxItem>) {
        if items.is_empty() {
            return;
        }
        let mut inner = self.lock();
        let Some(entry) = inner.get_mut(name) else {
            return;
        };
        if entry.state == AgentState::Stopped {
            for item in &items {
                if let InboxItem::Direct { id, .. } = item {
                    entry.in_flight.retain(|(flying, ..)| flying != id);
                    if let Some(ack) = entry.acks.iter_mut().find(|ack| ack.id == *id) {
                        ack.state = AckState::Dropped {
                            reason: "instance stopped".to_string(),
                        };
                        ack.delivered_after_chars = None;
                    }
                }
            }
            return;
        }
        for item in &items {
            if let InboxItem::Direct { id, .. } = item {
                // Back in the inbox, back to the pending view — a message rendered
                // both as sent and as queued would be on screen twice.
                entry.in_flight.retain(|(flying, ..)| flying != id);
                if let Some(ack) = entry.acks.iter_mut().find(|ack| ack.id == *id) {
                    ack.state = AckState::Queued;
                    ack.delivered_after_chars = None;
                }
            }
        }
        items.append(&mut entry.inbox);
        entry.inbox = items;
        drop(inner);
        self.notify_inbox();
    }

    /// A claimed idle run may be stopped before its task is spawned. Re-checking under the
    /// registry lock closes that gap without exposing the entry or holding the lock across spawn.
    pub fn accepts_run(&self, name: &str, run: u64) -> bool {
        self.lock()
            .get(name)
            .is_some_and(|entry| entry.state == AgentState::Running && entry.runs == run)
    }

    /// Turn failed: keep the pre-failure history, switch to Idle (retryable via SendMessage).
    pub fn mark_idle(&self, name: &str) {
        if let Some(entry) = self.lock().get_mut(name)
            && entry.state != AgentState::Stopped
        {
            entry.state = AgentState::Idle;
            entry.last_active = Instant::now();
        }
    }

    /// Queue a main command. Returns the message id — the receipt the sender uses to check the
    /// outcome later. Idle instances are claimed by the immediate dispatcher; running instances
    /// absorb the batch between tool rounds. `ack_timeout` records the wait the sender allowed
    /// before the acknowledgement is chased (see `follow_up`); it is a note on the record, not a
    /// timer — the caller owns the clock.
    pub fn deliver(
        &self,
        name: &str,
        from: &str,
        message: &str,
        images: Vec<crate::api::types::ImageAttachment>,
        ack_timeout: Option<Duration>,
    ) -> Result<MsgId, String> {
        let id = self.mint_msg_id();
        let mut inner = self.lock();
        let Some(entry) = inner.get_mut(name) else {
            let known: Vec<String> = inner.keys().cloned().collect();
            return Err(if known.is_empty() {
                format!("no subagent named {name} (there are no instances right now)")
            } else {
                format!(
                    "no subagent named {name}; existing instances: {}",
                    known.join(", ")
                )
            });
        };
        if entry.state == AgentState::Stopped {
            // CC subagent semantics (v4): a direct message after a stop resumes
            // the instance. Its session and history never left the registry, so
            // waking is the same move an idle instance makes — flip to Idle here
            // and the flush that follows every delivery spawns the run. Only
            // this door resumes: follow-up chases push without flipping state,
            // and a room broadcast skips stopped members, so nothing automatic
            // undoes a stop the user asked for.
            entry.state = AgentState::Idle;
        }
        entry.last_active = Instant::now();
        entry.inbox.push(InboxItem::Direct {
            id,
            from: from.to_string(),
            text: message.to_string(),
            images,
        });
        entry.lease = HIRE_LEASE;
        push_ack(
            entry,
            Ack {
                id,
                excerpt: first_line(message),
                state: AckState::Queued,
                queued_at: Instant::now(),
                timeout: ack_timeout,
                follow_ups: 0,
                delivered_after_chars: None,
            },
        );
        drop(inner);
        // The receiver's page shows what it was told, when it was told (D135).
        // A running instance absorbs its mail at its next tool barrier, so
        // without this a user watching one could not see what main had just
        // asked for until the run got round to reading it. Every sender comes
        // through here, so this is where the echo belongs; the absorbed prompt
        // repeats it and the console drops the repeat.
        if let Some(sink) = self.sink_for(name) {
            sink.send(crate::ui::UiEvent::Mail {
                from: from.to_string(),
                text: message.to_string(),
            });
        }
        self.notify_inbox();
        Ok(id)
    }

    /// Re-read one message's record and, while the sender is still owed an answer, put a follow-up
    /// in the receiver's inbox. Reading and enqueueing happen under the single registry lock, so a
    /// turn ending mid-check can never turn a just-answered message into a pointless nudge.
    pub fn follow_up(&self, name: &str, id: MsgId) -> FollowUp {
        let mut inner = self.lock();
        let Some(entry) = inner.get_mut(name) else {
            return FollowUp::Gone;
        };
        let Some(ack) = entry.acks.iter_mut().find(|a| a.id == id) else {
            return FollowUp::Gone;
        };
        if !ack.state.is_outstanding() {
            return FollowUp::Settled(ack.state.clone());
        }
        if ack.follow_ups >= MAX_FOLLOW_UPS {
            return FollowUp::Exhausted;
        }
        ack.follow_ups += 1;
        let round = ack.follow_ups;
        let excerpt = ack.excerpt.clone();
        let waited = ack.queued_at.elapsed();
        let delivered = matches!(ack.state, AckState::Delivered { .. });
        entry.inbox.push(InboxItem::FollowUp {
            original: id,
            round,
            excerpt,
            waited,
            delivered,
        });
        drop(inner);
        self.notify_inbox();
        FollowUp::Sent { round }
    }

    /// Queue a channel message. A stopped member is silently skipped — a broadcast doesn't fail
    /// because one member stopped. Returns whether it was accepted.
    pub fn deposit(&self, name: &str, item: InboxItem) -> bool {
        {
            let mut inner = self.lock();
            let Some(entry) = inner.get_mut(name) else {
                return false;
            };
            if entry.state == AgentState::Stopped {
                return false;
            }
            entry.last_active = Instant::now();
            entry.inbox.push(item);
        }
        // Every deposit pulses (v7). v6 pulsed only for a mention, because an
        // unmentioned line was on a batch clock it must not jump; D129 deleted
        // that clock, and the leftover condition was the last place the runtime
        // still read the `@` as a wake bit rather than as an obligation.
        self.notify_inbox();
        true
    }

    /// Delivery records for one instance, newest last (None = no such instance).
    pub fn acks_of(&self, name: &str) -> Option<Vec<Ack>> {
        Some(self.lock().get(name)?.acks.clone())
    }

    /// Direct messages still sitting in the inbox, in order, each with its
    /// sender.
    ///
    /// It had one production reader, the away page's echo of a message not yet
    /// claimed. D134 replaced that for the *user's* own sends, which the console
    /// echoes at the moment it delivers them — the same moment main's prompt
    /// enters main's transcript. Main's dispatches and mail get no echo, so a
    /// user watching a busy instance cannot see what main just asked it until
    /// the run absorbs it at a barrier; that is a loss, named here rather than
    /// papered over, and D135 is where the send paths merge and can close it.
    /// What is left is the delivery assertion the tests make.
    /// Direct messages claimed by the current run and not yet landed in
    /// history. The registry's own bookkeeping, asserted here for the same
    /// reason [`AgentRegistry::pending_of`] is.
    #[cfg(test)]
    pub fn in_flight_of(&self, name: &str) -> Vec<(String, String)> {
        self.lock()
            .get(name)
            .map(|entry| {
                entry
                    .in_flight
                    .iter()
                    .map(|(_, from, text)| (from.clone(), text.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub fn pending_of(&self, name: &str) -> Vec<(String, String)> {
        self.lock()
            .get(name)
            .map(|entry| {
                entry
                    .inbox
                    .iter()
                    .filter_map(|item| match item {
                        InboxItem::Direct { from, text, .. } => Some((from.clone(), text.clone())),
                        // A follow-up is the harness chasing an acknowledgement, not something
                        // the sender wrote — rendering it as their pending message would lie.
                        InboxItem::Channel { .. }
                        | InboxItem::FollowUp { .. }
                        | InboxItem::Unanswered { .. } => None,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Stop: abort a running turn (abort), no longer accept commands; history is kept
    /// and listable. Returns the watch line of the aborted turn (the caller sets
    /// Cancelled); when idle/already stopped there is no active line, returns None (idempotent).
    /// Stopping discards the inbox, so every message still in it is recorded as dropped: a
    /// sender that only ever saw "queued" must be able to find out it was never delivered.
    /// Returns the watch line and how many messages died with it.
    pub fn stop(&self, name: &str) -> Result<(Option<crate::watch::WatchId>, usize), String> {
        let result = {
            let mut inner = self.lock();
            let Some(entry) = inner.get_mut(name) else {
                return Err(format!("no subagent named {name}"));
            };
            if entry.state == AgentState::Stopped {
                (None, 0)
            } else {
                let was_running = entry.state == AgentState::Running;
                entry.state = AgentState::Stopped;
                let dropped = mark_inbox_dropped(entry, "instance stopped");
                if let Some(abort) = entry.abort.take() {
                    abort.abort();
                }
                let id = if was_running { entry.watch_id } else { None };
                (id, dropped)
            }
        };
        self.sync_share(name);
        Ok(result)
    }

    /// Remove: stop first, then drop the entry (name released). The ack trail goes with it, so
    /// the dropped count is the sender's last chance to learn what was lost.
    pub fn remove(&self, name: &str) -> Result<(Option<crate::watch::WatchId>, usize), String> {
        let outcome = self.stop(name)?;
        self.lock().remove(name);
        Ok(outcome)
    }

    /// Snapshot of all instances (sorted by name for stable list output).
    pub fn list(&self) -> Vec<AgentStatus> {
        let inner = self.lock();
        let mut out: Vec<AgentStatus> = inner
            .iter()
            .map(|(name, e)| {
                let progress = e.progress.as_ref().map(|progress| {
                    progress
                        .lock()
                        .unwrap_or_else(|err| err.into_inner())
                        .clone()
                });
                AgentStatus {
                    name: name.clone(),
                    def: e.def.clone(),
                    description: e.description.clone(),
                    prompt: e.prompt.clone(),
                    state: e.state,
                    kind: e.kind,
                    pending: e.inbox.len(),
                    unacked: e.acks.iter().filter(|a| a.state.is_outstanding()).count(),
                    model: e.session.runtime.model.borrow().clone(),
                    provider: e.session.runtime.provider.borrow().clone(),
                    elapsed: progress
                        .as_ref()
                        .and_then(|p| p.started_at.map(|started| started.elapsed())),
                    output_tokens: progress.as_ref().map_or(0, |p| p.output_tokens),
                    tool_uses: progress.as_ref().map_or(0, |p| p.tool_uses),
                    recent_activity: progress.map_or_else(Vec::new, |p| p.recent_activity),
                    last_active: e.last_active,
                }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// Do two sessions put the same instance in front of the model — the words it runs under
/// and the engine it runs on?
///
/// Compared over the built session rather than the definition file, because the file is
/// only one of the inputs: the blueprint's per-member overrides, the crew's working
/// agreement and the parent session's own model all reach the instance too, and what the
/// member actually runs with is the thing that must not change without saying so. History
/// and inbox are deliberately not part of it — they are what a refresh preserves.
fn same_definition(a: &Session, b: &Session) -> bool {
    a.system.len() == b.system.len()
        && a.system
            .iter()
            .zip(b.system.iter())
            .all(|(x, y)| x.text == y.text && x.cache == y.cache)
        && *a.runtime.model.borrow() == *b.runtime.model.borrow()
        && *a.runtime.provider.borrow() == *b.runtime.provider.borrow()
        && *a.runtime.thinking.borrow() == *b.runtime.thinking.borrow()
        && a.cwd() == b.cwd()
}

/// A reply answers a delivered message only if text was produced after that message entered the
/// query. Anything still queued is untouched: it has not been read yet.
fn answer_acks(entry: &mut Entry, output_chars: usize) {
    let run = entry.runs;
    for ack in entry.acks.iter_mut() {
        if let AckState::Delivered { run: delivered_run } = ack.state
            && (run > delivered_run && output_chars > 0
                || run == delivered_run
                    && ack
                        .delivered_after_chars
                        .is_some_and(|before| output_chars > before))
        {
            ack.state = AckState::Answered { run };
        }
    }
}

fn mark_delivered(entry: &mut Entry, items: &[InboxItem], run: u64, output_chars: usize) {
    for item in items {
        if let InboxItem::Direct { id, from, text, .. } = item {
            // Delivered into a run means gone from the inbox but not yet in the
            // history: record it so the DM keeps showing what was sent.
            entry.in_flight.push((*id, from.clone(), text.clone()));
            if let Some(ack) = entry.acks.iter_mut().find(|ack| ack.id == *id) {
                ack.state = AckState::Delivered { run };
                ack.delivered_after_chars = Some(output_chars);
            }
        }
    }
}

fn drain_inbox(entry: &mut Entry, run: u64, output_chars: usize) -> Vec<InboxItem> {
    let items = std::mem::take(&mut entry.inbox);
    mark_delivered(entry, &items, run, output_chars);
    items
}

/// v7: a non-empty inbox wakes its holder, and that is the whole rule.
///
/// v6 gated *reading* on a count and an age because the runtime could not tell
/// whether a line mattered — but the member who wrote it could, and now says so
/// with the `@` (`CHANNEL_NOTE`'s R1). The sigil decides what is **owed**;
/// nothing decides what is read. Two things the gate bought are kept: an empty
/// inbox never wakes, so a quiet room costs no model call and nothing polls,
/// and the predicate is still one function consulted by every door.
fn inbox_wakes(entry: &Entry) -> bool {
    !entry.inbox.is_empty()
}

/// Record every still-queued inbox message as dropped; returns how many.
fn mark_inbox_dropped(entry: &mut Entry, reason: &str) -> usize {
    let items = std::mem::take(&mut entry.inbox);
    let mut dropped = 0;
    for item in &items {
        if let InboxItem::Direct { id, .. } = item {
            set_ack(
                entry,
                *id,
                AckState::Dropped {
                    reason: reason.to_string(),
                },
            );
            dropped += 1;
        }
    }
    dropped
}

fn set_ack(entry: &mut Entry, id: MsgId, state: AckState) {
    if let Some(ack) = entry.acks.iter_mut().find(|a| a.id == id) {
        ack.state = state;
    }
}

fn push_ack(entry: &mut Entry, ack: Ack) {
    entry.acks.push(ack);
    if entry.acks.len() > MAX_ACKS {
        let overflow = entry.acks.len() - MAX_ACKS;
        entry.acks.drain(..overflow);
    }
}

/// First line, bounded — enough to recognize a message in a listing.
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    let cut: String = line.chars().take(40).collect();
    if cut.chars().count() < line.chars().count() {
        format!("{cut}…")
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn test_session() -> Arc<Session> {
        Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "http://x".into()),
            runtime: crate::query::Runtime::new("m".into(), None, Default::default()),
            permission_mode: crate::permission::PermissionMode::Default,
            settings: crate::settings::Settings::default(),
            system: Vec::new(),
            depth: 1,
            cwd: Arc::new(std::sync::Mutex::new(std::env::temp_dir())),
            home: std::env::temp_dir(),
            user_config_dir: std::env::temp_dir().join(".config"),
            quiet: true,
            compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "t")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
        })
    }

    #[test]
    fn list_reports_the_runtime_engine() {
        let registry = AgentRegistry::new();
        let session = test_session();
        let _ = session.runtime.model_tx.send("gpt-5.6-sol".into());
        let _ = session.runtime.provider_tx.send("road".into());
        registry.insert(
            "dev",
            AgentKind::Hire,
            None,
            "implementation".into(),
            session,
        );

        let statuses = registry.list();
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].model, "gpt-5.6-sol");
        assert_eq!(statuses[0].provider, "road");
    }

    #[test]
    fn loads_defs_with_project_over_user_precedence() {
        let root = std::env::temp_dir().join(format!("bingo-agents-{}-load", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let project = root.join("project");
        write(
            &home.join(".config/bingo/agents/reviewer.md"),
            "---\ndescription: user reviewer\nmodel: haiku\n---\nYou are the reviewer.\n",
        );
        write(
            &project.join(".bingo/agents/reviewer.md"),
            "---\ndescription: project reviewer\n---\nYou are the project reviewer.\n",
        );
        write(&project.join(".bingo/agents/scout.md"), "For research.\n");
        let defs = load_agent_defs(&home, &project);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["reviewer", "scout"],
            "the project layer overrides the user layer for same names"
        );
        let reviewer = &defs[0];
        assert_eq!(reviewer.description, "project reviewer");
        assert!(reviewer.system.contains("project reviewer"));
        assert!(
            reviewer.model.is_none(),
            "the overridden user definition does not leak through"
        );
        assert_eq!(
            reviewer.source,
            AgentDefSource::Project,
            "a cross-layer same-name override takes the project source"
        );
        // No frontmatter: name comes from the file name, description falls back to the first body line.
        assert_eq!(defs[1].description, "For research.");
        assert_eq!(defs[1].source, AgentDefSource::Project);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// source=User when only the user layer has a definition (D31 badge data).
    #[test]
    fn source_is_user_when_only_user_layer_has_def() {
        let root = std::env::temp_dir().join(format!("bingo-agents-{}-src", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        write(
            &home.join(".config/bingo/agents/only-user.md"),
            "User-layer only.\n",
        );
        let defs = load_agent_defs(&home, &root);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "only-user");
        assert_eq!(defs[0].source, AgentDefSource::User);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn frontmatter_name_and_model_override() {
        let root = std::env::temp_dir().join(format!("bingo-agents-{}-fm", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        write(
            &home.join(".config/bingo/agents/x.md"),
            "---\nname: deep-dive\ndescription: >-\n  multi-line\n  description\nmodel: sub-model\nprovider: ds\nthinking: xhigh\n---\nsystem body\n",
        );
        let defs = load_agent_defs(&home, &root);
        assert_eq!(defs.len(), 1);
        assert_eq!(
            defs[0].name, "deep-dive",
            "frontmatter name overrides the file name"
        );
        assert_eq!(
            defs[0].description, "multi-line description",
            "folded scalar"
        );
        assert_eq!(defs[0].model.as_deref(), Some("sub-model"));
        assert_eq!(defs[0].provider.as_deref(), Some("ds"));
        assert_eq!(defs[0].thinking.as_deref(), Some("xhigh"));
        assert_eq!(defs[0].system, "system body");
        assert!(
            defs[0].inherit_system,
            "defaults to appending to the parent system"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// `inherit_system: false` opts into replacing the parent's system blocks; anything else
    /// (including a typo) keeps the safe default.
    #[test]
    fn frontmatter_inherit_system_opt_out() {
        let root =
            std::env::temp_dir().join(format!("bingo-agents-{}-inherit", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        write(
            &home.join(".config/bingo/agents/lean.md"),
            "---\nname: lean\ninherit_system: false\n---\npersona only\n",
        );
        write(
            &home.join(".config/bingo/agents/keep.md"),
            "---\nname: keep\ninherit_system: yes\n---\nappended as usual\n",
        );
        let defs = load_agent_defs(&home, &root);
        let by = |n: &str| defs.iter().find(|d| d.name == n).unwrap().inherit_system;
        assert!(!by("lean"));
        assert!(by("keep"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn claim_name_dedupes_and_defaults() {
        let reg = AgentRegistry::new();
        assert_eq!(reg.claim_name(""), "agent", "empty name falls back");
        assert_eq!(reg.claim_name("reviewer"), "reviewer");
        reg.insert(
            "reviewer",
            AgentKind::Hire,
            None,
            "r".into(),
            test_session(),
        );
        assert_eq!(reg.claim_name("reviewer"), "reviewer-2");
        reg.insert(
            "reviewer-2",
            AgentKind::Hire,
            None,
            "r".into(),
            test_session(),
        );
        assert_eq!(reg.claim_name("reviewer"), "reviewer-3");
    }

    /// A hire serves one task and goes (D53): once it is idle with nothing waiting, the
    /// lease runs out and the name is released. The crew is never touched, and main gets
    /// one round to follow up before the instance disappears under it.
    #[test]
    fn a_finished_hire_is_released_and_the_crew_is_not() {
        let reg = AgentRegistry::new();
        reg.insert(
            "dev",
            AgentKind::Crew,
            None,
            "member".into(),
            test_session(),
        );
        reg.insert(
            "temp",
            AgentKind::Hire,
            None,
            "one job".into(),
            test_session(),
        );
        // Mirrors the real spawn: both Agent-tool paths call next_run right after insert,
        // and a hire with no run behind it is unstarted, not finished.
        let _ = reg.next_run("temp");

        // Running: nothing is released, however many sweeps run.
        assert!(reg.release_hires().is_empty());
        assert!(reg.release_hires().is_empty());
        assert_eq!(reg.list().len(), 2, "a working hire keeps its name");

        // Finished: idle, empty inbox, nothing owed. One sweep is not enough — that would
        // take the instance away in the very round its result reaches main.
        assert!(reg.finish("temp", Vec::new(), 1).is_none());
        assert!(
            reg.release_hires().is_empty(),
            "main still has a round to follow up in"
        );
        assert_eq!(reg.release_hires(), vec!["temp".to_string()]);
        let left = reg.list();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].name, "dev", "the crew member is untouched");
        assert_eq!(left[0].kind, AgentKind::Crew);
    }

    /// A follow-up renews the lease: the hire main is still talking to is not swept out
    /// from under the conversation.
    #[test]
    fn a_hire_with_work_waiting_keeps_its_name() {
        let reg = AgentRegistry::new();
        reg.insert(
            "dev",
            AgentKind::Crew,
            None,
            "member".into(),
            test_session(),
        );
        reg.insert(
            "temp",
            AgentKind::Hire,
            None,
            "one job".into(),
            test_session(),
        );
        // Mirrors the real spawn: both Agent-tool paths call next_run right after insert,
        // and a hire with no run behind it is unstarted, not finished.
        let _ = reg.next_run("temp");
        assert!(reg.finish("temp", Vec::new(), 1).is_none());
        assert!(reg.release_hires().is_empty());

        // A queued follow-up is work waiting: the count goes back to full.
        let _ = reg
            .deliver(
                "temp",
                crate::channels::MAIN_NAME,
                "one more thing",
                Vec::new(),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.release_hires().is_empty());
        assert!(reg.release_hires().is_empty(), "the lease was renewed");

        // Read into a run and answered, with nothing left waiting → released as before.
        let woken = reg.flush_pending();
        assert_eq!(woken.len(), 1);
        assert!(reg.finish("temp", Vec::new(), 1).is_none());
        assert!(reg.release_hires().is_empty());
        assert_eq!(reg.release_hires(), vec!["temp".to_string()]);
    }

    /// A message the hire never answered is not a finished task. Releasing there would
    /// destroy the record the sender uses to find out it was left hanging.
    #[test]
    fn a_hire_still_owing_an_answer_is_not_released() {
        let reg = AgentRegistry::new();
        reg.insert(
            "dev",
            AgentKind::Crew,
            None,
            "member".into(),
            test_session(),
        );
        reg.insert(
            "temp",
            AgentKind::Hire,
            None,
            "one job".into(),
            test_session(),
        );
        // Mirrors the real spawn: both Agent-tool paths call next_run right after insert,
        // and a hire with no run behind it is unstarted, not finished.
        let _ = reg.next_run("temp");
        assert!(reg.finish("temp", Vec::new(), 1).is_none());
        let _ = reg.deliver(
            "temp",
            crate::channels::MAIN_NAME,
            "answer me",
            Vec::new(),
            None,
        );
        assert_eq!(
            reg.flush_pending().len(),
            1,
            "the idle hire takes the message"
        );
        // The run that read it ended saying nothing: delivered, unanswered, inbox empty.
        assert!(reg.finish("temp", Vec::new(), 0).is_none());
        let owed = |reg: &AgentRegistry| {
            reg.list()
                .into_iter()
                .find(|a| a.name == "temp")
                .map(|a| a.unacked)
                .unwrap_or_default()
        };
        assert_eq!(owed(&reg), 1);
        for _ in 0..4 {
            assert!(
                reg.release_hires().is_empty(),
                "an outstanding message holds the instance open"
            );
        }
    }

    /// Without a crew there is nothing for a hire to be temporary *relative to*: an ad-hoc
    /// subagent is the ordinary way to work in such a project, and sweeping it would delete
    /// instances main still expects to address.
    #[test]
    fn hires_are_not_swept_in_a_project_with_no_crew() {
        let reg = AgentRegistry::new();
        reg.insert(
            "temp",
            AgentKind::Hire,
            None,
            "one job".into(),
            test_session(),
        );
        // Mirrors the real spawn: both Agent-tool paths call next_run right after insert,
        // and a hire with no run behind it is unstarted, not finished.
        let _ = reg.next_run("temp");
        assert!(reg.finish("temp", Vec::new(), 1).is_none());
        for _ in 0..4 {
            assert!(reg.release_hires().is_empty());
        }
        assert_eq!(reg.list().len(), 1);

        // A crew that has been stopped is not a crew either.
        reg.insert(
            "dev",
            AgentKind::Crew,
            None,
            "member".into(),
            test_session(),
        );
        let _ = reg.stop("dev");
        for _ in 0..4 {
            assert!(reg.release_hires().is_empty());
        }
    }

    /// A stopped hire will never run again, so it goes on the spot rather than waiting out
    /// a lease that measures a follow-up window it can no longer receive.
    #[test]
    fn a_stopped_hire_is_released_immediately() {
        let reg = AgentRegistry::new();
        reg.insert(
            "dev",
            AgentKind::Crew,
            None,
            "member".into(),
            test_session(),
        );
        reg.insert(
            "temp",
            AgentKind::Hire,
            None,
            "one job".into(),
            test_session(),
        );
        // Mirrors the real spawn: both Agent-tool paths call next_run right after insert,
        // and a hire with no run behind it is unstarted, not finished.
        let _ = reg.next_run("temp");
        let _ = reg.stop("temp");
        assert_eq!(reg.release_hires(), vec!["temp".to_string()]);
        assert_eq!(reg.list().len(), 1);
    }

    #[test]
    fn activity_timestamp_refreshes_only_when_the_instance_is_active() {
        let reg = AgentRegistry::new();
        reg.insert(
            "scout",
            AgentKind::Hire,
            None,
            "research".into(),
            test_session(),
        );
        let initial = reg.list()[0].last_active;
        std::thread::sleep(Duration::from_millis(2));
        let unchanged = reg.list()[0].last_active;
        assert_eq!(unchanged, initial, "listing is not agent activity");

        let first = reg
            .deliver(
                "scout",
                crate::channels::MAIN_NAME,
                "add A",
                Vec::new(),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        let delivered = reg.list()[0].last_active;
        assert!(
            delivered > initial,
            "receiving an inbox message is activity"
        );

        std::thread::sleep(Duration::from_millis(2));
        let _ = reg.follow_up("scout", first);
        assert_eq!(
            reg.list()[0].last_active,
            delivered,
            "watchdog bookkeeping is not agent activity"
        );

        std::thread::sleep(Duration::from_millis(2));
        reg.touch("scout");
        let streamed = reg.list()[0].last_active;
        assert!(
            streamed > delivered,
            "stream and tool hooks can touch the entry"
        );

        std::thread::sleep(Duration::from_millis(2));
        assert!(reg.finish("scout", Vec::new(), 0).is_some());
        let finished = reg.list()[0].last_active;
        assert!(finished > streamed, "turn completion is activity");
    }

    /// Every stored history message carries a landing clock for the DM view;
    /// a rewritten (shorter) history drops the stale clocks instead of lying.
    #[test]
    fn finish_stamps_history_and_drops_clocks_on_rewrite() {
        let reg = AgentRegistry::new();
        reg.insert(
            "scout",
            AgentKind::Hire,
            None,
            "research".into(),
            test_session(),
        );
        assert!(
            reg.finish(
                "scout",
                vec![Message::user_text("a"), Message::user_text("b")],
                0,
            )
            .is_none()
        );
        let (history, stamps, _) = reg.view_of("scout").unwrap_or_else(|| panic!("exists"));
        assert_eq!(stamps.len(), history.len());
        assert!(stamps.iter().all(|&at| at > 0), "{stamps:?}");
        // Compaction hands back a shorter, rewritten history: the old clocks no
        // longer describe it — no stamp is rendered rather than a wrong one.
        assert!(
            reg.finish("scout", vec![Message::user_text("summary")], 0)
                .is_none()
        );
        let (history, stamps, _) = reg.view_of("scout").unwrap_or_else(|| panic!("exists"));
        assert_eq!(history.len(), 1);
        assert_eq!(stamps, vec![0]);
        // The record grows again: only the new tail is stamped.
        assert!(
            reg.finish(
                "scout",
                vec![Message::user_text("summary"), Message::user_text("more")],
                0,
            )
            .is_none()
        );
        let (_, stamps, _) = reg.view_of("scout").unwrap_or_else(|| panic!("exists"));
        assert_eq!(stamps[0], 0, "the rewritten prefix stays clockless");
        assert!(stamps[1] > 0, "{stamps:?}");
    }

    /// A message claimed by a run must not vanish between the drain and the
    /// landing: the inbox empties at the claim point and the history only
    /// catches up at finish — the in-flight record carries the DM view across
    /// that window, and lets go the moment the history holds the message.
    #[test]
    fn a_claimed_message_stays_visible_until_it_lands() {
        let reg = AgentRegistry::new();
        reg.insert(
            "scout",
            AgentKind::Hire,
            None,
            "research".into(),
            test_session(),
        );
        assert!(reg.finish("scout", Vec::new(), 0).is_none());
        let _ = reg
            .deliver(
                "scout",
                crate::channels::MAIN_NAME,
                "map the module",
                Vec::new(),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            reg.pending_of("scout"),
            vec![(
                crate::channels::MAIN_NAME.to_string(),
                "map the module".to_string()
            )],
            "the sender rides with the message: a pair view has one conversation in it"
        );

        // Claimed: gone from the inbox, not yet in the history — in flight.
        assert_eq!(reg.flush_pending().len(), 1);
        assert!(reg.pending_of("scout").is_empty());
        let (history, _, _) = reg.view_of("scout").unwrap_or_else(|| panic!("exists"));
        let in_flight = reg.in_flight_of("scout");
        assert!(history.is_empty());
        assert_eq!(
            in_flight,
            vec![(
                crate::channels::MAIN_NAME.to_string(),
                "map the module".to_string()
            )]
        );

        // Landed: the stored history carries it, the bridge record is gone.
        let landed = vec![
            Message::user_text("map the module"),
            Message {
                role: crate::api::types::Role::Assistant,
                content: vec![crate::api::types::ContentBlock::Text {
                    text: "mapped".into(),
                }],
            },
        ];
        assert!(reg.finish("scout", landed, 6).is_none());
        let (history, _, _) = reg.view_of("scout").unwrap_or_else(|| panic!("exists"));
        assert_eq!(history.len(), 2);
        let in_flight = reg.in_flight_of("scout");
        assert!(in_flight.is_empty(), "history took over: {in_flight:?}");
    }

    /// A failed run puts its claimed batch back in the inbox; the in-flight
    /// record lets go of it in the same move, so the message reads as queued
    /// again instead of being on screen twice.
    #[test]
    fn a_restored_message_leaves_the_in_flight_record() {
        let reg = AgentRegistry::new();
        reg.insert(
            "scout",
            AgentKind::Hire,
            None,
            "research".into(),
            test_session(),
        );
        assert!(reg.finish("scout", Vec::new(), 0).is_none());
        let _ = reg
            .deliver(
                "scout",
                crate::channels::MAIN_NAME,
                "map the module",
                Vec::new(),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        let wake = reg
            .flush_pending()
            .pop()
            .unwrap_or_else(|| panic!("claimed"));
        let in_flight = reg.in_flight_of("scout");
        assert_eq!(in_flight.len(), 1);

        reg.restore_inbox("scout", wake.items);
        let in_flight = reg.in_flight_of("scout");
        assert!(in_flight.is_empty(), "{in_flight:?}");
        assert_eq!(
            reg.pending_of("scout"),
            vec![(
                crate::channels::MAIN_NAME.to_string(),
                "map the module".to_string()
            )],
            "the sender rides with the message: a pair view has one conversation in it"
        );
    }

    #[test]
    fn lifecycle_running_idle_queue_and_revive() {
        let reg = AgentRegistry::new();
        reg.insert(
            "scout",
            AgentKind::Hire,
            None,
            "research".into(),
            test_session(),
        );
        // Running: message queued (delivery never happens inside deliver itself).
        let first = reg
            .deliver(
                "scout",
                crate::channels::MAIN_NAME,
                "add A",
                Vec::new(),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        // Turn finished + inbox non-empty → continues (history saved, inbox drained, ack set).
        let next = reg
            .finish("scout", vec![Message::user_text("hi")], 1)
            .unwrap_or_else(|| panic!("should continue"));
        assert_eq!(
            next.history.len(),
            1,
            "the continuation carries the latest history"
        );
        assert!(
            matches!(&next.items[..], [InboxItem::Direct { text: m, .. }] if m == "add A"),
            "inbox content"
        );
        assert_eq!(reg.list()[0].state, AgentState::Running);
        let acks = reg.acks_of("scout").unwrap_or_else(|| unreachable!());
        assert_eq!(acks[0].id, first);
        assert_eq!(acks[0].state, AckState::Delivered { run: next.run });
        // Finish again with an empty inbox → Idle.
        assert!(reg.finish("scout", Vec::new(), 1).is_none());
        assert_eq!(reg.list()[0].state, AgentState::Idle);
        // Idle: the message waits for a flush rather than starting a run on the spot.
        let _ = reg
            .deliver(
                "scout",
                crate::channels::MAIN_NAME,
                "look at B again",
                Vec::new(),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            reg.list()[0].state,
            AgentState::Idle,
            "delivery does not start a run by itself"
        );
        let woken = reg.flush_pending();
        assert_eq!(woken.len(), 1);
        assert!(
            matches!(&woken[0].items[..], [InboxItem::Direct { text: m, .. }] if m == "look at B again")
        );
        assert_eq!(reg.list()[0].state, AgentState::Running);
        assert!(
            reg.flush_pending().is_empty(),
            "claimed instances do not start twice"
        );
    }

    fn room_line(from: &str, seq: u64) -> InboxItem {
        InboxItem::Channel {
            channel: "t".into(),
            from: from.into(),
            text: "report".into(),
            seq,
        }
    }

    #[test]
    fn inbox_accumulates_direct_and_channel_items_in_order() {
        let reg = AgentRegistry::new();
        reg.insert("w", AgentKind::Hire, None, "w".into(), test_session());
        let _ = reg.deliver(
            "w",
            crate::channels::MAIN_NAME,
            "do 1 first",
            Vec::new(),
            None,
        );
        assert!(reg.deposit("w", room_line("a", 3)));
        let items = reg
            .finish("w", Vec::new(), 1)
            .unwrap_or_else(|| panic!("continue"))
            .items;
        assert_eq!(items.len(), 2);
        assert!(
            matches!(&items[0], InboxItem::Direct { text: m, .. } if m == "do 1 first"),
            "in order"
        );
        assert!(
            matches!(&items[1], InboxItem::Channel { seq: 3, from, .. } if from == "a"),
            "channel entries carry seq/from"
        );
        // v7: an unmentioned room line wakes an idle member like any other —
        // the `@` decides what is owed, never what is read. A finished turn
        // with an empty inbox still parks idle: nothing polls.
        assert!(
            reg.finish("w", Vec::new(), 1).is_none(),
            "empty inbox parks"
        );
        assert!(reg.deposit("w", room_line("b", 4)));
        let woken = reg.flush_pending();
        assert_eq!(woken.len(), 1, "one unmentioned line is enough");
        assert_eq!(woken[0].items.len(), 1);
        assert!(reg.finish("w", Vec::new(), 1).is_none());
        assert!(reg.deposit("w", room_line("b", 5)));
        assert!(reg.deposit("w", room_line("b", 6)));
        let woken = reg.flush_pending();
        assert_eq!(woken.len(), 1);
        assert_eq!(
            woken[0].items.len(),
            2,
            "whatever is waiting drains together, in order"
        );
        let _ = reg.stop("w");
        let dropped = room_line("c", 6);
        assert!(
            !reg.deposit("w", dropped.clone()),
            "stopped members do not receive"
        );
        assert!(
            !reg.deposit("ghost", dropped),
            "unknown instances are silently dropped"
        );
    }

    /// v7's wake rule, whole: a non-empty inbox wakes, an empty one never
    /// does. The count and age gates it replaces were proxies for a question
    /// the sender now answers with the `@` — and in a room of six the count
    /// was an amplifier, since one round where everyone speaks leaves five
    /// unread in every inbox and re-crosses it for all of them.
    #[test]
    fn any_waiting_line_wakes_and_an_empty_inbox_never_does() {
        let reg = AgentRegistry::new();
        reg.insert("w", AgentKind::Hire, None, "w".into(), test_session());
        assert!(reg.finish("w", Vec::new(), 0).is_none(), "start idle");
        assert!(
            reg.flush_pending().is_empty(),
            "an empty inbox never wakes: no polling, and a quiet room is free"
        );

        assert!(reg.deposit("w", room_line("a", 1)));
        let woken = reg.flush_pending();
        assert_eq!(woken.len(), 1, "one unmentioned line wakes on its own");
        assert_eq!(woken[0].items.len(), 1);
        assert!(
            reg.flush_pending().is_empty(),
            "and the drain leaves nothing behind to wake it again"
        );

        // A running member is not woken twice: what lands while it works is
        // absorbed at its next tool boundary instead (`take_running`).
        assert!(reg.deposit("w", room_line("a", 2)));
        assert!(
            reg.flush_pending().is_empty(),
            "a running member takes its mail at the tool boundary, not by waking"
        );
        assert_eq!(reg.take_running("w", 0).len(), 1, "steered mid-turn");
    }

    #[test]
    fn share_hooks_track_insert_finish_stop() {
        let root = std::env::temp_dir().join(format!("bingo-agents-{}-share", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = crate::share::ShareStore::load_or_create(&root.join("shares").join("s.json"))
            .unwrap_or_else(|e| panic!("{e}"));
        let reg = AgentRegistry::new();
        reg.attach_share(store.clone());

        // insert → creates an entry (running, empty history).
        reg.insert(
            "scout",
            AgentKind::Hire,
            Some("scout".into()),
            "research".into(),
            test_session(),
        );
        let doc = store.snapshot();
        assert_eq!(doc.agents.len(), 1);
        assert_eq!(doc.agents[0].state, "running");
        assert_eq!(doc.agents[0].def.as_deref(), Some("scout"));
        assert!(doc.agents[0].history.is_empty());

        // finish → history + state (empty inbox → idle).
        reg.finish("scout", vec![Message::user_text("hi")], 1);
        let doc = store.snapshot();
        assert_eq!(doc.agents[0].state, "idle");
        assert_eq!(doc.agents[0].history.len(), 1);
        assert_eq!(doc.agents[0].history[0], Message::user_text("hi"));

        // A busy non-empty inbox → stays running after finish (Idle wake-up drains the inbox into Start,
        // while Running queues; two instructions create the queue scenario).
        reg.deliver(
            "scout",
            crate::channels::MAIN_NAME,
            "check again",
            Vec::new(),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        reg.deliver(
            "scout",
            crate::channels::MAIN_NAME,
            "check once more",
            Vec::new(),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        reg.finish("scout", Vec::new(), 1);
        let doc = store.snapshot();
        assert_eq!(doc.agents[0].state, "running");
        // Inbox drained → idle.
        reg.finish("scout", Vec::new(), 1);
        let doc = store.snapshot();
        assert_eq!(doc.agents[0].state, "idle");

        // stop → stopped.
        reg.stop("scout").unwrap_or_else(|e| panic!("{e}"));
        let doc = store.snapshot();
        assert_eq!(doc.agents[0].state, "stopped");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn main_name_is_reserved() {
        let reg = AgentRegistry::new();
        assert_eq!(
            reg.claim_name("main"),
            "main-2",
            "the main agent owns the name; a subagent asking for it is renamed"
        );
    }

    /// Several messages sent before a boundary arrive as one batch: the receiver reads them
    /// together instead of burning a turn per message.
    #[test]
    fn messages_sent_in_one_turn_arrive_as_one_batch() {
        let reg = AgentRegistry::new();
        reg.insert("w", AgentKind::Hire, None, "w".into(), test_session());
        assert!(reg.finish("w", Vec::new(), 1).is_none(), "turns idle first");
        for text in ["look at A first", "look at B again", "and finally C"] {
            reg.deliver("w", crate::channels::MAIN_NAME, text, Vec::new(), None)
                .unwrap_or_else(|e| panic!("{e}"));
        }
        assert_eq!(
            reg.list()[0].pending,
            3,
            "all queued, none started individually"
        );

        let woken = reg.flush_pending();
        assert_eq!(woken.len(), 1, "one instance runs one round");
        assert_eq!(woken[0].items.len(), 3, "all three delivered at once");
        let acks = reg.acks_of("w").unwrap_or_else(|| unreachable!());
        assert!(
            acks.iter()
                .all(|a| a.state == AckState::Delivered { run: woken[0].run }),
            "all three land in one round: {acks:?}"
        );
    }

    /// Stopping discards the inbox — every message in it is recorded as dropped, so a sender
    /// that only saw "queued" can still find out it was never delivered.
    #[test]
    fn stop_records_undelivered_messages_as_dropped() {
        let reg = AgentRegistry::new();
        reg.insert("w", AgentKind::Hire, None, "w".into(), test_session());
        let id = reg
            .deliver(
                "w",
                crate::channels::MAIN_NAME,
                "is it too late",
                Vec::new(),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        let (_, dropped) = reg.stop("w").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(dropped, 1);
        let acks = reg.acks_of("w").unwrap_or_else(|| unreachable!());
        assert_eq!(acks.len(), 1);
        assert_eq!(acks[0].id, id);
        assert!(
            matches!(&acks[0].state, AckState::Dropped { reason } if reason.contains("stopped")),
            "{:?}",
            acks[0].state
        );
        assert_eq!(reg.list()[0].pending, 0, "inbox cleared");
    }

    /// CC subagent semantics (D105a): a direct message to a stopped instance
    /// resumes it — the registry kept its session and history, so the delivery
    /// flips it to idle and the ordinary wake path takes it from there. The
    /// chase and the room broadcast deliberately do not take this door.
    #[test]
    fn a_direct_message_resumes_a_stopped_instance() {
        let reg = AgentRegistry::new();
        reg.insert("w", AgentKind::Hire, None, "w".into(), test_session());
        reg.finish("w", vec![crate::api::types::Message::user_text("go")], 0);
        reg.stop("w").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(reg.list()[0].state, AgentState::Stopped);

        reg.deliver(
            "w",
            crate::channels::MAIN_NAME,
            "carry on",
            Vec::new(),
            None,
        )
        .unwrap_or_else(|e| panic!("a stopped instance accepts a direct message: {e}"));
        assert_eq!(
            reg.list()[0].state,
            AgentState::Idle,
            "flipped, not refused"
        );

        let woken = reg.flush_pending();
        assert_eq!(woken.len(), 1, "the ordinary wake path picks it up");
        assert_eq!(woken[0].name, "w");
        assert_eq!(
            woken[0].history,
            vec![crate::api::types::Message::user_text("go")],
            "resumed from the history the registry kept"
        );
        assert_eq!(reg.list()[0].state, AgentState::Running);
    }

    /// The chase is bounded and self-cancelling: while a message goes unanswered each round leaves
    /// one follow-up riding with it, the budget stops at MAX_FOLLOW_UPS, and the reply that finally
    /// comes settles every later check.
    #[test]
    fn follow_up_chases_a_queued_message_until_the_budget_runs_out() {
        let reg = AgentRegistry::new();
        reg.insert("w", AgentKind::Hire, None, "w".into(), test_session());
        let id = reg
            .deliver(
                "w",
                crate::channels::MAIN_NAME,
                "check the logs",
                Vec::new(),
                Some(Duration::from_secs(30)),
            )
            .unwrap_or_else(|e| panic!("{e}"));
        for round in 1..=MAX_FOLLOW_UPS {
            assert_eq!(reg.follow_up("w", id), FollowUp::Sent { round });
        }
        assert_eq!(
            reg.follow_up("w", id),
            FollowUp::Exhausted,
            "budget exhausted"
        );
        let items = reg
            .finish("w", Vec::new(), 1)
            .unwrap_or_else(|| panic!("queued messages should be claimed by the receiver"))
            .items;
        assert_eq!(
            items.len(),
            1 + MAX_FOLLOW_UPS as usize,
            "follow-ups arrive in the same batch as the original"
        );
        assert!(
            matches!(&items[1], InboxItem::FollowUp { original, round: 1, .. } if *original == id),
            "follow-up points at the original message: {:?}",
            items[1]
        );
        let acks = reg.acks_of("w").unwrap_or_else(|| unreachable!());
        assert_eq!(acks.len(), 1, "the follow-up itself leaves no receipt");
        assert_eq!(
            acks[0].follow_ups, MAX_FOLLOW_UPS,
            "follow-up count is available for review"
        );
        assert_eq!(acks[0].timeout, Some(Duration::from_secs(30)));
        // Read into a prompt is still not an acknowledgement — only the reply ends the chase.
        assert!(
            matches!(
                reg.acks_of("w").unwrap_or_else(|| unreachable!())[0].state,
                AckState::Delivered { .. }
            ),
            "entering the context is not yet a receipt"
        );
        assert!(
            reg.finish("w", Vec::new(), 2).is_none(),
            "that round answers"
        );
        assert!(
            matches!(
                reg.follow_up("w", id),
                FollowUp::Settled(AckState::Answered { .. })
            ),
            "no follow-up after a reply"
        );
    }

    /// The silence the sender actually cares about: the receiver took the message and ended its
    /// turn without a word. Delivery looks like success and is not, so the chase must continue —
    /// and the follow-up has to name which silence it is, since the two need different words.
    #[test]
    fn a_turn_that_says_nothing_does_not_acknowledge_what_it_read() {
        let reg = AgentRegistry::new();
        reg.insert(
            "mute",
            AgentKind::Hire,
            None,
            "silent".into(),
            test_session(),
        );
        assert!(
            reg.finish("mute", Vec::new(), 1).is_none(),
            "turns idle first"
        );
        let id = reg
            .deliver(
                "mute",
                crate::channels::MAIN_NAME,
                "report progress",
                Vec::new(),
                Some(Duration::from_secs(30)),
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            reg.flush_pending().len(),
            1,
            "idle instances receive at the boundary"
        );
        // The turn ends producing no text for main.
        assert!(reg.finish("mute", Vec::new(), 0).is_none());
        let acks = reg.acks_of("mute").unwrap_or_else(|| unreachable!());
        assert!(
            matches!(acks[0].state, AckState::Delivered { run: 1 }),
            "a silent round is not a receipt: {:?}",
            acks[0].state
        );
        assert_eq!(
            reg.list()[0].unacked,
            1,
            "the sender is still waiting for an answer"
        );
        assert_eq!(reg.follow_up("mute", id), FollowUp::Sent { round: 1 });
        assert!(
            matches!(
                reg.flush_pending()[0].items[..],
                [InboxItem::FollowUp {
                    delivered: true,
                    ..
                }]
            ),
            "the follow-up marks 'read but silent' rather than 'not picked up'"
        );
        // Speaking up answers what it had already read, even though a later run says it.
        assert!(reg.finish("mute", Vec::new(), 1).is_none());
        assert_eq!(
            reg.acks_of("mute").unwrap_or_else(|| unreachable!())[0].state,
            AckState::Answered { run: 2 },
            "the answering round adds the receipt"
        );
        assert_eq!(reg.list()[0].unacked, 0);
    }

    /// The chase also ends when there is nothing left to chase: a stopped instance drops the
    /// message, a deleted one takes the record with it. Both are reportable outcomes, not silence.
    #[test]
    fn follow_up_settles_on_a_dropped_message_and_a_gone_instance() {
        let reg = AgentRegistry::new();
        reg.insert("w", AgentKind::Hire, None, "w".into(), test_session());
        let id = reg
            .deliver(
                "w",
                crate::channels::MAIN_NAME,
                "is it too late",
                Vec::new(),
                Some(Duration::from_secs(10)),
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(reg.follow_up("w", id), FollowUp::Sent { round: 1 });
        reg.stop("w").unwrap_or_else(|e| panic!("{e}"));
        assert!(
            matches!(
                reg.follow_up("w", id),
                FollowUp::Settled(AckState::Dropped { .. })
            ),
            "stopping discards"
        );
        reg.remove("w").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(reg.follow_up("w", id), FollowUp::Gone);
        assert_eq!(reg.follow_up("ghost", MsgId(999)), FollowUp::Gone);
    }

    /// A run chain that dies with messages still queued must not strand them: the instance goes
    /// back to Idle and the recovery dispatcher picks the batch up.
    #[test]
    fn messages_survive_a_failed_run_and_are_retried() {
        let reg = AgentRegistry::new();
        reg.insert("w", AgentKind::Hire, None, "w".into(), test_session());
        reg.deliver(
            "w",
            crate::channels::MAIN_NAME,
            "continue",
            Vec::new(),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        // The run failed (spawn_agent_loop's error branch) — it only marks the instance idle.
        reg.mark_idle("w");
        assert_eq!(
            reg.list()[0].pending,
            1,
            "the message is still in the inbox"
        );
        let woken = reg.flush_pending();
        assert_eq!(woken.len(), 1, "the recovery dispatcher re-delivers");
        assert_eq!(woken[0].items.len(), 1);
    }

    #[test]
    fn delivered_messages_require_output_after_their_delivery_offset() {
        let reg = AgentRegistry::new();
        reg.insert("w", AgentKind::Hire, None, "w".into(), test_session());
        let _ = reg.next_run("w");
        let id = reg
            .deliver(
                "w",
                crate::channels::MAIN_NAME,
                "late instruction",
                Vec::new(),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        let items = reg.take_running("w", 5);
        assert_eq!(items.len(), 1);
        assert_eq!(
            reg.acks_of("w").unwrap_or_else(|| unreachable!())[0].state,
            AckState::Delivered { run: 1 }
        );
        assert!(reg.finish("w", Vec::new(), 5).is_none());
        assert_eq!(
            reg.acks_of("w").unwrap_or_else(|| unreachable!())[0].state,
            AckState::Delivered { run: 1 },
            "text produced before delivery does not answer it"
        );
        assert_eq!(reg.follow_up("w", id), FollowUp::Sent { round: 1 });
        let follow_up = reg.flush_pending();
        assert_eq!(follow_up.len(), 1);
        assert!(reg.finish("w", Vec::new(), 1).is_none());
        assert_eq!(
            reg.acks_of("w").unwrap_or_else(|| unreachable!())[0].state,
            AckState::Answered { run: 2 },
            "later text does answer the previously delivered message"
        );
    }

    #[test]
    fn restored_running_batch_returns_to_queued_state() {
        let reg = AgentRegistry::new();
        reg.insert("w", AgentKind::Hire, None, "w".into(), test_session());
        let _ = reg.next_run("w");
        reg.deliver(
            "w",
            crate::channels::MAIN_NAME,
            "retry me",
            Vec::new(),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let items = reg.take_running("w", 0);
        assert!(matches!(
            reg.acks_of("w").unwrap_or_else(|| unreachable!())[0].state,
            AckState::Delivered { run: 1 }
        ));
        reg.restore_inbox("w", items);
        assert_eq!(
            reg.acks_of("w").unwrap_or_else(|| unreachable!())[0].state,
            AckState::Queued
        );
        reg.mark_idle("w");
        let retry = reg.flush_pending();
        assert_eq!(retry.len(), 1);
        assert_eq!(retry[0].run, 2);
    }

    #[tokio::test]
    async fn stop_wins_before_a_claimed_run_installs_its_abort_handle() {
        let reg = AgentRegistry::new();
        reg.insert("w", AgentKind::Hire, None, "w".into(), test_session());
        reg.mark_idle("w");
        reg.deliver("w", crate::channels::MAIN_NAME, "start", Vec::new(), None)
            .unwrap_or_else(|e| panic!("{e}"));
        let wake = reg.flush_pending().pop().unwrap_or_else(|| unreachable!());
        assert_eq!(wake.run, 1);
        reg.stop("w").unwrap_or_else(|e| panic!("{e}"));
        let task = tokio::spawn(async { std::future::pending::<()>().await });
        assert!(
            !reg.set_abort_if_running("w", wake.run, task.abort_handle(), wake.items),
            "a stopped entry rejects the late handle"
        );
        assert!(!reg.accepts_run("w", wake.run));
    }

    #[test]
    fn stop_and_delete_semantics() {
        let reg = AgentRegistry::new();
        reg.insert("x", AgentKind::Hire, None, "x".into(), test_session());
        reg.set_run_watch("x", crate::watch::WatchId(7));
        assert_eq!(
            reg.stop("x").unwrap_or_else(|e| panic!("{e}")),
            (Some(crate::watch::WatchId(7)), 0),
            "stopping while running returns the current watch line"
        );
        assert!(
            reg.stop("x").unwrap_or_else(|e| panic!("{e}")).0.is_none(),
            "idempotent"
        );
        // Turn finishing after a stop: history is still archived, no revival.
        assert!(reg.finish("x", vec![Message::user_text("h")], 1).is_none());
        assert_eq!(reg.list()[0].state, AgentState::Stopped);
        // A direct message after the stop is the one thing that revives (D105a).
        reg.deliver(
            "x",
            crate::channels::MAIN_NAME,
            "still there",
            Vec::new(),
            None,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            reg.list()[0].state,
            AgentState::Idle,
            "resumed, not refused"
        );
        reg.remove("x").unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.list().is_empty());
        assert_eq!(reg.claim_name("x"), "x", "deletion frees the name");
        assert!(
            reg.deliver("x", crate::channels::MAIN_NAME, "hi", Vec::new(), None)
                .is_err(),
            "unknown instance errors"
        );
        // Stopping an idle instance: no active line.
        reg.insert("y", AgentKind::Hire, None, "y".into(), test_session());
        reg.set_run_watch("y", crate::watch::WatchId(9));
        assert!(reg.finish("y", Vec::new(), 1).is_none());
        assert!(
            reg.stop("y").unwrap_or_else(|e| panic!("{e}")).0.is_none(),
            "stopping while idle does not cancel a terminal watch line"
        );
    }
}

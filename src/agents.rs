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

use tokio::sync::{mpsc, oneshot};

use crate::api::types::Message;
use crate::app::answer::Answer;
use crate::app::controller::Control;
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
    /// The thinking budget this instance runs with, which a definition or a
    /// blueprint can pin per instance the same way it pins the model. `None` is
    /// the level "off" rather than an unknown.
    pub thinking: Option<String>,
    /// Where this instance works. A sub-team node can sit in another directory
    /// or another repository, so "which member is where" is not the session's
    /// answer to give.
    pub cwd: PathBuf,
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
    /// Who sent it. Recorded since D137, when the answer stopped being obvious:
    /// every direct message used to come from main, so a chase could name the
    /// sender from a constant. A peer sends now too, and a follow-up that says
    /// "Main sent you message #3" about a colleague's message is a lie the
    /// receiver has no way to check.
    pub from: String,
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
        /// Who is still waiting — copied off the [`Ack`] rather than assumed to
        /// be main (D137).
        from: String,
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

/// One instance as a reader sees it.
///
/// Held behind an `Arc` in [`Roster`] so republishing one instance's change
/// does not copy every other instance's history. The session and the progress
/// cell travel as handles rather than as copies, because both are read *live*:
/// a row's model, its permission mode and its token count all change without the
/// registry hearing about it, and a snapshot that froze them would stop the
/// clock on a running turn.
struct AgentRow {
    def: Option<String>,
    description: String,
    prompt: String,
    state: AgentState,
    kind: AgentKind,
    last_active: Instant,
    history: Vec<Message>,
    stamps: Vec<u64>,
    inbox: Vec<InboxItem>,
    /// Read by the tests that assert the bridge record; no surface draws it
    /// directly (the DM view reads it through the page walk).
    #[cfg_attr(not(test), allow(dead_code))]
    in_flight: Vec<(MsgId, String, String)>,
    acks: Vec<Ack>,
    session: Arc<Session>,
    progress: Option<Arc<Mutex<AgentProgress>>>,
}

/// The replacement snapshot the registry publishes after every change.
#[derive(Default)]
pub struct Roster {
    instances: std::collections::BTreeMap<String, Arc<AgentRow>>,
    /// Whether a share document is attached.
    #[cfg_attr(not(test), allow(dead_code))]
    shared: bool,
    /// The permission prompt subagents borrow, and the console channel their
    /// turns are written to. Attached once by whoever owns the screen; a
    /// headless run has neither and says so rather than inventing one.
    ask: Option<Arc<crate::query::AskFn>>,
    events: Option<crate::ui::EventSink>,
}

/// Which instances a change touched, so republishing can keep the rest.
enum Touched {
    None,
    One(String),
    Every,
}

/// The instance registry's state, owned by the session actor.
///
/// What used to be a single lock carrying the state machine and the inbox — so
/// that the check-and-claim of delivery and turn finalization could not
/// interleave — is now the actor's one loop, which gives the same atomicity
/// across all three registries rather than within one.
pub struct AgentRegistry {
    inner: HashMap<String, Entry>,
    /// Share persistence (Option semantics: behavior is unchanged when not attached; once attached, insert/finish/stop sync snapshots).
    share: Option<Arc<crate::share::ShareStore>>,
    /// Where the attached document's disk writes happen — never here.
    saver: Option<crate::share::ShareSaver>,
    /// Permission prompt of the session that owns the UI. Subagents have none of their own, so
    /// they borrow this one; the registry is the single place every spawn path can reach it from
    /// (the Agent tool, channel delivery, and the TUI channel room alike).
    ask: Option<Arc<crate::query::AskFn>>,
    /// The console's event channel, attached by whichever front end owns the
    /// screen. Every spawn path reaches a run's UI through the registry already
    /// (`ask`), and a run's stream is the same kind of borrowing: the instance
    /// has no surface of its own, so it writes onto the surface that does.
    events: Option<crate::ui::EventSink>,
    /// Monotonic message id source (registry-wide, so ids never collide across instances).
    next_msg: u64,
    /// Messages accepted since the actor last looked, in the order they were
    /// accepted. Drained by the actor, which turns each into an item in the
    /// receiver's conversation (B4).
    delivered: Vec<Delivered>,
    /// Inbox generation: every accepted item advances this watch channel. Receivers wait on it
    /// between tool rounds so a busy agent does not depend on the sender reaching a boundary.
    inbox_tx: tokio::sync::watch::Sender<u64>,
    view: tokio::sync::watch::Sender<Arc<Roster>>,
}

impl AgentRegistry {
    fn notify_inbox(&self) {
        self.inbox_tx.send_modify(|generation| *generation += 1);
    }

    fn mint_msg_id(&mut self) -> MsgId {
        let id = MsgId(self.next_msg);
        self.next_msg += 1;
        id
    }

    /// Replace share persistence for future instance changes.
    fn attach_share(&mut self, store: Arc<crate::share::ShareStore>) {
        self.saver = Some(crate::share::ShareSaver::spawn(store.clone()));
        self.share = Some(store);
    }

    /// A sink bound to `name`'s conversation, or `None` with no front end
    /// attached — an embedded or headless run, whose turns nobody is watching.
    fn sink_for(&self, name: &str) -> Option<crate::ui::EventSink> {
        self.events
            .as_ref()
            .map(|sink| sink.bound_to(crate::ui::ConvKey::Agent(name.to_string())))
    }

    /// Write an instance's latest snapshot into the share document (no-op without a store).
    fn sync_share(&self, name: &str) {
        let Some(store) = self.share.as_ref() else {
            return;
        };
        let Some(entry) = self.inner.get(name) else {
            return;
        };
        store.upsert_agent(
            name,
            entry.def.clone(),
            entry.description.clone(),
            entry.state,
            entry.history.clone(),
        );
        if let Some(saver) = &self.saver {
            saver.save();
        }
    }

    /// Claim an instance name: use the base name when free, otherwise append `-2`/`-3`…
    /// (so parallel same-name instances stay distinguishable).
    /// `main`/`user` are reserved for main and the user (channel member names),
    /// `all` for the broadcast mention token, and none are ever handed out.
    fn claim_name(&self, base: &str) -> String {
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
        let inner = &self.inner;
        if !taken(inner, base) {
            return base.to_string();
        }
        let mut n = 2;
        loop {
            let candidate = format!("{base}-{n}");
            if !taken(inner, &candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Register a new instance (state=Running). The name must first go through claim_name.
    fn insert(
        &mut self,
        name: &str,
        kind: AgentKind,
        def: Option<String>,
        description: String,
        session: Arc<Session>,
    ) {
        self.inner.insert(
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
    fn refresh(
        &mut self,
        name: &str,
        def: Option<String>,
        description: String,
        session: Arc<Session>,
    ) -> Refresh {
        let outcome = {
            let Some(entry) = self.inner.get_mut(name) else {
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
    fn release_hires(&mut self) -> Vec<String> {
        let inner = &mut self.inner;
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

    fn set_prompt(&mut self, name: &str, prompt: String) {
        if let Some(entry) = self.inner.get_mut(name) {
            entry.prompt = prompt;
        }
    }

    fn set_progress(&mut self, name: &str, progress: Option<Arc<Mutex<AgentProgress>>>) {
        if let Some(entry) = self.inner.get_mut(name) {
            entry.progress = progress;
        }
    }

    fn touch(&mut self, name: &str) {
        if let Some(entry) = self.inner.get_mut(name) {
            entry.last_active = Instant::now();
        }
    }

    /// The permission mode this instance runs under (D105).
    ///
    /// Inherited from the parent at spawn (`tool::agent`) and cycled per
    /// instance from its zoomed view, the way CC cycles a viewed teammate's own
    /// mode and leaves the leader's alone (`PromptInput.tsx:1410-1447`; the
    /// field is declared "cycled independently via Shift+Tab when viewing",
    /// `InProcessTeammateTask/types.ts:44`).
    /// Point an instance at a session carrying `mode`, and say whether that
    /// changed anything.
    ///
    /// `Session` is immutable inside its `Arc`, so this is the same derive-a-copy
    /// move the console makes for its own turns (`Chat::session_for_turn`): every
    /// other field is a shared handle, so the registries, the watch board and the
    /// task store still point at the same state. The run **in flight** captured
    /// the old `Arc` and keeps its mode; the next one — a wake, a resume, a
    /// follow-up — reads this.
    fn set_permission_mode(&mut self, name: &str, mode: crate::permission::PermissionMode) -> bool {
        let Some(entry) = self.inner.get_mut(name) else {
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
    /// Replace an instance's stored context with a rewritten one — `/compact`
    /// on its page, and nothing else.
    ///
    /// **Refused while it is running**, and the refusal is the point: a turn in
    /// flight carries its own copy of the history and writes it back at
    /// [`AgentRegistry::finish`], so a summary spliced in underneath would be
    /// overwritten by the very next round. The check reads the state under the
    /// same lock the write takes, so a run that starts between the console's
    /// look and its write loses the race rather than the work.
    fn replace_history(&mut self, name: &str, history: Vec<Message>) -> bool {
        let replaced = {
            let Some(entry) = self.inner.get_mut(name) else {
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

    fn set_abort_if_running(
        &mut self,
        name: &str,
        run: u64,
        abort: tokio::task::AbortHandle,
        items: Vec<InboxItem>,
    ) -> bool {
        let Some(entry) = self.inner.get_mut(name) else {
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
    fn next_run(&mut self, name: &str) -> u64 {
        match self.inner.get_mut(name) {
            Some(entry) => {
                entry.runs += 1;
                entry.last_active = Instant::now();
                entry.runs
            }
            None => 1,
        }
    }

    /// Record the watch line of the current turn.
    fn set_run_watch(&mut self, name: &str, id: crate::watch::WatchId) {
        if let Some(entry) = self.inner.get_mut(name) {
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
    fn finish(
        &mut self,
        name: &str,
        history: Vec<Message>,
        output_chars: usize,
    ) -> Option<Continuation> {
        let result = {
            let entry = self.inner.get_mut(name)?;
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
    fn flush_pending(&mut self) -> Vec<Wake> {
        let mut woken = Vec::new();
        {
            for (name, entry) in self.inner.iter_mut() {
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
    fn take_running(&mut self, name: &str, output_chars: usize) -> Vec<InboxItem> {
        let items = {
            let Some(entry) = self.inner.get_mut(name) else {
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
    fn restore_inbox(&mut self, name: &str, mut items: Vec<InboxItem>) {
        if items.is_empty() {
            return;
        }
        let Some(entry) = self.inner.get_mut(name) else {
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
        self.notify_inbox();
    }

    /// A claimed idle run may be stopped before its task is spawned. Re-checking under the
    /// registry lock closes that gap without exposing the entry or holding the lock across spawn.
    fn accepts_run(&self, name: &str, run: u64) -> bool {
        self.inner
            .get(name)
            .is_some_and(|entry| entry.state == AgentState::Running && entry.runs == run)
    }

    /// Turn failed: keep the pre-failure history, switch to Idle (retryable via SendMessage).
    fn mark_idle(&mut self, name: &str) {
        if let Some(entry) = self.inner.get_mut(name)
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
    fn deliver(
        &mut self,
        name: &str,
        from: &str,
        message: &str,
        images: Vec<crate::api::types::ImageAttachment>,
        ack_timeout: Option<Duration>,
    ) -> Result<MsgId, String> {
        let id = self.mint_msg_id();
        let Some(entry) = self.inner.get_mut(name) else {
            let known: Vec<String> = self.inner.keys().cloned().collect();
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
                from: from.to_string(),
                excerpt: first_line(message),
                state: AckState::Queued,
                queued_at: Instant::now(),
                timeout: ack_timeout,
                follow_ups: 0,
                delivered_after_chars: None,
            },
        );
        // Sending *is* answering, where the sender is a colleague: this is the
        // one place a message back can be observed, and without it a peer's
        // request stays outstanding until the chase gives up on an instance
        // that answered it properly (D137).
        if let Some(sender) = self.inner.get_mut(from) {
            settle_peer_acks(sender, name);
        }
        // The receiver's page shows what it was told, when it was told (D135).
        // A running instance absorbs its mail at its next tool barrier, so
        // without this a user watching one could not see what main had just
        // asked for until the run got round to reading it. Every sender comes
        // through here, so this is where the echo belongs; the absorbed prompt
        // repeats it and the console drops the repeat.
        //
        // The actor reads the same instant off `drain_delivered`: the message
        // becomes an item in the receiver's conversation here, with the whole
        // text rather than the ack's excerpt.
        self.delivered.push(Delivered {
            id,
            from: from.to_string(),
            to: name.to_string(),
            text: message.to_string(),
        });
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
    fn follow_up(&mut self, name: &str, id: MsgId) -> FollowUp {
        let Some(entry) = self.inner.get_mut(name) else {
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
        let from = ack.from.clone();
        let excerpt = ack.excerpt.clone();
        let waited = ack.queued_at.elapsed();
        let delivered = matches!(ack.state, AckState::Delivered { .. });
        entry.inbox.push(InboxItem::FollowUp {
            original: id,
            from,
            round,
            excerpt,
            waited,
            delivered,
        });
        self.notify_inbox();
        FollowUp::Sent { round }
    }

    /// Queue a channel message. A stopped member is silently skipped — a broadcast doesn't fail
    /// because one member stopped. Returns whether it was accepted.
    fn deposit(&mut self, name: &str, item: InboxItem) -> bool {
        {
            let Some(entry) = self.inner.get_mut(name) else {
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

    /// Stop: abort a running turn (abort), no longer accept commands; history is kept
    /// and listable. Returns the watch line of the aborted turn (the caller sets
    /// Cancelled); when idle/already stopped there is no active line, returns None (idempotent).
    /// Stopping discards the inbox, so every message still in it is recorded as dropped: a
    /// sender that only ever saw "queued" must be able to find out it was never delivered.
    /// Returns the watch line and how many messages died with it.
    fn stop(&mut self, name: &str) -> Result<(Option<crate::watch::WatchId>, usize), String> {
        let result = {
            let Some(entry) = self.inner.get_mut(name) else {
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
    fn remove(&mut self, name: &str) -> Result<(Option<crate::watch::WatchId>, usize), String> {
        let outcome = self.stop(name)?;
        self.inner.remove(name);
        Ok(outcome)
    }
    /// Republish the reader's snapshot, keeping the `Arc` of every instance the
    /// change did not touch: one instance's turn ending must not copy the
    /// histories of all the others.
    fn publish(&self, touched: Touched) {
        let previous = self.view.borrow().clone();
        let mut instances = std::collections::BTreeMap::new();
        for (name, entry) in &self.inner {
            let unchanged = match &touched {
                Touched::None => true,
                Touched::One(one) => one != name,
                Touched::Every => false,
            };
            let reuse = unchanged
                .then(|| previous.instances.get(name).cloned())
                .flatten();
            instances.insert(
                name.clone(),
                reuse.unwrap_or_else(|| {
                    Arc::new(AgentRow {
                        def: entry.def.clone(),
                        description: entry.description.clone(),
                        prompt: entry.prompt.clone(),
                        state: entry.state,
                        kind: entry.kind,
                        last_active: entry.last_active,
                        history: entry.history.clone(),
                        stamps: entry.stamps.clone(),
                        inbox: entry.inbox.clone(),
                        in_flight: entry.in_flight.clone(),
                        acks: entry.acks.clone(),
                        session: entry.session.clone(),
                        progress: entry.progress.clone(),
                    })
                }),
            );
        }
        let _ = self.view.send(Arc::new(Roster {
            instances,
            shared: self.share.is_some(),
            ask: self.ask.clone(),
            events: self.events.clone(),
        }));
    }

    /// Apply one message, republish what a reader sees, and only then answer.
    ///
    /// The order of the last two steps is the same contract the rooms keep: a
    /// caller that claims a run and then reads the roster cannot read the roster
    /// as it was before its own claim.
    /// Let go of everything the instances hold, for a session that is closing.
    ///
    /// The `Arc<Session>` each entry keeps is one half of D29's cycle — the
    /// registry holds the session, the session holds the handle that reaches the
    /// registry — so releasing them here is what lets the actor's inbox close and
    /// its thread end. Runs are aborted first: their turns are closed by the core
    /// on the way out, and an aborted run cannot report into one anyway.
    pub(crate) fn release(&mut self) {
        for entry in self.inner.values_mut() {
            if let Some(abort) = entry.abort.take() {
                abort.abort();
            }
        }
        self.inner.clear();
        self.ask = None;
        self.events = None;
        self.delivered.clear();
        self.share = None;
        self.saver = None;
        self.publish(Touched::Every);
    }

    pub(crate) fn handle(&mut self, message: AgentMsg) {
        let (touched, answer) = self.apply(message);
        self.publish(touched);
        if let Some(answer) = answer {
            answer();
        }
    }

    fn apply(&mut self, message: AgentMsg) -> (Touched, Option<Answered>) {
        match message {
            AgentMsg::AttachShare(store) => {
                self.attach_share(store);
                (Touched::None, None)
            }
            AgentMsg::DetachShare => {
                self.share = None;
                self.saver = None;
                (Touched::None, None)
            }
            AgentMsg::AttachAsk(ask) => {
                self.ask = Some(ask);
                (Touched::None, None)
            }
            AgentMsg::SetEvents(events) => {
                self.events = Some(events);
                (Touched::None, None)
            }
            AgentMsg::ClaimName { base, reply } => {
                let name = self.claim_name(&base);
                (Touched::None, answered(reply, name))
            }
            AgentMsg::Insert(insertion) => {
                let Insertion {
                    name,
                    kind,
                    def,
                    description,
                    session,
                    reply,
                } = *insertion;
                self.insert(&name, kind, def, description, session);
                (Touched::One(name), answered(reply, ()))
            }
            AgentMsg::Refresh(refresh) => {
                let Refreshing {
                    name,
                    def,
                    description,
                    session,
                    reply,
                } = *refresh;
                let outcome = self.refresh(&name, def, description, session);
                (Touched::One(name), answered(reply, outcome))
            }
            AgentMsg::ReleaseHires { reply } => {
                let released = self.release_hires();
                (Touched::Every, answered(reply, released))
            }
            AgentMsg::SetPrompt {
                name,
                prompt,
                reply,
            } => {
                self.set_prompt(&name, prompt);
                (Touched::One(name), answered(reply, ()))
            }
            AgentMsg::SetProgress {
                name,
                progress,
                reply,
            } => {
                self.set_progress(&name, progress);
                (Touched::One(name), answered(reply, ()))
            }
            AgentMsg::Touch { name } => {
                self.touch(&name);
                (Touched::One(name), None)
            }
            AgentMsg::SetPermissionMode { name, mode, reply } => {
                let changed = self.set_permission_mode(&name, mode);
                (Touched::One(name), answered(reply, changed))
            }
            AgentMsg::ReplaceHistory {
                name,
                history,
                reply,
            } => {
                let replaced = self.replace_history(&name, history);
                (Touched::One(name), answered(reply, replaced))
            }
            AgentMsg::SetAbortIfRunning(abort) => {
                let Aborting {
                    name,
                    run,
                    abort,
                    items,
                    reply,
                } = *abort;
                let armed = self.set_abort_if_running(&name, run, abort, items);
                (Touched::One(name), answered(reply, armed))
            }
            AgentMsg::NextRun { name, reply } => {
                let run = self.next_run(&name);
                (Touched::One(name), answered(reply, run))
            }
            AgentMsg::SetRunWatch { name, id } => {
                self.set_run_watch(&name, id);
                (Touched::One(name), None)
            }
            AgentMsg::Finish {
                name,
                history,
                output_chars,
                reply,
            } => {
                let continuation = self.finish(&name, history, output_chars);
                (Touched::One(name), answered(reply, continuation))
            }
            AgentMsg::FlushPending { reply } => {
                let woken = self.flush_pending();
                (Touched::Every, answered(reply, woken))
            }
            AgentMsg::TakeRunning {
                name,
                output_chars,
                reply,
            } => {
                let items = self.take_running(&name, output_chars);
                (Touched::One(name), answered(reply, items))
            }
            AgentMsg::RestoreInbox { name, items } => {
                self.restore_inbox(&name, items);
                (Touched::One(name), None)
            }
            AgentMsg::AcceptsRun { name, run, reply } => {
                let accepts = self.accepts_run(&name, run);
                (Touched::None, answered(reply, accepts))
            }
            AgentMsg::MarkIdle { name } => {
                self.mark_idle(&name);
                (Touched::One(name), None)
            }
            AgentMsg::Deliver(delivery) => {
                let Delivery {
                    name,
                    from,
                    message,
                    images,
                    ack_timeout,
                    reply,
                } = *delivery;
                let id = self.deliver(&name, &from, &message, images, ack_timeout);
                // The sender's own record changes too when a peer answers by
                // writing back (D137), so no instance's snapshot can be reused.
                (Touched::Every, answered(reply, id))
            }
            AgentMsg::FollowUp { name, id, reply } => {
                let outcome = self.follow_up(&name, id);
                (Touched::One(name), answered(reply, outcome))
            }
            AgentMsg::Deposit { name, item, reply } => {
                let accepted = self.deposit(&name, item);
                (Touched::One(name), answered(reply, accepted))
            }
            AgentMsg::Stop { name, reply } => {
                let stopped = self.stop(&name);
                (Touched::One(name), answered(reply, stopped))
            }
            AgentMsg::Remove { name, reply } => {
                let removed = self.remove(&name);
                (Touched::Every, answered(reply, removed))
            }
        }
    }
}

/// One instance as the application event layer names it: everything an
/// `AgentResource` carries except the server-minted identifier, which only the
/// actor may stamp.
///
/// B4 gives the resource its conversation and its thinking level; what is here
/// is what the registry itself knows.
pub(crate) struct AgentFacts {
    pub name: String,
    pub def: Option<String>,
    pub description: String,
    pub kind: AgentKind,
    pub state: AgentState,
    pub model: String,
    pub provider: String,
    pub thinking: Option<String>,
    pub cwd: PathBuf,
    pub pending: u32,
    pub unacked: u32,
    pub elapsed_ms: Option<u64>,
    pub output_tokens: u64,
    pub tool_uses: u32,
}

/// One message the registry accepted, as the actor reads it back.
///
/// The whole text travels, not the ack's excerpt: this becomes the item the
/// receiver's conversation keeps, and an item that said forty characters of what
/// arrived would be a worse record than the one the model got.
pub(crate) struct Delivered {
    pub id: MsgId,
    pub from: String,
    pub to: String,
    pub text: String,
}

/// Where one direct message stands, as the delivery resource names it.
///
/// The domain's own vocabulary is older and one step out: an `Ack` is `Queued`
/// while it sits in the receiver's inbox and `Delivered` once the receiver's run
/// folded it into a prompt. On the wire those are *delivered* and *read* — the
/// two moments D135 separated, named for what each one means to the sender.
pub(crate) struct DeliveryFact {
    pub id: MsgId,
    pub from: String,
    pub to: String,
    pub state: AckState,
    pub follow_ups: u8,
}

impl AgentRegistry {
    /// The messages accepted since the last look. Draining is the point: each
    /// one becomes exactly one item.
    pub(crate) fn drain_delivered(&mut self) -> Vec<Delivered> {
        std::mem::take(&mut self.delivered)
    }

    /// Where every recorded message stands. Ordered by identifier, so a diff
    /// against the last look is stable.
    pub(crate) fn delivery_facts(&self) -> Vec<DeliveryFact> {
        let mut out: Vec<DeliveryFact> = self
            .inner
            .iter()
            .flat_map(|(name, entry)| {
                entry.acks.iter().map(move |ack| DeliveryFact {
                    id: ack.id,
                    from: ack.from.clone(),
                    to: name.clone(),
                    state: ack.state.clone(),
                    follow_ups: ack.follow_ups,
                })
            })
            .collect();
        out.sort_by_key(|fact| fact.id.0);
        out
    }

    /// What every instance stands at, for the actor to turn into events.
    pub(crate) fn facts(&self) -> Vec<AgentFacts> {
        let mut out: Vec<AgentFacts> = self
            .inner
            .iter()
            .map(|(name, entry)| {
                let progress = entry.progress.as_ref().map(|progress| {
                    progress
                        .lock()
                        .unwrap_or_else(|err| err.into_inner())
                        .clone()
                });
                AgentFacts {
                    name: name.clone(),
                    def: entry.def.clone(),
                    description: entry.description.clone(),
                    kind: entry.kind,
                    state: entry.state,
                    model: entry.session.runtime.model.borrow().clone(),
                    provider: entry.session.runtime.provider.borrow().clone(),
                    thinking: entry.session.runtime.thinking.borrow().clone(),
                    cwd: entry.session.cwd(),
                    pending: entry.inbox.len() as u32,
                    unacked: entry
                        .acks
                        .iter()
                        .filter(|a| a.state.is_outstanding())
                        .count() as u32,
                    elapsed_ms: progress
                        .as_ref()
                        .and_then(|p| p.started_at.map(|at| at.elapsed().as_millis() as u64)),
                    output_tokens: progress.as_ref().map_or(0, |p| p.output_tokens),
                    tool_uses: progress.as_ref().map_or(0, |p| p.tool_uses) as u32,
                }
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// An answer held back until the view carrying its effect has been published.
type Answered = Box<dyn FnOnce()>;

fn answered<T: 'static>(reply: oneshot::Sender<T>, value: T) -> Option<Answered> {
    Some(Box::new(move || {
        let _ = reply.send(value);
    }))
}

/// A new instance, as it is handed to the actor. Boxed with the rest of the
/// wide messages so one variant does not set the size of the whole enum.
pub struct Insertion {
    name: String,
    kind: AgentKind,
    def: Option<String>,
    description: String,
    session: Arc<Session>,
    reply: oneshot::Sender<()>,
}

/// An instance being re-pointed at a freshly built session (D69).
pub struct Refreshing {
    name: String,
    def: Option<String>,
    description: String,
    session: Arc<Session>,
    reply: oneshot::Sender<Refresh>,
}

/// A run's abort handle, offered to the entry that owns the run.
pub struct Aborting {
    name: String,
    run: u64,
    abort: tokio::task::AbortHandle,
    items: Vec<InboxItem>,
    reply: oneshot::Sender<bool>,
}

/// One message written to an instance.
pub struct Delivery {
    name: String,
    from: String,
    message: String,
    images: Vec<crate::api::types::ImageAttachment>,
    ack_timeout: Option<Duration>,
    reply: oneshot::Sender<Result<MsgId, String>>,
}

impl Delivery {
    /// One delivery and the answer to it, for a caller that is already inside the
    /// actor and so cannot ask itself through [`AgentHandle::deliver`].
    pub(crate) fn build(
        name: &str,
        from: &str,
        message: &str,
        ack_timeout: Option<Duration>,
    ) -> (AgentMsg, oneshot::Receiver<Result<MsgId, String>>) {
        let (reply, answer) = oneshot::channel();
        (
            AgentMsg::Deliver(Box::new(Self {
                name: name.to_string(),
                from: from.to_string(),
                message: message.to_string(),
                images: Vec::new(),
                ack_timeout,
                reply,
            })),
            answer,
        )
    }
}

/// What the actor is told about the instances, and what it is asked.
pub enum AgentMsg {
    AttachShare(Arc<crate::share::ShareStore>),
    DetachShare,
    AttachAsk(Arc<crate::query::AskFn>),
    SetEvents(crate::ui::EventSink),
    ClaimName {
        base: String,
        reply: oneshot::Sender<String>,
    },
    Insert(Box<Insertion>),
    Refresh(Box<Refreshing>),
    ReleaseHires {
        reply: oneshot::Sender<Vec<String>>,
    },
    SetPrompt {
        name: String,
        prompt: String,
        reply: oneshot::Sender<()>,
    },
    SetProgress {
        name: String,
        progress: Option<Arc<Mutex<AgentProgress>>>,
        reply: oneshot::Sender<()>,
    },
    Touch {
        name: String,
    },
    SetPermissionMode {
        name: String,
        mode: crate::permission::PermissionMode,
        reply: oneshot::Sender<bool>,
    },
    ReplaceHistory {
        name: String,
        history: Vec<Message>,
        reply: oneshot::Sender<bool>,
    },
    SetAbortIfRunning(Box<Aborting>),
    NextRun {
        name: String,
        reply: oneshot::Sender<u64>,
    },
    SetRunWatch {
        name: String,
        id: crate::watch::WatchId,
    },
    Finish {
        name: String,
        history: Vec<Message>,
        output_chars: usize,
        reply: oneshot::Sender<Option<Continuation>>,
    },
    FlushPending {
        reply: oneshot::Sender<Vec<Wake>>,
    },
    TakeRunning {
        name: String,
        output_chars: usize,
        reply: oneshot::Sender<Vec<InboxItem>>,
    },
    RestoreInbox {
        name: String,
        items: Vec<InboxItem>,
    },
    AcceptsRun {
        name: String,
        run: u64,
        reply: oneshot::Sender<bool>,
    },
    MarkIdle {
        name: String,
    },
    Deliver(Box<Delivery>),
    FollowUp {
        name: String,
        id: MsgId,
        reply: oneshot::Sender<FollowUp>,
    },
    Deposit {
        name: String,
        item: InboxItem,
        reply: oneshot::Sender<bool>,
    },
    Stop {
        name: String,
        reply: oneshot::Sender<Result<(Option<crate::watch::WatchId>, usize), String>>,
    },
    Remove {
        name: String,
        reply: oneshot::Sender<Result<(Option<crate::watch::WatchId>, usize), String>>,
    },
}

/// Build the registry and the handle everything reaches it by.
pub(crate) fn attach(control: mpsc::UnboundedSender<Control>) -> (AgentRegistry, AgentHandle) {
    let (inbox_tx, inbox) = tokio::sync::watch::channel(0);
    let (view, reader) = tokio::sync::watch::channel(Arc::new(Roster::default()));
    let registry = AgentRegistry {
        inner: HashMap::new(),
        share: None,
        saver: None,
        ask: None,
        events: None,
        next_msg: 1,
        delivered: Vec::new(),
        inbox_tx,
        view,
    };
    let handle = AgentHandle {
        control,
        view: reader,
        inbox,
    };
    (registry, handle)
}

/// How everything outside the actor reaches the instances.
///
/// The method names and their meanings are the ones the registry has always had:
/// a report is an ordered send nobody waits on, a question is a send plus the
/// answer coming back — `.await` from an engine task, `.now()` at the terminal
/// front end's synchronous seams — and a listing is a borrow of the published
/// snapshot.
#[derive(Clone)]
pub struct AgentHandle {
    control: mpsc::UnboundedSender<Control>,
    view: tokio::sync::watch::Receiver<Arc<Roster>>,
    /// Inbox generation. Handed out rather than asked for: a run waits on it
    /// between tool rounds, and a wait is not a question.
    inbox: tokio::sync::watch::Receiver<u64>,
}

impl AgentHandle {
    fn report(&self, message: AgentMsg) {
        let _ = self.control.send(Control::Agents(message));
    }

    fn ask<T>(&self, build: impl FnOnce(oneshot::Sender<T>) -> AgentMsg, gone: T) -> Answer<T> {
        let (reply, answer) = oneshot::channel();
        let _ = self.control.send(Control::Agents(build(reply)));
        Answer::new(answer, gone)
    }

    fn row<T>(&self, name: &str, read: impl FnOnce(&AgentRow) -> T) -> Option<T> {
        self.view.borrow().instances.get(name).map(|row| read(row))
    }

    pub fn subscribe_inbox(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inbox.clone()
    }

    /// Replace share persistence for future instance changes.
    pub fn attach_share(&self, store: Arc<crate::share::ShareStore>) {
        self.report(AgentMsg::AttachShare(store));
    }

    pub fn detach_share(&self) {
        self.report(AgentMsg::DetachShare);
    }

    #[cfg(test)]
    pub fn has_share(&self) -> bool {
        self.view.borrow().shared
    }

    /// Attach the prompt surface subagents borrow (called once by whoever owns the UI).
    pub fn attach_ask(&self, ask: Arc<crate::query::AskFn>) {
        self.report(AgentMsg::AttachAsk(ask));
    }

    pub fn ask_fn(&self) -> Option<Arc<crate::query::AskFn>> {
        self.view.borrow().ask.clone()
    }

    /// Attach the console's event channel (the front end does this once).
    pub fn set_events(&self, events: crate::ui::EventSink) {
        self.report(AgentMsg::SetEvents(events));
    }

    /// A sink bound to `name`'s conversation, or `None` with no front end
    /// attached — an embedded or headless run, whose turns nobody is watching.
    pub fn sink_for(&self, name: &str) -> Option<crate::ui::EventSink> {
        self.view
            .borrow()
            .events
            .as_ref()
            .map(|sink| sink.bound_to(crate::ui::ConvKey::Agent(name.to_string())))
    }

    /// Claim an instance name: use the base name when free, otherwise append `-2`/`-3`…
    pub fn claim_name(&self, base: &str) -> Answer<String> {
        let base = base.to_string();
        self.ask(
            |reply| AgentMsg::ClaimName { base, reply },
            "agent".to_string(),
        )
    }

    /// Register a new instance (state=Running). The name must first go through
    /// claim_name.
    ///
    /// A question rather than a report, though it answers with nothing: what a
    /// caller does next is read the roster — `/team` opens the new member's
    /// rooms, a spawn starts its first run — and a listing taken before the
    /// insert had landed would not have it in it.
    pub fn insert(
        &self,
        name: &str,
        kind: AgentKind,
        def: Option<String>,
        description: String,
        session: Arc<Session>,
    ) -> Answer<()> {
        let name = name.to_string();
        self.ask(
            |reply| {
                AgentMsg::Insert(Box::new(Insertion {
                    name,
                    kind,
                    def,
                    description,
                    session,
                    reply,
                }))
            },
            (),
        )
    }

    /// Re-point an existing instance at a freshly built session (D69).
    pub fn refresh(
        &self,
        name: &str,
        def: Option<String>,
        description: String,
        session: Arc<Session>,
    ) -> Answer<Refresh> {
        let name = name.to_string();
        self.ask(
            |reply| {
                AgentMsg::Refresh(Box::new(Refreshing {
                    name,
                    def,
                    description,
                    session,
                    reply,
                }))
            },
            Refresh::Missing,
        )
    }

    /// Release the hires whose task is done, returning the names taken away.
    pub fn release_hires(&self) -> Answer<Vec<String>> {
        self.ask(|reply| AgentMsg::ReleaseHires { reply }, Vec::new())
    }

    /// Both of these are questions that answer with nothing, for the reason
    /// `insert` is: what a caller does next is draw the instance, and a row
    /// drawn before the change landed would show the run without its task.
    pub fn set_prompt(&self, name: &str, prompt: String) -> Answer<()> {
        let name = name.to_string();
        self.ask(
            |reply| AgentMsg::SetPrompt {
                name,
                prompt,
                reply,
            },
            (),
        )
    }

    pub fn set_progress(
        &self,
        name: &str,
        progress: Option<Arc<Mutex<AgentProgress>>>,
    ) -> Answer<()> {
        let name = name.to_string();
        self.ask(
            |reply| AgentMsg::SetProgress {
                name,
                progress,
                reply,
            },
            (),
        )
    }

    #[cfg(test)]
    pub fn set_progress_snapshot(&self, name: &str, progress: AgentProgress) -> Answer<()> {
        self.set_progress(name, Some(Arc::new(Mutex::new(progress))))
    }

    pub fn touch(&self, name: &str) {
        self.report(AgentMsg::Touch {
            name: name.to_string(),
        });
    }

    /// Instance view data (None if the instance doesn't exist).
    pub fn view_of(&self, name: &str) -> Option<AgentView> {
        self.row(name, |row| {
            (row.history.clone(), row.stamps.clone(), row.state)
        })
    }

    /// Whether an instance belongs to the given project directory.
    pub fn is_in_project(&self, name: &str, cwd: &Path) -> bool {
        self.row(name, |row| row.session.cwd() == cwd)
            .unwrap_or(false)
    }

    /// Instance depth (channel cohort check: only direct subagents with depth==1 may join a channel).
    pub fn depth_of(&self, name: &str) -> Option<usize> {
        self.row(name, |row| row.session.depth)
    }

    /// The permission mode this instance runs under (D105).
    pub fn permission_mode_of(&self, name: &str) -> Option<crate::permission::PermissionMode> {
        self.row(name, |row| row.session.permission_mode)
    }

    /// Point an instance at a session carrying `mode`, and say whether that
    /// changed anything.
    pub fn set_permission_mode(
        &self,
        name: &str,
        mode: crate::permission::PermissionMode,
    ) -> Answer<bool> {
        let name = name.to_string();
        self.ask(
            |reply| AgentMsg::SetPermissionMode { name, mode, reply },
            false,
        )
    }

    /// The session an instance runs on.
    pub fn session_of(&self, name: &str) -> Option<Arc<Session>> {
        self.row(name, |row| row.session.clone())
    }

    /// Replace an instance's stored context with a rewritten one — `/compact`
    /// on its page, and nothing else. Refused while it is running.
    pub fn replace_history(&self, name: &str, history: Vec<Message>) -> Answer<bool> {
        let name = name.to_string();
        self.ask(
            |reply| AgentMsg::ReplaceHistory {
                name,
                history,
                reply,
            },
            false,
        )
    }

    pub fn set_abort_if_running(
        &self,
        name: &str,
        run: u64,
        abort: tokio::task::AbortHandle,
        items: Vec<InboxItem>,
    ) -> Answer<bool> {
        let name = name.to_string();
        self.ask(
            |reply| {
                AgentMsg::SetAbortIfRunning(Box::new(Aborting {
                    name,
                    run,
                    abort,
                    items,
                    reply,
                }))
            },
            false,
        )
    }

    /// Next run sequence number (starting at 1).
    pub fn next_run(&self, name: &str) -> Answer<u64> {
        let name = name.to_string();
        self.ask(|reply| AgentMsg::NextRun { name, reply }, 1)
    }

    /// Record the watch line of the current turn.
    pub fn set_run_watch(&self, name: &str, id: crate::watch::WatchId) {
        self.report(AgentMsg::SetRunWatch {
            name: name.to_string(),
            id,
        });
    }

    /// Turn finished: store the latest history, and say whether the inbox
    /// refilled while it ran.
    pub fn finish(
        &self,
        name: &str,
        history: Vec<Message>,
        output_chars: usize,
    ) -> Answer<Option<Continuation>> {
        let name = name.to_string();
        self.ask(
            |reply| AgentMsg::Finish {
                name,
                history,
                output_chars,
                reply,
            },
            None,
        )
    }

    /// Claim every idle instance holding mail (v7).
    pub fn flush_pending(&self) -> Answer<Vec<Wake>> {
        self.ask(|reply| AgentMsg::FlushPending { reply }, Vec::new())
    }

    /// Put a direct message into one running instance's current query.
    pub fn take_running(&self, name: &str, output_chars: usize) -> Answer<Vec<InboxItem>> {
        let name = name.to_string();
        self.ask(
            |reply| AgentMsg::TakeRunning {
                name,
                output_chars,
                reply,
            },
            Vec::new(),
        )
    }

    /// Turn failed before the claimed batch completed: restore it to the front of the inbox.
    pub fn restore_inbox(&self, name: &str, items: Vec<InboxItem>) {
        self.report(AgentMsg::RestoreInbox {
            name: name.to_string(),
            items,
        });
    }

    /// A claimed idle run may be stopped before its task is spawned. Asking the
    /// actor rather than the published roster is what closes that gap: the
    /// answer is the state at the moment the question was served.
    pub fn accepts_run(&self, name: &str, run: u64) -> Answer<bool> {
        let name = name.to_string();
        self.ask(|reply| AgentMsg::AcceptsRun { name, run, reply }, false)
    }

    /// Turn failed: keep the pre-failure history, switch to Idle (retryable via SendMessage).
    pub fn mark_idle(&self, name: &str) {
        self.report(AgentMsg::MarkIdle {
            name: name.to_string(),
        });
    }

    /// Queue a message for an instance. Returns the message id — the receipt the
    /// sender uses to check the outcome later.
    pub fn deliver(
        &self,
        name: &str,
        from: &str,
        message: &str,
        images: Vec<crate::api::types::ImageAttachment>,
        ack_timeout: Option<Duration>,
    ) -> Answer<Result<MsgId, String>> {
        let name = name.to_string();
        let from = from.to_string();
        let message = message.to_string();
        self.ask(
            |reply| {
                AgentMsg::Deliver(Box::new(Delivery {
                    name,
                    from,
                    message,
                    images,
                    ack_timeout,
                    reply,
                }))
            },
            Err(SESSION_ENDED.to_string()),
        )
    }

    /// Re-read one message's record and, while the sender is still owed an
    /// answer, put a follow-up in the receiver's inbox.
    pub fn follow_up(&self, name: &str, id: MsgId) -> Answer<FollowUp> {
        let name = name.to_string();
        self.ask(
            |reply| AgentMsg::FollowUp { name, id, reply },
            FollowUp::Gone,
        )
    }

    /// Queue a channel message. A stopped member is silently skipped.
    pub fn deposit(&self, name: &str, item: InboxItem) -> Answer<bool> {
        let name = name.to_string();
        self.ask(|reply| AgentMsg::Deposit { name, item, reply }, false)
    }

    /// Delivery records for one instance, newest last (None = no such instance).
    pub fn acks_of(&self, name: &str) -> Option<Vec<Ack>> {
        self.row(name, |row| row.acks.clone())
    }

    /// Direct messages claimed by the current run and not yet landed in
    /// history. The registry's own bookkeeping, asserted in tests for the same
    /// reason [`AgentHandle::pending_of`] is.
    #[cfg(test)]
    pub fn in_flight_of(&self, name: &str) -> Vec<(String, String)> {
        self.row(name, |row| {
            row.in_flight
                .iter()
                .map(|(_, from, text)| (from.clone(), text.clone()))
                .collect()
        })
        .unwrap_or_default()
    }

    /// Direct messages still sitting in the inbox, in order, each with its
    /// sender.
    #[cfg(test)]
    pub fn pending_of(&self, name: &str) -> Vec<(String, String)> {
        self.row(name, |row| {
            row.inbox
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

    /// Stop: abort a running turn, no longer accept commands; history is kept
    /// and listable.
    pub fn stop(
        &self,
        name: &str,
    ) -> Answer<Result<(Option<crate::watch::WatchId>, usize), String>> {
        let name = name.to_string();
        self.ask(
            |reply| AgentMsg::Stop { name, reply },
            Err(SESSION_ENDED.to_string()),
        )
    }

    /// Remove: stop first, then drop the entry (name released).
    pub fn remove(
        &self,
        name: &str,
    ) -> Answer<Result<(Option<crate::watch::WatchId>, usize), String>> {
        let name = name.to_string();
        self.ask(
            |reply| AgentMsg::Remove { name, reply },
            Err(SESSION_ENDED.to_string()),
        )
    }

    /// Snapshot of all instances (sorted by name for stable list output).
    pub fn list(&self) -> Vec<AgentStatus> {
        // The instances are already in name order: the roster holds them in a
        // sorted map so no reader has to impose one.
        self.view
            .borrow()
            .instances
            .iter()
            .map(|(name, row)| {
                // Sampled here rather than stored: a run's token count and its
                // elapsed time move without the registry hearing about it, and a
                // row that froze them would stop the clock on a live turn.
                let progress = row.progress.as_ref().map(|progress| {
                    progress
                        .lock()
                        .unwrap_or_else(|err| err.into_inner())
                        .clone()
                });
                AgentStatus {
                    name: name.clone(),
                    def: row.def.clone(),
                    description: row.description.clone(),
                    prompt: row.prompt.clone(),
                    state: row.state,
                    kind: row.kind,
                    pending: row.inbox.len(),
                    unacked: row.acks.iter().filter(|a| a.state.is_outstanding()).count(),
                    model: row.session.runtime.model.borrow().clone(),
                    provider: row.session.runtime.provider.borrow().clone(),
                    thinking: row.session.runtime.thinking.borrow().clone(),
                    cwd: row.session.cwd(),
                    elapsed: progress
                        .as_ref()
                        .and_then(|p| p.started_at.map(|started| started.elapsed())),
                    output_tokens: progress.as_ref().map_or(0, |p| p.output_tokens),
                    tool_uses: progress.as_ref().map_or(0, |p| p.tool_uses),
                    recent_activity: progress.map_or_else(Vec::new, |p| p.recent_activity),
                    last_active: row.last_active,
                }
            })
            .collect()
    }

    /// Wait until everything already sent has been applied.
    #[cfg(test)]
    pub async fn settle(&self) {
        crate::app::controller::settle(&self.control).await;
    }

    /// The same barrier for a synchronous test.
    #[cfg(test)]
    pub fn settle_now(&self) {
        crate::app::controller::settle_now(&self.control);
    }
}

/// What a question answers with once the session is over. Unreachable while any
/// handle lives, since the actor outlives them all.
const SESSION_ENDED: &str = "the session has ended";

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

/// Whether this sender reads the receiver's turn text — which decides what
/// counts as an answer to them (D137).
///
/// Main does: a run's result is delivered to whoever started it. The user does:
/// it is on the page they are watching. **A colleague does neither.** Its
/// message arrived in an inbox and its answer has to arrive in one too, so
/// "they produced some text" is evidence of nothing where a peer is concerned —
/// and treating it as an answer would close the very record the sender relies
/// on to find out they were never answered.
fn reads_turn_text(from: &str) -> bool {
    from == crate::channels::MAIN_NAME || from == crate::channels::USER_NAME
}

/// A reply answers a delivered message only if text was produced after that message entered the
/// query. Anything still queued is untouched: it has not been read yet.
fn answer_acks(entry: &mut Entry, output_chars: usize) {
    let run = entry.runs;
    for ack in entry.acks.iter_mut() {
        if !reads_turn_text(&ack.from) {
            continue;
        }
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

/// `sender` just wrote to `recipient`: whatever `recipient` had asked them and
/// they had already read is answered by it (D137).
///
/// The peer half of [`answer_acks`], and deliberately the same precondition —
/// only a *delivered* message is settled. A message still queued has not been
/// read, and an unrelated send while it waits is not an answer to something the
/// sender has not seen.
fn settle_peer_acks(entry: &mut Entry, recipient: &str) {
    let run = entry.runs;
    for ack in entry.acks.iter_mut() {
        if ack.from == recipient && matches!(ack.state, AckState::Delivered { .. }) {
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
#[path = "agents_tests.rs"]
mod tests;

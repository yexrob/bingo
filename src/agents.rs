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
    /// Turn in progress (new messages queue and are delivered automatically at turn end).
    Running,
    /// Waiting for a command (SendMessage wakes it immediately; history is kept).
    Idle,
    /// Stopped (no longer receives messages; the name is released after delete).
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

/// Snapshot for list.
#[derive(Debug, Clone)]
pub struct AgentStatus {
    pub name: String,
    pub def: Option<String>,
    pub description: String,
    pub state: AgentState,
    /// Messages waiting in the inbox for the next turn boundary.
    pub pending: usize,
    /// Messages the sender has had no reply to yet — queued, or read and left unanswered.
    pub unacked: usize,
    /// The engine this instance actually runs on. Worth reporting because it need
    /// not be the session's: a definition or a team blueprint can pin a different
    /// one per instance, and "which member is on which model" is otherwise
    /// invisible until the bill arrives.
    pub model: String,
    pub provider: String,
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
    /// Run #N ended with a reply for the hub, which answers this message (not necessarily the run
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

/// Inbox item: a direct hub command, or a channel message (injected in batch on wake, in order).
#[derive(Debug, Clone)]
pub enum InboxItem {
    Direct {
        id: MsgId,
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
    /// Automatic chase for a direct message the hub never got an answer to. It carries no new
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
}

/// A run the caller should start: the instance was idle with a non-empty inbox, and this call
/// claimed it (state is already Running, inbox already drained) — so two flushes can't
/// double-start the same instance.
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
    state: AgentState,
    /// Full message history since the last completed turn (continuation context).
    history: Vec<Message>,
    /// Inbox accumulated since the last drain (commands + channel messages, injected as one
    /// batch at a turn boundary — never one message per turn).
    inbox: Vec<InboxItem>,
    /// Delivery records for direct messages, oldest first, capped at MAX_ACKS.
    acks: Vec<Ack>,
    session: Arc<Session>,
    abort: Option<tokio::task::AbortHandle>,
    /// Cumulative run count (watch lines are labeled `#N`).
    runs: u64,
    /// Watch line of the current turn (used to set Cancelled on stop/delete).
    watch_id: Option<crate::watch::WatchId>,
    /// Streaming output of the current turn (shares the same Arc with subagent_hooks;
    /// cleared at turn end — the TUI instance view shows the live tail from this).
    live: Option<Arc<Mutex<Vec<LiveBlock>>>>,
}

/// One piece of a running turn, as the instance view sees it while it happens.
///
/// A running turn used to reach the view as one flat string of text deltas, which
/// showed neither the tool calls between rounds nor the boundaries between them —
/// so a five-round turn read as one wall with sentences butting together
/// (`…the current state.Now let me verify…`). The finished history has always
/// carried both; this is what lets the live view say the same thing before the
/// turn ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveBlock {
    /// Assistant prose, one block per round.
    Text(String),
    /// A tool call, already rendered the way the transcript renders one.
    Tool(String),
}

impl LiveBlock {
    /// Append streamed text, continuing the open prose block or opening one.
    pub fn push_text(blocks: &mut Vec<LiveBlock>, text: &str) {
        match blocks.last_mut() {
            Some(LiveBlock::Text(open)) => open.push_str(text),
            _ => blocks.push(LiveBlock::Text(text.to_string())),
        }
    }
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
    /// Monotonic message id source (registry-wide, so ids never collide across instances).
    next_msg: std::sync::atomic::AtomicU64,
}

impl AgentRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            share: Mutex::new(None),
            ask: Mutex::new(None),
            next_msg: std::sync::atomic::AtomicU64::new(1),
        })
    }

    fn mint_msg_id(&self) -> MsgId {
        MsgId(
            self.next_msg
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Attach share persistence: instance create/finish/stop events sync into the share document from now on.
    pub fn attach_share(&self, store: Arc<crate::share::ShareStore>) {
        *self.share.lock().unwrap_or_else(|e| e.into_inner()) = Some(store);
    }

    /// Attach the prompt surface subagents borrow (called once by whoever owns the UI).
    pub fn attach_ask(&self, ask: Arc<crate::query::AskFn>) {
        *self.ask.lock().unwrap_or_else(|e| e.into_inner()) = Some(ask);
    }

    pub fn ask_fn(&self) -> Option<Arc<crate::query::AskFn>> {
        self.ask.lock().unwrap_or_else(|e| e.into_inner()).clone()
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
    /// `main`/`user` are reserved for the hub and the user (channel member names) and
    /// are never handed out.
    pub fn claim_name(&self, base: &str) -> String {
        let base = if base.trim().is_empty() {
            "agent"
        } else {
            base.trim()
        };
        let taken = |inner: &HashMap<String, Entry>, name: &str| {
            name == crate::channels::HUB_NAME
                || name == crate::channels::USER_NAME
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
        def: Option<String>,
        description: String,
        session: Arc<Session>,
    ) {
        self.lock().insert(
            name.to_string(),
            Entry {
                def,
                description,
                state: AgentState::Running,
                history: Vec::new(),
                inbox: Vec::new(),
                acks: Vec::new(),
                session,
                abort: None,
                runs: 0,
                watch_id: None,
                live: None,
            },
        );
        self.sync_share(name);
    }

    /// Streaming output buffer of the current turn (attached at turn start, detached at turn end).
    pub fn set_live(&self, name: &str, live: Option<Arc<Mutex<Vec<LiveBlock>>>>) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.live = live;
        }
    }

    /// Instance view data: history + live tail + state (None if the instance doesn't exist).
    pub fn view_of(&self, name: &str) -> Option<(Vec<Message>, Vec<LiveBlock>, AgentState)> {
        let inner = self.lock();
        let entry = inner.get(name)?;
        let live = entry
            .live
            .as_ref()
            .map(|l| l.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .unwrap_or_default();
        Some((entry.history.clone(), live, entry.state))
    }

    /// Instance depth (channel cohort check: only direct subagents with depth==1 may join a channel).
    pub fn depth_of(&self, name: &str) -> Option<usize> {
        self.lock().get(name).map(|e| e.session.depth)
    }

    pub fn set_abort(&self, name: &str, abort: tokio::task::AbortHandle) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.abort = Some(abort);
        }
    }

    /// Next run sequence number (starting at 1).
    pub fn next_run(&self, name: &str) -> u64 {
        match self.lock().get_mut(name) {
            Some(entry) => {
                entry.runs += 1;
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
    /// `spoke` is whether the turn produced any text for the hub. Only then are the messages this
    /// run carried acknowledged: a turn that ends in silence is the case the sender most needs to
    /// hear about, so it must not look like one that answered.
    pub fn finish(&self, name: &str, history: Vec<Message>, spoke: bool) -> Option<Continuation> {
        let result = {
            let mut inner = self.lock();
            let entry = inner.get_mut(name)?;
            entry.history = history;
            if spoke {
                answer_acks(entry);
            }
            if entry.state == AgentState::Stopped {
                None
            } else if entry.inbox.is_empty() {
                entry.state = AgentState::Idle;
                None
            } else {
                let items = drain_inbox(entry);
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

    /// Turn-boundary batch delivery: every idle instance with a non-empty inbox is claimed
    /// (flipped to Running, inbox drained in one pass) and handed back for the caller to run.
    ///
    /// Delivery is deliberately not immediate. Several messages sent within one turn all land in
    /// the inbox first and are folded into a *single* prompt here — waking on the first one would
    /// make the receiver process them one at a time. It also means a run chain that died with
    /// messages still queued gets picked up at the next boundary instead of stranding them.
    pub fn flush_pending(&self) -> Vec<Wake> {
        let mut woken = Vec::new();
        {
            let mut inner = self.lock();
            for (name, entry) in inner.iter_mut() {
                if entry.state != AgentState::Idle || entry.inbox.is_empty() {
                    continue;
                }
                let items = drain_inbox(entry);
                entry.state = AgentState::Running;
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

    /// Turn failed: keep the pre-failure history, switch to Idle (retryable via SendMessage).
    pub fn mark_idle(&self, name: &str) {
        if let Some(entry) = self.lock().get_mut(name)
            && entry.state != AgentState::Stopped
        {
            entry.state = AgentState::Idle;
        }
    }

    /// Deliver a hub command: queue when Running; wake when Idle (returns the session,
    /// history and drained inbox needed to continue); error when Stopped/unknown.
    /// Queue a hub command. Returns the message id — the receipt the sender uses to check the
    /// outcome later; delivery itself happens at the next turn boundary (see `flush_pending`).
    /// `ack_timeout` records the wait the sender allowed before the acknowledgement is chased
    /// (see `follow_up`); it is a note on the record, not a timer — the caller owns the clock.
    pub fn deliver(
        &self,
        name: &str,
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
            return Err(format!(
                "{name} is stopped and no longer accepts instructions (delete removes the instance)"
            ));
        }
        entry.inbox.push(InboxItem::Direct {
            id,
            text: message.to_string(),
            images,
        });
        push_ack(
            entry,
            Ack {
                id,
                excerpt: first_line(message),
                state: AckState::Queued,
                queued_at: Instant::now(),
                timeout: ack_timeout,
                follow_ups: 0,
            },
        );
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
        FollowUp::Sent { round }
    }

    /// Queue a channel message. A stopped member is silently skipped — a broadcast doesn't fail
    /// because one member stopped. Returns whether it was accepted.
    pub fn deposit(&self, name: &str, item: InboxItem) -> bool {
        let mut inner = self.lock();
        let Some(entry) = inner.get_mut(name) else {
            return false;
        };
        if entry.state == AgentState::Stopped {
            return false;
        }
        entry.inbox.push(item);
        true
    }

    /// Delivery records for one instance, newest last (None = no such instance).
    pub fn acks_of(&self, name: &str) -> Option<Vec<Ack>> {
        Some(self.lock().get(name)?.acks.clone())
    }

    /// Direct messages still sitting in the inbox, in order. The DM view renders
    /// them after the history so a message you just sent stays on screen until
    /// the turn boundary folds it into the transcript.
    pub fn pending_of(&self, name: &str) -> Vec<String> {
        self.lock()
            .get(name)
            .map(|entry| {
                entry
                    .inbox
                    .iter()
                    .filter_map(|item| match item {
                        InboxItem::Direct { text, .. } => Some(text.clone()),
                        // A follow-up is the harness chasing an acknowledgement, not something
                        // the sender wrote — rendering it as their pending message would lie.
                        InboxItem::Channel { .. } | InboxItem::FollowUp { .. } => None,
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
            .map(|(name, e)| AgentStatus {
                name: name.clone(),
                def: e.def.clone(),
                description: e.description.clone(),
                state: e.state,
                pending: e.inbox.len(),
                unacked: e.acks.iter().filter(|a| a.state.is_outstanding()).count(),
                model: e.session.runtime.model.borrow().clone(),
                provider: e.session.runtime.provider.borrow().clone(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

/// A turn that produced text answers every message the instance has read so far — replying is a
/// turn-level act, not a per-message one, and a message first read in a silent run is answered by
/// the run that finally speaks. Anything still queued is untouched: it has not been read yet.
fn answer_acks(entry: &mut Entry) {
    let run = entry.runs;
    for ack in entry.acks.iter_mut() {
        if matches!(ack.state, AckState::Delivered { .. }) {
            ack.state = AckState::Answered { run };
        }
    }
}

/// Take the whole inbox in one pass and mark every direct message in it delivered: being folded
/// into the next prompt is exactly the moment a message enters the receiver's context.
fn drain_inbox(entry: &mut Entry) -> Vec<InboxItem> {
    let items = std::mem::take(&mut entry.inbox);
    entry.runs += 1;
    let run = entry.runs;
    for item in &items {
        if let InboxItem::Direct { id, .. } = item {
            set_ack(entry, *id, AckState::Delivered { run });
        }
    }
    items
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
        reg.insert("reviewer", None, "r".into(), test_session());
        assert_eq!(reg.claim_name("reviewer"), "reviewer-2");
        reg.insert("reviewer-2", None, "r".into(), test_session());
        assert_eq!(reg.claim_name("reviewer"), "reviewer-3");
    }

    #[test]
    fn lifecycle_running_idle_queue_and_revive() {
        let reg = AgentRegistry::new();
        reg.insert("scout", None, "research".into(), test_session());
        // Running: message queued (delivery never happens inside deliver itself).
        let first = reg
            .deliver("scout", "add A", Vec::new(), None)
            .unwrap_or_else(|e| panic!("{e}"));
        // Turn finished + inbox non-empty → continues (history saved, inbox drained, ack set).
        let next = reg
            .finish("scout", vec![Message::user_text("hi")], true)
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
        assert!(reg.finish("scout", Vec::new(), true).is_none());
        assert_eq!(reg.list()[0].state, AgentState::Idle);
        // Idle: the message waits for a flush rather than starting a run on the spot.
        let _ = reg
            .deliver("scout", "look at B again", Vec::new(), None)
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

    #[test]
    fn inbox_accumulates_direct_and_channel_items_in_order() {
        let reg = AgentRegistry::new();
        reg.insert("w", None, "w".into(), test_session());
        let _ = reg.deliver("w", "do 1 first", Vec::new(), None);
        assert!(reg.deposit(
            "w",
            InboxItem::Channel {
                channel: "t".into(),
                from: "a".into(),
                text: "report".into(),
                seq: 3,
            },
        ));
        let items = reg
            .finish("w", Vec::new(), true)
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
        // Idle: deposit wakes it; Stopped/unknown silently dropped.
        assert!(reg.finish("w", Vec::new(), true).is_none());
        assert!(reg.deposit(
            "w",
            InboxItem::Channel {
                channel: "t".into(),
                from: "b".into(),
                text: "x".into(),
                seq: 4,
            },
        ));
        let woken = reg.flush_pending();
        assert_eq!(woken.len(), 1);
        assert_eq!(woken[0].items.len(), 1);
        let _ = reg.stop("w");
        let dropped = InboxItem::Channel {
            channel: "t".into(),
            from: "c".into(),
            text: "y".into(),
            seq: 5,
        };
        assert!(
            !reg.deposit("w", dropped.clone()),
            "stopped members do not receive"
        );
        assert!(
            !reg.deposit("ghost", dropped),
            "unknown instances are silently dropped"
        );
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
        reg.finish("scout", vec![Message::user_text("hi")], true);
        let doc = store.snapshot();
        assert_eq!(doc.agents[0].state, "idle");
        assert_eq!(doc.agents[0].history.len(), 1);
        assert_eq!(doc.agents[0].history[0], Message::user_text("hi"));

        // A busy non-empty inbox → stays running after finish (Idle wake-up drains the inbox into Start,
        // while Running queues; two instructions create the queue scenario).
        reg.deliver("scout", "check again", Vec::new(), None)
            .unwrap_or_else(|e| panic!("{e}"));
        reg.deliver("scout", "check once more", Vec::new(), None)
            .unwrap_or_else(|e| panic!("{e}"));
        reg.finish("scout", Vec::new(), true);
        let doc = store.snapshot();
        assert_eq!(doc.agents[0].state, "running");
        // Inbox drained → idle.
        reg.finish("scout", Vec::new(), true);
        let doc = store.snapshot();
        assert_eq!(doc.agents[0].state, "idle");

        // stop → stopped.
        reg.stop("scout").unwrap_or_else(|e| panic!("{e}"));
        let doc = store.snapshot();
        assert_eq!(doc.agents[0].state, "stopped");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn hub_name_is_reserved() {
        let reg = AgentRegistry::new();
        assert_eq!(
            reg.claim_name("main"),
            "main-2",
            "main is reserved for the hub"
        );
    }

    /// Several messages sent before a boundary arrive as one batch: the receiver reads them
    /// together instead of burning a turn per message.
    #[test]
    fn messages_sent_in_one_turn_arrive_as_one_batch() {
        let reg = AgentRegistry::new();
        reg.insert("w", None, "w".into(), test_session());
        assert!(
            reg.finish("w", Vec::new(), true).is_none(),
            "turns idle first"
        );
        for text in ["look at A first", "look at B again", "and finally C"] {
            reg.deliver("w", text, Vec::new(), None)
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
        reg.insert("w", None, "w".into(), test_session());
        let id = reg
            .deliver("w", "is it too late", Vec::new(), None)
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

    /// The chase is bounded and self-cancelling: while a message goes unanswered each round leaves
    /// one follow-up riding with it, the budget stops at MAX_FOLLOW_UPS, and the reply that finally
    /// comes settles every later check.
    #[test]
    fn follow_up_chases_a_queued_message_until_the_budget_runs_out() {
        let reg = AgentRegistry::new();
        reg.insert("w", None, "w".into(), test_session());
        let id = reg
            .deliver(
                "w",
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
            .finish("w", Vec::new(), true)
            .unwrap_or_else(|| panic!("queued messages should be picked up at the turn boundary"))
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
            reg.finish("w", Vec::new(), true).is_none(),
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
        reg.insert("mute", None, "silent".into(), test_session());
        assert!(
            reg.finish("mute", Vec::new(), true).is_none(),
            "turns idle first"
        );
        let id = reg
            .deliver(
                "mute",
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
        // The turn ends producing no text for the hub.
        assert!(reg.finish("mute", Vec::new(), false).is_none());
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
        assert!(reg.finish("mute", Vec::new(), true).is_none());
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
        reg.insert("w", None, "w".into(), test_session());
        let id = reg
            .deliver(
                "w",
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
    /// back to Idle and the next boundary flush picks the batch up.
    #[test]
    fn messages_survive_a_failed_run_and_are_retried() {
        let reg = AgentRegistry::new();
        reg.insert("w", None, "w".into(), test_session());
        reg.deliver("w", "continue", Vec::new(), None)
            .unwrap_or_else(|e| panic!("{e}"));
        // The run failed (spawn_agent_loop's error branch) — it only marks the instance idle.
        reg.mark_idle("w");
        assert_eq!(
            reg.list()[0].pending,
            1,
            "the message is still in the inbox"
        );
        let woken = reg.flush_pending();
        assert_eq!(woken.len(), 1, "the next turn boundary re-delivers");
        assert_eq!(woken[0].items.len(), 1);
    }

    #[test]
    fn stop_and_delete_semantics() {
        let reg = AgentRegistry::new();
        reg.insert("x", None, "x".into(), test_session());
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
        assert!(
            reg.deliver("x", "still there", Vec::new(), None).is_err(),
            "rejected after stop"
        );
        // Turn finishing after a stop: history is still archived, no revival.
        assert!(
            reg.finish("x", vec![Message::user_text("h")], true)
                .is_none()
        );
        assert_eq!(reg.list()[0].state, AgentState::Stopped);
        reg.remove("x").unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.list().is_empty());
        assert_eq!(reg.claim_name("x"), "x", "deletion frees the name");
        assert!(
            reg.deliver("x", "hi", Vec::new(), None).is_err(),
            "unknown instance errors"
        );
        // Stopping an idle instance: no active line.
        reg.insert("y", None, "y".into(), test_session());
        reg.set_run_watch("y", crate::watch::WatchId(9));
        assert!(reg.finish("y", Vec::new(), true).is_none());
        assert!(
            reg.stop("y").unwrap_or_else(|e| panic!("{e}")).0.is_none(),
            "stopping while idle does not cancel a terminal watch line"
        );
    }
}

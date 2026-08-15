//! The conversation engine (D88): every conversation has the same shape.
//!
//! A buffer is one conversation — `@main`, a DM with a subagent, or a room the
//! user is in. The engine holds what is *about* a conversation
//! (how far you have read, whether it wants you, the draft you left in it) and
//! nothing of what is *in* one: a buffer's transcript stays where the domain
//! already keeps it, and [`BufferId`] is the key that reaches it. There is no
//! second copy of any message here, and nothing is written to disk — unread
//! marks and drafts are session-local by construction, and the registry
//! rebuilds itself from the domain on the next start.
//!
//! Two facts about the existing code shape this module more than the design did:
//!
//! - **Unread is derived, not counted.** The workspace has always computed
//!   `seq - read_cursor` fresh on every frame rather than incrementing a
//!   counter (`entity.rs::snapshot`). The engine keeps that: [`Buffers::refresh`]
//!   re-reads the sequence numbers and the badge falls out of the subtraction.
//!   A counter fed by events can drift from the thing it counts; a cursor cannot.
//! - **Nothing replays a stored conversation into rows yet.** `/resume` clears
//!   the transcript and prints a line; `@main`'s row builder (`build_rows`)
//!   reads `Vec<UiMessage>` and only the live flow ever fills it. So
//!   [`Buffers::rehydrate`] produces `UiMessage` — the unit that builder already
//!   consumes — and extracts it with the functions the workspace extracted posts
//!   with ([`dm_posts`], [`channel_posts`], which moved here when the workspace
//!   retired), and that is where the D64 `[DM from user]` rules and the batching
//!   rules live. Writing a second parser next to them would have been the one
//!   thing worth avoiding.
//!
//! [`crate::tui::bufferview`] is the host side: it switches conversations, puts
//! a replay on screen and routes what the composer sends. The extraction rules
//! that turn a domain store into displayable messages live at the bottom of this
//! file, where the workspace skin left them (D89).

use std::sync::Arc;

use crate::api::types::Message;
use crate::channels::{ChannelMessage, USER_NAME};
use crate::query::Session;
use crate::tui::chat::{Role, UiMessage};
use crate::watch::{WatchKind, WatchState};

/// How many lifecycle events the team feed keeps. Spawn/done/ack are broadcast
/// and retained nowhere else, so the feed is the one display-side store with no
/// domain store behind it, and therefore the one that has to bound itself.
const TEAM_LOG_MAX: usize = 200;

/// Which conversation a buffer is.
///
/// The derived ordering *is* the registry's ordering: `@main`, rooms by name,
/// then DMs by name. Declaration order carries it, so there is no second sort
/// key to keep in step with this enum.
///
/// **There is no `Team` variant, and that is the D95 ruling.** The team is the
/// organization, not a conversation: you cannot speak to it, and everything it
/// had to say — who exists, what rooms there are, what just happened — is a
/// directory (`ctrl+t`, [`crate::tui::directory`]) rather than a board you
/// visit and a badge that asks you to. A read-only buffer in the bar was a
/// conversation-shaped hole where a roster belonged.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BufferId {
    /// The **home conversation**: the user↔main dialogue plus the host's own
    /// furniture — the transcript already on screen.
    ///
    /// D101 retired the word "hub" everywhere else, but kept this variant's
    /// spelling on the design doc's explicit ruling: home is the one buffer
    /// whose mechanics are genuinely different (it owns the turn loop, it has
    /// no sequence to read to, it replays nothing, it is never closable), and
    /// a name that says so is worth more than a name that matches its label.
    /// Everything a user or a model reads calls it `@main`.
    Hub,
    /// A room the user is a member of (D29's channel, D95's vocabulary).
    Channel(String),
    /// A direct conversation with one subagent instance.
    Dm(String),
}

impl BufferId {
    /// The name this conversation goes by on screen.
    pub fn label(&self) -> String {
        match self {
            Self::Hub => format!("@{}", crate::channels::MAIN_NAME),
            Self::Channel(name) => format!("#{name}"),
            Self::Dm(name) => format!("@{name}"),
        }
    }

    /// The rule that opens this conversation in the flow, and the one that
    /// closes it. One formatter, so a replay and the hand-back home can
    /// never drift into two shapes.
    pub fn rule(&self) -> String {
        format!("── {} ──", self.label())
    }
}

impl std::fmt::Display for BufferId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label())
    }
}

/// One conversation's accounting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Buffer {
    pub id: BufferId,
    /// How far the source has got, in its own unit: channel `seq`, DM history
    /// length, team-log length. Main has no sequence and stays at 0.
    seq: u64,
    /// How far the user has read, in the same unit.
    read: u64,
    /// Something addressed to the user is waiting.
    mention: bool,
    /// Composer text left behind on the way out (D89 puts it back on the way in).
    draft: String,
    /// Tick of the last observed change in the source.
    last_activity: u64,
}

impl Buffer {
    pub fn id(&self) -> &BufferId {
        &self.id
    }
    pub fn unread(&self) -> u64 {
        self.seq.saturating_sub(self.read)
    }
    pub fn mention(&self) -> bool {
        self.mention
    }
    /// Tick of the last observed change in the source. The switcher orders by
    /// it (D90), which is the reason it is readable at all: a registry sorted
    /// by name answers "what exists", and a reader reaching for ctrl+k is
    /// asking "what just happened".
    pub fn last_activity(&self) -> u64 {
        self.last_activity
    }
}

/// One element of a buffer's replay.
///
/// Not comparable: `UiMessage` carries activities and fold state and has never
/// been an equality type. Tests read the fields they mean.
#[derive(Debug, Clone)]
pub enum Replay {
    /// The rule that opens the conversation on switch.
    Divider(String),
    /// A message from the source. `message` is the main transcript's own unit, so
    /// the existing row builder renders it with the code the live flow uses;
    /// `who` carries the sender main's two roles cannot express, for the
    /// decorations D89 hangs on it.
    Message { who: String, message: UiMessage },
    /// Something that happened in the conversation that nobody said: a room's
    /// membership change, a wake-up the runtime wrote into an instance's
    /// history. One dim line, no name over it and no send stamp beside it,
    /// because there is no sender and nothing was sent — the same row shape a
    /// [`Replay::Divider`] gets, which is what [`PostKind::Note`] always
    /// claimed to be and, until D95, was rendered as a quoted message anyway.
    Note(String),
}

/// Where a composer submit belongs. Data only — [`deliver`] performs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitTarget {
    /// Main's normal turn path.
    Turn(String),
    /// `AgentRegistry::deliver` under the user's name. The `[DM from user]`
    /// marker is *not* applied here and must not be: it is added downstream in
    /// `absorb_inbox`, derived from `from`, and adding it at both ends would
    /// double it (D64).
    Dm { agent: String, text: String },
    /// `tool::channel::deliver_post` under the user's name.
    Channel { channel: String, text: String },
    /// A room the user is only watching (D95): reading is free, speaking is a
    /// membership event, so the composer says how to become a member rather
    /// than posting under a name that is not on the roster.
    Refused(&'static str),
}

/// What the composer says under a room the user is observing. Stated once: the
/// refusal on submit and the standing hint above the prompt are the same
/// sentence, so the answer to "why can't I type here" does not depend on
/// whether you tried.
pub const OBSERVER_HINT: &str = "read-only · /join to speak in this room";

/// What came of a routed submit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// The host owns this one: run the text as a turn.
    Turn(String),
    Sent,
    /// A notice to put above the composer (English; the wording is final).
    Rejected(String),
}

/// One entry in the team feed — the directory's "recent" column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamEvent {
    /// The watch label, which already carries the instance name and run — or
    /// the command that produced the entry, for the board's own output.
    pub label: String,
    /// The lifecycle state this entry reports. `None` means the entry is not a
    /// lifecycle event at all but output posted to the feed (`/team`, D90):
    /// there is no state to name, and naming one anyway would report a
    /// transition that never happened.
    pub state: Option<WatchState>,
    pub detail: Option<String>,
    /// Unix seconds.
    pub at: u64,
}

/// One feed entry as a line.
///
/// A lifecycle event says what happened *and* what was reported, in that order:
/// the detail alone used to be the whole row, so a finished run and a running
/// one were told apart only by what the agent happened to say. Feed output
/// (`/team`) has no state and is its own text.
pub(crate) fn team_line(event: &TeamEvent) -> String {
    match (event.state, &event.detail) {
        (Some(state), Some(detail)) => format!("{} · {detail}", state_word(state)),
        (Some(state), None) => state_word(state).to_string(),
        (None, Some(detail)) => detail.clone(),
        (None, None) => String::new(),
    }
}

/// The word a lifecycle state goes by on the board.
fn state_word(state: WatchState) -> &'static str {
    match state {
        WatchState::Running => "running",
        WatchState::Idle => "idle",
        WatchState::Done => "done",
        WatchState::Failed => "failed",
        WatchState::Cancelled => "cancelled",
    }
}

/// What a DM's badge is measured in (D99): one entry per Said post of the pair
/// lane, `true` where the agent is the one who spoke.
///
/// Kept beside the length of the history it was derived from, because deriving
/// it means walking the whole record and the poll runs every fifteen frames. A
/// history that has not changed length has not changed: it is replaced
/// wholesale at the end of a run and never edited in place, and a compaction —
/// the one rewrite — makes it *shorter*, which the length sees.
#[derive(Debug, Clone, Default)]
struct SaidCache {
    history: usize,
    authors: Vec<bool>,
}

/// Whether a room post says a name at somebody (D99).
///
/// Case-insensitive with word boundaries on both sides of the token: an agent
/// writing `@User`, `@USER,` or `(@user)` is addressing the person and reaches
/// them, while `@username` and `mail@user.example` are not and do not. The
/// literal-`@user` test this replaced meant a badge that depended on the model
/// getting the case right.
fn names(text: &str, name: &str) -> bool {
    let haystack = text.to_lowercase();
    let needle = format!("@{}", name.to_lowercase());
    let part_of_a_word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let mut from = 0;
    while let Some(offset) = haystack.get(from..).and_then(|rest| rest.find(&needle)) {
        let start = from + offset;
        let end = start + needle.len();
        let before_ok = haystack[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !part_of_a_word(c));
        let after_ok = haystack[end..]
            .chars()
            .next()
            .is_none_or(|c| !part_of_a_word(c));
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// The registry: main plus whatever the domain currently has.
#[derive(Debug, Clone)]
pub struct Buffers {
    /// Home first, always; the rest in [`BufferId`] order.
    list: Vec<Buffer>,
    active: BufferId,
    team: Vec<TeamEvent>,
    /// Per instance, the pair lane's measure — see [`SaidCache`].
    said: std::collections::HashMap<String, SaidCache>,
}

impl Default for Buffers {
    fn default() -> Self {
        Self::new()
    }
}

impl Buffers {
    pub fn new() -> Self {
        Self {
            list: vec![Buffer {
                id: BufferId::Hub,
                seq: 0,
                read: 0,
                mention: false,
                draft: String::new(),
                last_activity: 0,
            }],
            active: BufferId::Hub,
            team: Vec::new(),
            said: std::collections::HashMap::new(),
        }
    }

    pub fn active(&self) -> &BufferId {
        &self.active
    }

    /// Point the engine at another conversation. Entering one reads it.
    pub fn set_active(&mut self, id: BufferId) {
        self.active = id.clone();
        self.mark_read(&id);
    }

    pub fn get(&self, id: &BufferId) -> Option<&Buffer> {
        self.list.iter().find(|b| b.id == *id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Buffer> {
        self.list.iter()
    }

    /// Materialize a buffer, or reach the one that is already there.
    ///
    /// A conversation seen for the first time starts read: opening bingo on a
    /// session that has been running for an hour should not greet you with a
    /// badge for every turn that already happened. The workspace made the same
    /// choice for the same reason.
    fn entry(&mut self, id: BufferId, seq: u64, tick: u64) -> &mut Buffer {
        if self.list.iter().all(|b| b.id != id) {
            // Insert in order rather than push-then-sort: the list is kept
            // sorted, so the position is a search, and main stays at 0
            // because `BufferId::Hub` sorts first.
            let at = self
                .list
                .iter()
                .position(|b| b.id > id)
                .unwrap_or(self.list.len());
            self.list.insert(
                at,
                Buffer {
                    id: id.clone(),
                    seq,
                    read: seq,
                    mention: false,
                    draft: String::new(),
                    last_activity: tick,
                },
            );
        }
        let i = self.list.iter().position(|b| b.id == id).unwrap_or(0);
        &mut self.list[i]
    }

    /// The accounting step, with the domain already read. Pure, so the unread
    /// and mention rules are testable without a session behind them.
    pub fn observe(&mut self, id: BufferId, seq: u64, mention: bool, tick: u64) {
        let active = self.active.clone();
        let buf = self.entry(id, seq, tick);
        if seq > buf.seq {
            buf.last_activity = tick;
            buf.mention |= mention;
        }
        // A source can shrink: compacting a subagent rewrites its history, and a
        // cursor left past the end would read as "nothing new" forever after.
        buf.read = buf.read.min(seq);
        buf.seq = seq;
        if buf.id == active {
            // You are looking at it. Nothing in it is unread, by definition.
            buf.read = seq;
            buf.mention = false;
        }
    }

    /// Re-read the domain and update every buffer's accounting. Materializes
    /// conversations that have appeared since the last call.
    ///
    /// This does not remove buffers whose source is gone: a stopped instance
    /// still has a conversation worth reading back, and the workspace keeps its
    /// row too. D89 decides what a dead conversation looks like.
    ///
    /// **Rooms are the exception, and membership is why** (D95). A DM survives
    /// its agent because the conversation was still yours; a room you are not
    /// in was never yours to begin with — it is somebody else's conversation,
    /// findable in the directory and readable there. So a room is listed while
    /// the user is a member of it and drops out of the registry when they
    /// leave, which is what makes the bar mean "conversations I am in".
    pub fn refresh(&mut self, session: &Arc<Session>, tick: u64) {
        let mut mine: Vec<String> = Vec::new();
        for status in session.channels.list() {
            if !status.members.iter().any(|m| m == USER_NAME) {
                continue;
            }
            mine.push(status.name.clone());
            let id = BufferId::Channel(status.name.clone());
            let read = self.get(&id).map(|b| b.read).unwrap_or(status.seq);
            // Only worth reading the log when something in it is unread — this
            // runs on main's poll, not inside a modal that was already
            // cloning logs per frame.
            let mention = status.seq > read
                && session
                    .channels
                    .log_of(&status.name)
                    .iter()
                    .any(|m| m.seq > read && names(&m.text, USER_NAME));
            self.observe(id, status.seq, mention, tick);
        }
        // A room the user has left (or that is gone) stops being one of their
        // conversations — including the one they are standing in, which simply
        // becomes a room they are observing. The flow does not move: reading
        // was never the part that needed membership.
        self.list.retain(|buffer| match buffer.id() {
            BufferId::Channel(name) => mine.iter().any(|kept| kept == name),
            _ => true,
        });
        let mut live: Vec<String> = Vec::new();
        for status in session.agents.list() {
            let id = BufferId::Dm(status.name.clone());
            let read = self.get(&id).map(|b| b.read).unwrap_or(0) as usize;
            let (seq, mention) = self.pair_measure(session, &status.name, read);
            live.push(status.name);
            self.observe(id, seq, mention, tick);
        }
        self.said
            .retain(|name, _| live.iter().any(|kept| kept == name));
    }

    /// A DM's badge, measured in the pair lane (D99): how many messages it holds
    /// and whether the agent is the one who wrote any of the unread ones.
    ///
    /// **Said, not history length.** The old measure counted every row the
    /// record grew, so a turn that made forty tool calls read as forty unread
    /// messages; the measure the observation page already states — "process rows
    /// are work, not messages" — is the one the bar wants. **And a mention
    /// means it answered you**: marking every change was badge blindness by
    /// construction, because a DM was then always accented and the accent said
    /// nothing.
    ///
    /// Memoized on the history length, so the fifteen-frame poll walks a record
    /// only when the record moved. See [`SaidCache`].
    fn pair_measure(&mut self, session: &Arc<Session>, name: &str, read: usize) -> (u64, bool) {
        let Some((history, stamps, ..)) = session.agents.view_of(name) else {
            return (0, false);
        };
        let entry = self.said.entry(name.to_string()).or_default();
        if entry.history != history.len() {
            entry.history = history.len();
            entry.authors = crate::tui::perspective::pair_lane(name, &history, &stamps)
                .into_iter()
                .filter(|item| item.post.kind == PostKind::Said)
                .map(|item| !item.post.you)
                .collect();
        }
        (
            entry.authors.len() as u64,
            entry.authors.iter().skip(read).any(|&by_agent| by_agent),
        )
    }

    /// The console's own unread (D99).
    ///
    /// @main is the one conversation with no domain store behind it — its record
    /// *is* the flow — so the host says when main has spoken rather than the
    /// registry counting a source. `mention` is reserved for the one line that
    /// arrives without main's consent: the D98 failure alert.
    pub fn note_console(&mut self, mention: bool, tick: u64) {
        let seq = self.get(&BufferId::Hub).map(|b| b.seq).unwrap_or(0) + 1;
        self.observe(BufferId::Hub, seq, mention, tick);
    }

    /// Tee of the lifecycle stream (`UiEvent::WatchEvent`).
    ///
    /// Only agent events reach the feed. Room events belong to their own
    /// conversation and command events are main's own tools, so neither is
    /// team news. Nothing here is gated any more: the feed is a column in a
    /// directory the user opens, not a buffer that asks to be read, so there is
    /// no badge to withhold and no reason to decide whether a formation counts
    /// as a formation before writing down that an agent started.
    pub fn note_watch_event(
        &mut self,
        label: &str,
        kind: WatchKind,
        state: WatchState,
        detail: Option<&str>,
        _tick: u64,
    ) {
        if kind != WatchKind::Agent {
            return;
        }
        self.push_team(TeamEvent {
            label: label.to_string(),
            state: Some(state),
            detail: detail.map(str::to_string),
            at: crate::channels::now_unix(),
        });
    }

    /// The bounded lifecycle log, oldest first — the team directory's feed, and
    /// since D94 the only place a main-idle spawn or completion is written down
    /// on the display side.
    pub fn team_log(&self) -> &[TeamEvent] {
        &self.team
    }

    /// Post the host's own output to the feed (D90).
    ///
    /// `/team` reports what the formation is, and that is team news rather than
    /// main news: without this the answer landed in main's info tier and
    /// scrolled away. It goes where the rest of the formation's history goes,
    /// and main keeps one line pointing at it.
    pub fn note_team_output(&mut self, label: &str, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        self.push_team(TeamEvent {
            label: label.to_string(),
            state: None,
            detail: Some(text.to_string()),
            at: crate::channels::now_unix(),
        });
    }

    /// Append to the bounded feed. It is the one display-side store with no
    /// domain store behind it, so it is the one that has to bound itself.
    fn push_team(&mut self, event: TeamEvent) {
        self.team.push(event);
        if self.team.len() > TEAM_LOG_MAX {
            let over = self.team.len() - TEAM_LOG_MAX;
            self.team.drain(..over);
        }
    }

    /// Mark everything in a conversation read.
    pub fn mark_read(&mut self, id: &BufferId) {
        if let Some(buf) = self.list.iter_mut().find(|b| b.id == *id) {
            buf.read = buf.seq;
            buf.mention = false;
        }
    }

    /// Leave a draft behind in a conversation.
    pub fn stash_draft(&mut self, id: &BufferId, text: String) {
        if let Some(buf) = self.list.iter_mut().find(|b| b.id == *id) {
            buf.draft = text;
        }
    }

    /// Take the draft back out. Reading it clears it: a draft that stayed put
    /// after being restored would come back a second time on the next switch.
    pub fn take_draft(&mut self, id: &BufferId) -> String {
        match self.list.iter_mut().find(|b| b.id == *id) {
            Some(buf) => std::mem::take(&mut buf.draft),
            None => String::new(),
        }
    }

    /// What to put on screen when switching to a conversation: a divider, then
    /// its last `budget` messages.
    ///
    /// `budget` counts messages, not rows. A row budget cannot be honoured
    /// here — rows exist only after a layout at a known width, which is the
    /// host's business and D89's — and a number that silently meant something
    /// else would be worse than a number that says what it is.
    ///
    /// Main yields nothing: it is already on screen, and replaying it would
    /// print the transcript a second time under a rule.
    pub fn rehydrate(&self, session: &Arc<Session>, id: &BufferId, budget: usize) -> Vec<Replay> {
        if *id == BufferId::Hub {
            return Vec::new();
        }
        let mut out = vec![Replay::Divider(self.rule_for(session, id))];
        // A DM's work is folded into the message it belongs to (D99), so its
        // replay is built by the pair walk rather than assembled from posts —
        // an activity has nowhere to live on a `Post`.
        if let BufferId::Dm(name) = id {
            // Settled history only: the live tail, the in-flight claim and the
            // queued items are states, not record, and the host draws those
            // itself once it is showing the conversation.
            let mut items = match session.agents.view_of(name) {
                Some((history, stamps, ..)) => pair_replay(name, &history, &stamps),
                None => Vec::new(),
            };
            let start = items.len().saturating_sub(budget);
            out.extend(items.drain(start..));
            return out;
        }
        let posts: Vec<Post> = match id {
            BufferId::Channel(name) => channel_posts(&session.channels.log_of(name), USER_NAME),
            BufferId::Hub | BufferId::Dm(_) => Vec::new(),
        };
        let kept: Vec<&Post> = posts
            .iter()
            .filter(|p| matches!(p.kind, PostKind::Said | PostKind::Note))
            .collect();
        let start = kept.len().saturating_sub(budget);
        out.extend(kept[start..].iter().map(|post| match post.kind {
            // Nobody said it, so nothing renders a name over it or a clock
            // beside it — the stamp, where the source has one, is part of the
            // line's own text.
            PostKind::Note => Replay::Note(post.text.clone()),
            _ => Replay::Message {
                who: if post.you {
                    USER_NAME.to_string()
                } else {
                    post.from.clone()
                },
                message: UiMessage {
                    role: if post.you {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    text: post.text.clone(),
                    at: post.at,
                    activities: Vec::new(),
                    insert_points: Vec::new(),
                    groups: Vec::new(),
                    group_of: Vec::new(),
                    digest: false,
                },
            },
        }));
        out
    }

    /// The rule a conversation opens under. A room the user is only watching
    /// says so in the one place they cannot miss and cannot mistake for
    /// somebody's message: `── #parser · observer · read-only ──`.
    pub fn rule_for(&self, session: &Arc<Session>, id: &BufferId) -> String {
        match id {
            BufferId::Channel(name) if !session.channels.is_member(name, USER_NAME) => {
                format!("── {} · observer · read-only ──", id.label())
            }
            _ => id.rule(),
        }
    }

    /// Where a composer submit in this conversation belongs.
    ///
    /// The session is here for one question — am I in this room — and it is
    /// asked at the router rather than at the caller so that every way of
    /// submitting (the composer, `#room …` from main) gets the same answer.
    pub fn route_submit(&self, session: &Arc<Session>, id: &BufferId, text: &str) -> SubmitTarget {
        let text = text.to_string();
        match id {
            BufferId::Hub => SubmitTarget::Turn(text),
            BufferId::Channel(name) if !session.channels.is_member(name, USER_NAME) => {
                SubmitTarget::Refused(OBSERVER_HINT)
            }
            BufferId::Channel(name) => SubmitTarget::Channel {
                channel: name.clone(),
                text,
            },
            BufferId::Dm(name) => SubmitTarget::Dm {
                agent: name.clone(),
                text,
            },
        }
    }
}

/// Perform a routed submit against the domain — the same two calls the
/// workspace composer makes today, in the same order, so a message sent from a
/// buffer is indistinguishable from one sent from the modal.
pub fn deliver(session: &Arc<Session>, target: SubmitTarget) -> Delivery {
    match target {
        SubmitTarget::Turn(text) => Delivery::Turn(text),
        SubmitTarget::Refused(why) => Delivery::Rejected(why.to_string()),
        SubmitTarget::Dm { agent, text } => {
            match session
                .agents
                .deliver(&agent, USER_NAME, &text, Vec::new(), None)
            {
                Ok(_) => {
                    crate::tool::agent::flush_agent_inbox(session, &session.watch);
                    Delivery::Sent
                }
                Err(e) => Delivery::Rejected(e),
            }
        }
        SubmitTarget::Channel { channel, text } => {
            match crate::tool::channel::deliver_post(
                session,
                &session.watch,
                USER_NAME,
                &channel,
                &text,
            ) {
                Ok(crate::tool::channel::PostDelivery::Sent { .. }) => Delivery::Sent,
                Ok(crate::tool::channel::PostDelivery::Stale { .. }) => Delivery::Rejected(
                    "the channel got new messages; read them and resend".to_string(),
                ),
                Err(e) => Delivery::Rejected(e),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Extraction: a domain store's messages → posts
//
// These rules moved here from the retired workspace skin (D89). They are the
// one place a stored conversation becomes displayable messages. The recognition
// of what a stored line *is* lives in `line_source`, and the attribution walk
// over it in `tui::perspective`; what stays here is the presentation each view
// wants — the pair lane and its live-turn tail (`dm_posts`), a room's log
// (`channel_posts`), and the replay elements the flow prints (`pair_replay`).
// ---------------------------------------------------------------------------

/// What a message row shows besides its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostKind {
    /// An ordinary message.
    Said,
    /// Sent but still in the inbox — delivery happens at the next turn boundary.
    Queued,
    /// The streaming tail of a running turn (Slack's "…is typing").
    Typing,
    /// Wake-up scaffolding the runtime wrote into the instance's history — a
    /// relayed channel message, a follow-up chase, the task reminder. Nobody
    /// typed it, so it gets one dim line instead of a quoted block with a name
    /// and an avatar over it.
    Note,
    /// A step of the agent's work — a tool call or a reasoning phase — shown
    /// the way the main transcript shows it: one dim line under the agent's
    /// name, kept after the turn lands (the history's ToolUse/Thinking blocks
    /// re-render the same rows).
    Process,
}

/// One rendered message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    pub from: String,
    /// Written by the human sitting in front of the terminal.
    pub you: bool,
    /// Unix seconds; 0 when the source carries no clock.
    pub at: u64,
    pub text: String,
    pub kind: PostKind,
}

/// Room log → posts. Speech becomes a message; a roster change becomes one dim
/// line that carries its own clock, because the row it renders as has nowhere
/// to hang a stamp (D93's convention, stated inside the text instead of beside
/// it).
pub fn channel_posts(log: &[ChannelMessage], me: &str) -> Vec<Post> {
    log.iter()
        .map(|m| match m.kind {
            crate::channels::MessageKind::Said => Post {
                from: m.from.clone(),
                you: m.from == me,
                at: m.at,
                text: m.text.clone(),
                kind: PostKind::Said,
            },
            crate::channels::MessageKind::Membership => {
                let when = match stamp(m.at) {
                    at if at.is_empty() => String::new(),
                    at => format!(" {at}"),
                };
                Post {
                    from: m.from.clone(),
                    you: false,
                    at: m.at,
                    text: format!("· {} {} ·{when}", m.from, m.text),
                    kind: PostKind::Note,
                }
            }
        })
        .collect()
}

/// What one line of a stored user-role message *is*, by the shape the runtime
/// composed it in (`absorb_inbox`, the task reminder, the steer path).
///
/// **One parser, one walk.** The shapes are recognised here and nowhere else,
/// and [`crate::tui::perspective::walk`] is the single reader that turns them
/// into attributed posts. The observation page keeps every lane the walk files;
/// the user's `@X` pair view ([`dm_posts`]) keeps the one lane it is in and
/// drops the rest, which is why a room relay or a chase no longer renders in a
/// DM as a dim note (D99) — it is not the user's conversation to read there.
///
/// Anything the runtime wraps in its own brackets is not a message somebody
/// typed; `None` is prose, which is main's default voice (main is the one
/// sender `direct_text` leaves unmarked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineSource {
    /// The human, under the D64 marker.
    User,
    /// A room message relayed into the agent's context. `body` is the relay's
    /// own `from: text`, the one scaffolding shape that kept a sender's name.
    Room { channel: String, body: String },
    /// Main, labelled because a batch made the boundaries ambiguous. Unlike
    /// every other bracketed shape this one carries a real instruction.
    MainBatched { text: String },
    /// An agent speaking directly to the main agent (D98's `SendMessage`).
    /// Like [`LineSource::User`] it is a header line and the message is what
    /// follows it — the sender is named because `main` hears from many.
    Agent { name: String },
    /// An automatic chase for a main message nobody answered. Carries no
    /// instruction — only the fact that somebody is still waiting.
    Chase,
    /// The task reminder. A *block*, not a line: everything after it in the
    /// same message belongs to it.
    TaskReminder,
}

/// The scaffolding shapes, recognised once. See [`LineSource`].
pub fn line_source(line: &str) -> Option<LineSource> {
    let line = line.trim_end();
    if line.starts_with(crate::query::TASK_REMINDER_MARKER) {
        return Some(LineSource::TaskReminder);
    }
    if line == crate::tool::agent::DM_FROM_USER_MARKER {
        return Some(LineSource::User);
    }
    if let Some(rest) = line.strip_prefix(crate::channels::MAIN_MESSAGE_PREFIX)
        && let Some(name) = rest.strip_suffix(']')
        && !name.is_empty()
    {
        return Some(LineSource::Agent {
            name: name.to_string(),
        });
    }
    if let Some(rest) = line.strip_prefix("[#")
        && let Some((head, body)) = rest.split_once("] ")
        && let Some((channel, _)) = head.split_once(' ')
    {
        return Some(LineSource::Room {
            channel: channel.to_string(),
            body: body.to_string(),
        });
    }
    if let Some(rest) = line.strip_prefix("[follow-up ") {
        // `[follow-up instruction] …` and `[follow-up 2/3] …` share a prefix and
        // mean opposite things: the first is main talking, the second is the
        // runtime reporting silence.
        return Some(match rest.strip_prefix("instruction]") {
            Some(text) => LineSource::MainBatched {
                text: text.trim_start().to_string(),
            },
            None => LineSource::Chase,
        });
    }
    None
}

/// The collapsed reasoning row, exactly the transcript's header: the phase is
/// shown, the stream is not.
pub(crate) const THINKING_ROW: &str = "✻ Thinking";

/// A stored tool-use block → the transcript's call line (`⏺ Bash(git status)`),
/// the same brick `on_tool_ready` builds the live tail from.
pub(crate) fn tool_call_line(name: &str, input: &serde_json::Value) -> String {
    let glyph = crate::tui::activities::tool_glyph(name);
    let shown = crate::tui::activities::display_tool_name(name);
    let summary = crate::query::summarize_input(name, input);
    if summary.is_empty() {
        format!("{glyph}{shown}")
    } else {
        format!("{glyph}{shown}({summary})")
    }
}

/// Subagent history + live turn → the **pair lane**: the user's conversation
/// with this agent and nothing else (D99).
///
/// What renders is what the user said (the D64 marker, the steer path), what
/// the agent answered *them*, and the work the agent did for those turns — the
/// protagonist rule, borrowed whole from the observation page. Main's
/// instructions, room relays, `[message from @X]` mail, chases, the task
/// reminder and the prompt the instance was created with all belong to a lane
/// that is not this one, and the page ([`crate::tui::perspective`]) is where
/// they are read.
///
/// The lane comes from [`crate::tui::perspective::pair_lane`] rather than from a
/// splitter of its own: [`line_source`] is the one parser, and the attribution
/// walk above it is the one walk.
///
/// `in_flight` is the messages already claimed by the running turn but not yet
/// landed in history, and `pending` the ones still in the inbox; both are the
/// user's own — the caller filters by sender — so a message never vanishes
/// between the send and the turn's end.
pub fn dm_posts(
    history: &[Message],
    stamps: &[u64],
    in_flight: &[String],
    live: &[crate::agents::LiveBlock],
    pending: &[String],
    who: &str,
) -> Vec<Post> {
    let process = |text: String| Post {
        from: who.to_string(),
        you: false,
        at: 0,
        text,
        kind: PostKind::Process,
    };
    let mut out: Vec<Post> = crate::tui::perspective::pair_lane(who, history, stamps)
        .into_iter()
        .map(|item| item.post)
        .collect();
    // Claimed by the running turn, not yet in the record: an ordinary message
    // (it is one — the run's prompt carries it), just without a landing clock.
    for text in in_flight {
        out.push(Post {
            from: USER_NAME.to_string(),
            you: true,
            at: 0,
            text: text.clone(),
            kind: PostKind::Said,
        });
    }
    for text in pending {
        out.push(Post {
            from: USER_NAME.to_string(),
            you: true,
            at: 0,
            text: text.clone(),
            kind: PostKind::Queued,
        });
    }
    let typing_at = match live.last() {
        Some(crate::agents::LiveBlock::Text(t)) if !t.trim().is_empty() => Some(live.len() - 1),
        _ => None,
    };
    for (i, block) in live.iter().enumerate() {
        match block {
            crate::agents::LiveBlock::Text(text) if !text.trim().is_empty() => out.push(Post {
                from: who.to_string(),
                you: false,
                at: 0,
                text: text.clone(),
                kind: if Some(i) == typing_at {
                    PostKind::Typing
                } else {
                    PostKind::Said
                },
            }),
            crate::agents::LiveBlock::Tool(text) => out.push(process(text.clone())),
            crate::agents::LiveBlock::Thinking(_) => out.push(process(THINKING_ROW.to_string())),
            crate::agents::LiveBlock::Text(_) => {}
        }
    }
    // The indicator spans the whole stretch the agent owes a reply: from the
    // instant a message is on its way (queued or claimed, before the stream
    // says anything) through tool waits and round gaps. Without the early leg
    // the DM sits silent for exactly the send-to-first-delta latency.
    if typing_at.is_none() && !(live.is_empty() && in_flight.is_empty() && pending.is_empty()) {
        out.push(Post {
            from: who.to_string(),
            you: false,
            at: 0,
            text: String::new(),
            kind: PostKind::Typing,
        });
    }
    out
}

/// An empty message of one role — the shape every replayed element starts from.
fn blank_message(role: Role) -> UiMessage {
    UiMessage {
        role,
        text: String::new(),
        at: 0,
        activities: Vec::new(),
        insert_points: Vec::new(),
        groups: Vec::new(),
        group_of: Vec::new(),
        digest: false,
    }
}

/// A replayed tool name as a `&'static str`, interned.
///
/// The activity model holds tool names by static reference, and the live path
/// leaks the streamed name once per call. A replay re-reads the same names on
/// every switch, so it interns instead: one leak per distinct name for the life
/// of the process, rather than one per switch.
fn interned_tool(name: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut seen = NAMES
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(found) = seen.get(name) {
        return found;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    seen.insert(leaked);
    leaked
}

/// Hang one step of an agent's work on the message it belongs to, exactly the
/// way the console's tool events hang it on main's (D99).
///
/// The grouping rules are not restated here — [`crate::tui::chat::classify_tool`]
/// decides what collapses and [`crate::tui::chat::collapse_summary`] words it,
/// so `4 searches, 2 reads` reads the same in an agent's DM as in @main. What a
/// replay cannot supply is the *output*: a stored history keeps the call, and
/// the tool's result went to the model, not to the record. So expanding a
/// replayed group shows the calls it folded and no output rows — the record
/// (ctrl+o, the observation page) is where the rest is.
fn push_work(message: &mut UiMessage, work: &crate::tui::perspective::Work) {
    use crate::tui::activities::{Activity, ActivityKind, Thinking, ThinkingState, ToolCall};
    use crate::tui::chat::{CollapseGroup, CollapseKind, classify_tool};

    let idx = message.activities.len();
    let at = message.text.chars().count();
    match work {
        crate::tui::perspective::Work::Thinking => {
            message
                .activities
                .push(Activity::new(ActivityKind::Thinking(Thinking {
                    state: ThinkingState::Done,
                    duration_ms: 0,
                    stage: "",
                    done_verb: None,
                    start_tick: 0,
                    segments: 1,
                })));
            message.insert_points.push(at);
            message.group_of.push(None);
        }
        crate::tui::perspective::Work::Tool { name, input } => {
            message
                .activities
                .push(Activity::new(ActivityKind::Tool(ToolCall {
                    name: interned_tool(name),
                    // Settled: the call is in the record, so it ran to an end.
                    status: crate::tui::activities::ToolStatus::Done,
                    summary: crate::query::summarize_input(name, input),
                    duration_ms: 0,
                    output: None,
                    result_summary: None,
                })));
            message.insert_points.push(at);
            message.group_of.push(None);
            let Some(kind) = classify_tool(name, input) else {
                // A call that does not collapse ends whatever run was open —
                // the console's rule, so a `Write` between two reads breaks the
                // group here too.
                if let Some(group) = message.groups.last_mut() {
                    group.active = false;
                }
                return;
            };
            let open = message
                .groups
                .last()
                .is_some_and(|g| g.active && !g.activities.is_empty());
            if !open {
                message.groups.push(CollapseGroup {
                    active: true,
                    ..CollapseGroup::default()
                });
            }
            let g = message.groups.len().saturating_sub(1);
            message.group_of[idx] = Some(g);
            message.groups[g].activities.push(idx);
            match kind {
                CollapseKind::Search => message.groups[g].search += 1,
                CollapseKind::Read(Some(path)) => message.groups[g].read_paths.push(path),
                CollapseKind::Read(None) => message.groups[g].read_ops += 1,
                CollapseKind::List => message.groups[g].list += 1,
                CollapseKind::Bash => message.groups[g].bash += 1,
                CollapseKind::AgentCheck => message.groups[g].agent_checks += 1,
                CollapseKind::AgentStop => message.groups[g].agent_stops += 1,
                CollapseKind::AgentDelete => message.groups[g].agent_deletes += 1,
            }
        }
    }
}

/// The pair lane as replay elements (D99).
///
/// The user's messages come back one bubble each. The agent's *run* — its prose
/// and the work it did between sentences — comes back as **one message with
/// activities**, which is the unit the console holds a turn in, so the same
/// renderer folds it into `⏺ Searched for 4 patterns, read 2 files`. That is
/// the whole reason this is not a list of dim `⏺ Tool(…)` lines any more.
///
/// A run ends where anything at all stood between two of the agent's rows in
/// the full record — a message from main, a room relay, a chase — because that
/// something is what ended it, even though this lane never shows it. The rule
/// also keeps the replay **append-only**: every continuation is triggered by an
/// item that breaks the run, so a message already printed never grows.
fn pair_replay(who: &str, history: &[Message], stamps: &[u64]) -> Vec<Replay> {
    let mut out: Vec<Replay> = Vec::new();
    let mut open: Option<UiMessage> = None;
    fn flush(open: &mut Option<UiMessage>, out: &mut Vec<Replay>, who: &str) {
        let Some(mut message) = open.take() else {
            return;
        };
        // The turn is over, so nothing in it is still running: the console
        // closes the last group at `TurnEnd` and a replay is all past tense.
        if let Some(group) = message.groups.last_mut() {
            group.active = false;
        }
        out.push(Replay::Message {
            who: who.to_string(),
            message,
        });
    }
    for item in crate::tui::perspective::pair_lane(who, history, stamps) {
        if item.post.you {
            flush(&mut open, &mut out, who);
            let mut message = blank_message(Role::User);
            message.text = item.post.text;
            message.at = item.post.at;
            out.push(Replay::Message {
                who: USER_NAME.to_string(),
                message,
            });
            continue;
        }
        if !item.contiguous {
            flush(&mut open, &mut out, who);
        }
        let message = open.get_or_insert_with(|| blank_message(Role::Assistant));
        match &item.work {
            Some(work) => push_work(message, work),
            None => {
                if !message.text.is_empty() {
                    message.text.push_str("\n\n");
                }
                message.text.push_str(&item.post.text);
                // The reply's clock is when its last piece landed, which is the
                // rule the console stamps a turn with.
                if item.post.at != 0 {
                    message.at = item.post.at;
                }
            }
        }
    }
    flush(&mut open, &mut out, who);
    out
}

/// Send-time stamp trailing a message body (issue #41), the same in every view:
/// local `HH:MM` today, `M/D HH:MM` on any other day. Empty when the source
/// carries no timestamp — a missing clock renders as nothing rather than being
/// invented (the [`ChannelMessage`] rule).
/// Now, in the unix seconds every `at` in this module is measured in. One
/// clock reading, so a caller that needs to stamp something does not have to
/// know how the ones already stored were made.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn stamp(at: u64) -> String {
    use chrono::TimeZone;
    if at == 0 {
        return String::new();
    }
    chrono::Local
        .timestamp_opt(at as i64, 0)
        .single()
        .map(|t| stamp_of(&t, chrono::Local::now().date_naive()))
        .unwrap_or_default()
}

/// [`stamp`] with "today" injected, so tests pin both sides of the day boundary.
fn stamp_of(t: &chrono::DateTime<chrono::Local>, today: chrono::NaiveDate) -> String {
    if t.date_naive() == today {
        t.format("%H:%M").to_string()
    } else {
        t.format("%-m/%-d %H:%M").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;
    use crate::api::types::{ContentBlock, Message};
    use crate::channels::ChannelMode;
    use crate::tui::test_util::test_session;

    fn assistant(text: &str) -> Message {
        Message {
            role: crate::api::types::Role::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    /// A message the *user* sent, in the shape `absorb_inbox` records it: the
    /// D64 marker heading the text. Unmarked prose is the main agent talking,
    /// which is exactly what the pair view has to tell apart (D99).
    fn from_user(text: &str) -> Message {
        Message::user_text(format!(
            "{}\n{text}",
            crate::tool::agent::DM_FROM_USER_MARKER
        ))
    }

    fn tool_use(name: &str, input: serde_json::Value) -> ContentBlock {
        ContentBlock::ToolUse {
            id: "toolu_1".to_string(),
            name: name.to_string(),
            input,
        }
    }

    /// An instance with `history` already behind it, the way a subagent that has
    /// answered a few times looks to the registry.
    fn seed_agent(session: &Arc<Session>, name: &str, history: Vec<Message>) {
        session.agents.insert(
            name,
            AgentKind::Hire,
            None,
            "test instance".to_string(),
            session.clone(),
        );
        if !history.is_empty() {
            session.agents.finish(name, history, 0);
        }
    }

    /// A crew member (D53): a teammate from the blueprint rather than a hire.
    fn seed_crew(session: &Arc<Session>, name: &str) {
        session.agents.insert(
            name,
            AgentKind::Crew,
            None,
            "crew member".to_string(),
            session.clone(),
        );
    }

    /// A room the user is in — the ordinary case for anything the bar shows.
    fn seed_room(session: &Arc<Session>, name: &str, members: &[&str]) {
        let mut roster = vec![USER_NAME.to_string()];
        roster.extend(members.iter().map(|m| m.to_string()));
        session
            .channels
            .create(name, roster, ChannelMode::Free)
            .expect("room created");
    }

    fn ids(buffers: &Buffers) -> Vec<String> {
        buffers.iter().map(|b| b.id().label()).collect()
    }

    // ---- registry -------------------------------------------------------

    #[test]
    fn main_is_there_before_anything_else_is() {
        let buffers = Buffers::new();
        assert_eq!(ids(&buffers), vec!["@main"]);
        assert_eq!(*buffers.active(), BufferId::Hub);
        assert_eq!(buffers.get(&BufferId::Hub).map(Buffer::unread), Some(0));
    }

    /// One vocabulary for naming a conversation: the label the id goes by, the
    /// rule the flow opens it with, and `Display`. D88 stated this as a `Source`
    /// enum nothing consulted; D89 made [`BufferId::rule`] load-bearing (the
    /// replay and the hand-back to main both read it), so the property is
    /// checked where it is now used.
    #[test]
    fn an_id_names_its_conversation_in_one_vocabulary() {
        assert_eq!(BufferId::Hub.label(), "@main");
        assert_eq!(BufferId::Channel("build".to_string()).label(), "#build");
        assert_eq!(BufferId::Dm("scout".to_string()).label(), "@scout");
        assert_eq!(BufferId::Dm("scout".to_string()).to_string(), "@scout");
        assert_eq!(BufferId::Hub.to_string(), "@main");
        // D101: the home conversation is spelled the way every participant is,
        // and the rule the flow hands back with says so literally. Written out
        // rather than derived, because the whole point of the rename is the
        // exact glyphs a reader sees.
        assert_eq!(BufferId::Hub.rule(), "── @main ──");
        // The rule is the label and nothing else, so a divider can never name a
        // conversation differently from the way it is addressed.
        for id in [
            BufferId::Hub,
            BufferId::Channel("build".to_string()),
            BufferId::Dm("scout".to_string()),
        ] {
            assert_eq!(id.rule(), format!("── {} ──", id.label()));
            assert!(!id.label().contains("hub"), "{id:?} still says hub");
        }
        assert_eq!(Buffers::new().iter().count(), 1, "main is always there");
    }

    #[test]
    fn conversations_materialize_from_the_domain_in_one_order() {
        let session = test_session();
        seed_room(&session, "build", &["scout"]);
        seed_room(&session, "alpha", &["scout"]);
        seed_agent(&session, "zoe", Vec::new());
        seed_crew(&session, "scout");

        let mut buffers = Buffers::new();
        buffers.refresh(&session, 1);
        buffers.note_watch_event(
            "scout #1 · go",
            WatchKind::Agent,
            WatchState::Running,
            None,
            1,
        );

        // Home, rooms by name, DMs by name — and main stays at 0 however many
        // conversations arrive after it. The team is not among them: it is a
        // directory, not a conversation (D95).
        assert_eq!(
            ids(&buffers),
            vec!["@main", "#alpha", "#build", "@scout", "@zoe"]
        );
    }

    /// The bar is "conversations I am in". A room formed by two agents is real,
    /// findable in the directory and readable — but it is not one of the user's
    /// conversations, so it is not listed, and joining is what lists it.
    #[test]
    fn a_room_is_listed_exactly_while_the_user_is_in_it() {
        let session = test_session();
        session
            .channels
            .create(
                "parser",
                vec!["scout".to_string(), "zoe".to_string()],
                ChannelMode::Free,
            )
            .expect("room created");
        let mut buffers = Buffers::new();
        buffers.refresh(&session, 1);
        assert_eq!(
            ids(&buffers),
            vec!["@main"],
            "somebody else's room is theirs"
        );

        session
            .channels
            .invite("parser", USER_NAME)
            .expect("joined");
        buffers.refresh(&session, 2);
        assert_eq!(ids(&buffers), vec!["@main", "#parser"], "joining lists it");

        session.channels.kick("parser", USER_NAME).expect("left");
        buffers.refresh(&session, 3);
        assert_eq!(
            ids(&buffers),
            vec!["@main"],
            "and leaving takes it off again — unlike a DM, a room you are not \
             in was never your conversation"
        );
    }

    #[test]
    fn the_same_id_always_names_the_same_buffer() {
        let session = test_session();
        seed_agent(&session, "scout", Vec::new());
        let mut buffers = Buffers::new();
        buffers.refresh(&session, 1);
        let before = buffers.iter().count();
        buffers.stash_draft(
            &BufferId::Dm("scout".to_string()),
            "half a thought".to_string(),
        );
        // Three more polls must not clone the conversation or lose what is in it.
        buffers.refresh(&session, 2);
        buffers.refresh(&session, 3);
        buffers.refresh(&session, 4);
        assert_eq!(buffers.iter().count(), before, "refresh is idempotent");
        assert_eq!(
            buffers
                .get(&BufferId::Dm("scout".to_string()))
                .map(|buffer| buffer.draft.as_str()),
            Some("half a thought")
        );
    }

    // ---- unread ---------------------------------------------------------

    #[test]
    fn a_conversation_seen_for_the_first_time_starts_read() {
        let session = test_session();
        seed_agent(&session, "scout", vec![assistant("done long ago")]);
        let mut buffers = Buffers::new();
        buffers.refresh(&session, 1);
        assert_eq!(
            buffers
                .get(&BufferId::Dm("scout".to_string()))
                .map(Buffer::unread),
            Some(0),
            "history that predates the buffer is not news"
        );
    }

    /// Rewritten for D99: the measure is the pair lane's messages, so the
    /// history the badge counts has to be a conversation rather than a stack of
    /// bare replies — an agent's report on its spawn task is main's, not yours.
    #[test]
    fn unread_counts_one_per_message_and_moves_the_stamp() {
        let session = test_session();
        seed_agent(&session, "scout", Vec::new());
        let mut buffers = Buffers::new();
        buffers.refresh(&session, 1);
        let id = BufferId::Dm("scout".to_string());
        assert_eq!(buffers.get(&id).map(|buffer| buffer.last_activity), Some(1));

        let mut history = vec![from_user("look at the parser"), assistant("one")];
        session.agents.finish("scout", history.clone(), 0);
        buffers.refresh(&session, 7);
        assert_eq!(buffers.get(&id).map(Buffer::unread), Some(2));
        assert_eq!(buffers.get(&id).map(|buffer| buffer.last_activity), Some(7));

        history.push(assistant("two"));
        session.agents.finish("scout", history, 0);
        buffers.refresh(&session, 9);
        assert_eq!(buffers.get(&id).map(Buffer::unread), Some(3));
        assert_eq!(buffers.get(&id).map(|buffer| buffer.last_activity), Some(9));

        // A poll that finds nothing new leaves the stamp where it was.
        buffers.refresh(&session, 30);
        assert_eq!(buffers.get(&id).map(|buffer| buffer.last_activity), Some(9));
    }

    /// The badge counts what was *said* (D99). It used to count the history's
    /// length, so a turn that made forty tool calls read as forty unread
    /// messages — a number about the agent's work rather than about anything
    /// addressed to the reader.
    #[test]
    fn a_dm_counts_messages_and_not_the_work_between_them() {
        let session = test_session();
        seed_agent(&session, "scout", Vec::new());
        let mut buffers = Buffers::new();
        buffers.refresh(&session, 1);
        let id = BufferId::Dm("scout".to_string());

        let history = vec![
            from_user("find the leak"),
            Message {
                role: crate::api::types::Role::Assistant,
                content: vec![
                    ContentBlock::Text {
                        text: "looking".to_string(),
                    },
                    tool_use("Grep", serde_json::json!({"pattern": "leak"})),
                    tool_use("Read", serde_json::json!({"file_path": "a.rs"})),
                    tool_use("Read", serde_json::json!({"file_path": "b.rs"})),
                ],
            },
            assistant("found it"),
        ];
        session.agents.finish("scout", history, 0);
        buffers.refresh(&session, 2);
        assert_eq!(
            buffers.get(&id).map(Buffer::unread),
            Some(3),
            "one from you, two from it — the three tool calls are work"
        );
    }

    /// A mention means the agent answered *you*. Every DM change used to raise
    /// one, which is badge blindness by construction: the accent was always on,
    /// so it said nothing (D99).
    #[test]
    fn a_dm_wants_you_when_the_agent_answers_and_not_when_you_speak() {
        let session = test_session();
        seed_agent(&session, "scout", Vec::new());
        let mut buffers = Buffers::new();
        buffers.refresh(&session, 1);
        let id = BufferId::Dm("scout".to_string());

        // Your own message landing in the record is not somebody wanting you.
        session
            .agents
            .finish("scout", vec![from_user("look at the parser")], 0);
        buffers.refresh(&session, 2);
        assert_eq!(buffers.get(&id).map(Buffer::unread), Some(1));
        assert_eq!(buffers.get(&id).map(Buffer::mention), Some(false));

        session.agents.finish(
            "scout",
            vec![from_user("look at the parser"), assistant("found it")],
            0,
        );
        buffers.refresh(&session, 3);
        assert_eq!(buffers.get(&id).map(Buffer::mention), Some(true));

        // And main's traffic through the same instance is not a DM at all.
        buffers.mark_read(&id);
        session.agents.finish(
            "scout",
            vec![
                from_user("look at the parser"),
                assistant("found it"),
                Message::user_text("also check the lexer"),
                assistant("checked"),
            ],
            0,
        );
        buffers.refresh(&session, 4);
        assert_eq!(
            buffers.get(&id).map(Buffer::unread),
            Some(0),
            "an exchange with main adds nothing to the user's own lane"
        );
        assert_eq!(buffers.get(&id).map(Buffer::mention), Some(false));
    }

    #[test]
    fn a_channel_wants_you_only_when_it_says_your_name() {
        let session = test_session();
        session
            .channels
            .create(
                "build",
                vec!["scout".to_string(), USER_NAME.to_string()],
                ChannelMode::Free,
            )
            .expect("channel created");
        let mut buffers = Buffers::new();
        buffers.refresh(&session, 1);
        let id = BufferId::Channel("build".to_string());

        session
            .channels
            .post("scout", "build", "landed the refactor")
            .expect("posted");
        buffers.refresh(&session, 2);
        assert_eq!(buffers.get(&id).map(Buffer::unread), Some(1));
        assert_eq!(
            buffers.get(&id).map(Buffer::mention),
            Some(false),
            "chatter in a room is not a summons"
        );

        session
            .channels
            .post("scout", "build", "@user can you look at this")
            .expect("posted");
        buffers.refresh(&session, 3);
        assert_eq!(buffers.get(&id).map(Buffer::unread), Some(2));
        assert_eq!(buffers.get(&id).map(Buffer::mention), Some(true));
    }

    #[test]
    fn the_conversation_you_are_in_is_never_unread() {
        let mut buffers = Buffers::new();
        let scout = BufferId::Dm("scout".to_string());
        buffers.observe(scout.clone(), 0, true, 1);
        buffers.set_active(scout.clone());
        buffers.observe(scout.clone(), 5, true, 2);
        assert_eq!(buffers.get(&scout).map(Buffer::unread), Some(0));
        assert_eq!(buffers.get(&scout).map(Buffer::mention), Some(false));

        // And leaving it lets the next message count again.
        buffers.set_active(BufferId::Hub);
        buffers.observe(scout.clone(), 6, true, 3);
        assert_eq!(buffers.get(&scout).map(Buffer::unread), Some(1));
    }

    #[test]
    fn main_never_counts_its_own_flow() {
        let session = test_session();
        seed_agent(&session, "scout", Vec::new());
        let mut buffers = Buffers::new();
        for tick in 1..5 {
            buffers.refresh(&session, tick);
        }
        assert_eq!(buffers.get(&BufferId::Hub).map(Buffer::unread), Some(0));
        assert_eq!(
            buffers.get(&BufferId::Hub).map(|buffer| buffer.seq),
            Some(0)
        );
    }

    #[test]
    fn a_shrunken_source_does_not_strand_the_read_cursor() {
        let mut buffers = Buffers::new();
        let scout = BufferId::Dm("scout".to_string());
        buffers.observe(scout.clone(), 10, true, 1);
        buffers.mark_read(&scout);
        // Compaction rewrote the instance's history shorter than it was.
        buffers.observe(scout.clone(), 3, true, 2);
        assert_eq!(buffers.get(&scout).map(Buffer::unread), Some(0));
        // The next real message still registers, instead of hiding under a
        // cursor parked past the end.
        buffers.observe(scout.clone(), 4, true, 3);
        assert_eq!(buffers.get(&scout).map(Buffer::unread), Some(1));
    }

    // ---- the team feed ---------------------------------------------------

    /// The feed hears agents and nothing else, and it hears them whoever they
    /// are: the D93 crew gate is gone with the board it protected. A gate
    /// existed because a badge asked to be read; a column in a directory the
    /// user opens asks for nothing, so there is nothing to withhold.
    #[test]
    fn the_feed_hears_agents_and_nothing_else_and_raises_no_conversation() {
        let session = test_session();
        seed_agent(&session, "scout", Vec::new());
        let mut buffers = Buffers::new();
        buffers.refresh(&session, 1);
        buffers.note_watch_event("ls", WatchKind::Command, WatchState::Done, None, 1);
        buffers.note_watch_event("#build", WatchKind::Channel, WatchState::Running, None, 1);
        assert_eq!(
            buffers.team_log().len(),
            0,
            "a command and a room post are not team news"
        );

        buffers.note_watch_event(
            "scout #1 · fix it",
            WatchKind::Agent,
            WatchState::Done,
            Some("done"),
            4,
        );
        assert_eq!(buffers.team_log().len(), 1);
        assert_eq!(
            ids(&buffers),
            vec!["@main", "@scout"],
            "a lifecycle event opens no conversation and asks for nothing"
        );
    }

    #[test]
    fn the_feed_bounds_what_it_remembers() {
        let mut buffers = Buffers::new();
        for i in 0..TEAM_LOG_MAX + 40 {
            buffers.note_watch_event(
                &format!("scout #{i}"),
                WatchKind::Agent,
                WatchState::Running,
                None,
                1,
            );
        }
        assert_eq!(buffers.team_log().len(), TEAM_LOG_MAX);
        assert_eq!(
            buffers.team_log()[0].label,
            format!("scout #{}", 40),
            "the oldest events fall off the front"
        );
    }

    // ---- rehydrate ------------------------------------------------------

    fn texts(replay: &[Replay]) -> Vec<String> {
        replay
            .iter()
            .map(|r| match r {
                Replay::Divider(text) | Replay::Note(text) => text.clone(),
                Replay::Message { message, .. } => message.text.clone(),
            })
            .collect()
    }

    #[test]
    fn main_replays_nothing_because_it_is_already_on_screen() {
        let session = test_session();
        let buffers = Buffers::new();
        assert!(
            buffers.rehydrate(&session, &BufferId::Hub, 50).is_empty(),
            "replaying main would print the transcript twice"
        );
    }

    #[test]
    fn a_dm_replays_under_a_divider() {
        let session = test_session();
        seed_agent(
            &session,
            "scout",
            vec![
                Message::user_text(format!(
                    "{}\nlook at the parser",
                    crate::tool::agent::DM_FROM_USER_MARKER
                )),
                assistant("found it"),
            ],
        );
        let buffers = Buffers::new();
        let replay = buffers.rehydrate(&session, &BufferId::Dm("scout".to_string()), 50);

        assert_eq!(
            texts(&replay),
            vec!["── @scout ──", "look at the parser", "found it"],
            "the marker line is scaffolding and does not render (D64)"
        );
        match &replay[1] {
            Replay::Message { who, message } => {
                assert_eq!(who, USER_NAME);
                assert_eq!(message.role, Role::User);
            }
            other => panic!("expected the user's message, got {other:?}"),
        }
        match &replay[2] {
            Replay::Message { who, message } => {
                assert_eq!(who, "scout");
                assert_eq!(message.role, Role::Assistant);
            }
            other => panic!("expected the agent's message, got {other:?}"),
        }
    }

    #[test]
    fn a_replay_says_what_the_workspace_says_about_the_same_history() {
        let session = test_session();
        let history = vec![
            Message::user_text(format!(
                "{}\nfirst\n{}\nsecond",
                crate::tool::agent::DM_FROM_USER_MARKER,
                crate::tool::agent::DM_FROM_USER_MARKER
            )),
            assistant("both noted"),
        ];
        seed_agent(&session, "scout", history.clone());
        let (stored, stamps, ..) = session.agents.view_of("scout").expect("the instance");
        let posts = dm_posts(&stored, &stamps, &[], &[], &[], "scout");

        let buffers = Buffers::new();
        let replay = buffers.rehydrate(&session, &BufferId::Dm("scout".to_string()), 50);

        // Same extraction, same result: the engine reads through the workspace's
        // own post builder rather than parsing history a second way.
        let replayed: Vec<String> = texts(&replay).into_iter().skip(1).collect();
        let rendered: Vec<String> = posts.iter().map(|p| p.text.clone()).collect();
        assert_eq!(replayed, rendered);
        assert_eq!(rendered, vec!["first", "second", "both noted"]);
    }

    #[test]
    fn a_room_replays_its_log() {
        let session = test_session();
        seed_room(&session, "build", &["scout"]);
        session
            .channels
            .post("scout", "build", "one")
            .expect("posted");
        session
            .channels
            .post("scout", "build", "two")
            .expect("posted");
        let buffers = Buffers::new();
        let replay = buffers.rehydrate(&session, &BufferId::Channel("build".to_string()), 50);
        assert_eq!(texts(&replay), vec!["── #build ──", "one", "two"]);
    }

    /// A roster change is part of the room's record and reads as one dim line
    /// nobody said — never as a message with a name over it, which is what it
    /// would have been if it had come back as a [`Replay::Message`].
    #[test]
    fn a_room_replays_its_membership_changes_as_notes() {
        let session = test_session();
        seed_room(&session, "build", &["scout"]);
        session
            .channels
            .post("scout", "build", "starting")
            .expect("posted");
        session.channels.invite("build", "coder").expect("joined");
        session.channels.kick("build", "scout").expect("left");

        let buffers = Buffers::new();
        let replay = buffers.rehydrate(&session, &BufferId::Channel("build".to_string()), 50);
        assert!(matches!(replay[1], Replay::Message { .. }), "{replay:?}");
        match (&replay[2], &replay[3]) {
            (Replay::Note(joined), Replay::Note(left)) => {
                assert!(joined.starts_with("· coder joined ·"), "{joined}");
                assert!(left.starts_with("· scout left ·"), "{left}");
            }
            other => panic!("membership changes are notes, got {other:?}"),
        }
    }

    /// The observer framing is a fact about the conversation, so it is decided
    /// where the rule is: the same room is `── #parser ──` to a member and
    /// `── #parser · observer · read-only ──` to somebody watching it.
    #[test]
    fn a_room_you_are_not_in_opens_under_an_observer_rule() {
        let session = test_session();
        session
            .channels
            .create("parser", vec!["scout".to_string()], ChannelMode::Free)
            .expect("room created");
        let buffers = Buffers::new();
        let id = BufferId::Channel("parser".to_string());
        assert_eq!(
            buffers.rule_for(&session, &id),
            "── #parser · observer · read-only ──"
        );
        session
            .channels
            .invite("parser", USER_NAME)
            .expect("joined");
        assert_eq!(buffers.rule_for(&session, &id), "── #parser ──");
    }

    /// Rewritten for D99: a run of the agent's replies is one message now, so a
    /// budget of two has to be counted over an exchange rather than over three
    /// consecutive answers that would fold into one.
    #[test]
    fn a_replay_keeps_to_its_budget() {
        let session = test_session();
        seed_agent(
            &session,
            "scout",
            vec![
                from_user("one"),
                assistant("two"),
                from_user("three"),
                assistant("four"),
            ],
        );
        let buffers = Buffers::new();
        let replay = buffers.rehydrate(&session, &BufferId::Dm("scout".to_string()), 2);
        assert_eq!(
            texts(&replay),
            vec!["── @scout ──", "three", "four"],
            "the budget keeps the tail, not the head"
        );
    }

    /// D99: an agent's work comes back as activities on its own message, so the
    /// console's collapse machinery folds it — `⏺ Searched for 1 pattern, read
    /// 2 files` — instead of the four flat dim lines the DM used to print. The
    /// grouping rules are not restated here; the point is that they are reached.
    #[test]
    fn a_dms_work_comes_back_as_activity_groups_and_never_as_flat_lines() {
        let session = test_session();
        seed_agent(
            &session,
            "scout",
            vec![
                from_user("find the leak"),
                Message {
                    role: crate::api::types::Role::Assistant,
                    content: vec![
                        ContentBlock::Text {
                            text: "looking".to_string(),
                        },
                        tool_use("Grep", serde_json::json!({"pattern": "leak"})),
                        tool_use("Read", serde_json::json!({"file_path": "a.rs"})),
                        tool_use("Read", serde_json::json!({"file_path": "b.rs"})),
                        ContentBlock::Text {
                            text: "found it".to_string(),
                        },
                    ],
                },
            ],
        );
        let buffers = Buffers::new();
        let replay = buffers.rehydrate(&session, &BufferId::Dm("scout".to_string()), 50);
        assert_eq!(
            texts(&replay),
            vec!["── @scout ──", "find the leak", "looking\n\nfound it"],
            "one message for the run, not one per block and none per tool"
        );
        let Replay::Message { message, .. } = &replay[2] else {
            panic!("the agent's turn is a message: {replay:?}")
        };
        assert_eq!(message.activities.len(), 3, "every call is an activity");
        assert_eq!(
            message.groups.len(),
            1,
            "and consecutive collapsible calls are one group"
        );
        assert!(!message.groups[0].active, "a replayed group is past tense");
        assert_eq!(
            crate::tui::chat::collapse_summary(&message.groups[0], false),
            "Searched for 1 pattern, read 2 files",
            "the console's own wording, reached rather than reimplemented"
        );
        // The work sits between the prose it happened between, which is what an
        // insert point is for.
        assert_eq!(message.insert_points, vec![7, 7, 7]);
    }

    /// A tool call the console does not collapse ends the group, here too.
    #[test]
    fn a_standalone_call_closes_the_group_the_way_the_console_closes_it() {
        let session = test_session();
        seed_agent(
            &session,
            "scout",
            vec![
                from_user("fix it"),
                Message {
                    role: crate::api::types::Role::Assistant,
                    content: vec![
                        tool_use("Read", serde_json::json!({"file_path": "a.rs"})),
                        tool_use("Write", serde_json::json!({"file_path": "a.rs"})),
                        tool_use("Read", serde_json::json!({"file_path": "b.rs"})),
                    ],
                },
            ],
        );
        let buffers = Buffers::new();
        let replay = buffers.rehydrate(&session, &BufferId::Dm("scout".to_string()), 50);
        let Replay::Message { message, .. } = &replay[2] else {
            panic!("{replay:?}")
        };
        assert_eq!(message.groups.len(), 2, "the write broke the run");
        assert_eq!(message.group_of, vec![Some(0), None, Some(1)]);
    }

    /// A name reaches the person whatever case it is written in, and a longer
    /// word that merely starts with it does not (D99). The literal-`@user` test
    /// this replaced made the badge depend on the model's typing.
    #[test]
    fn a_room_says_your_name_whatever_case_it_uses() {
        for reaching in [
            "@user can you look",
            "@User can you look",
            "@USER, look",
            "(@user) look",
            "look at this @user",
        ] {
            assert!(
                names(reaching, USER_NAME),
                "{reaching:?} is addressed to you"
            );
        }
        for not in [
            "@username can you look",
            "@user-2 can you look",
            "mail@user.example",
            "the user should look",
            "userful",
        ] {
            assert!(!names(not, USER_NAME), "{not:?} is not");
        }
    }

    /// The lifecycle log survived the board it used to back: it is the
    /// directory's feed now, read through the same accessor the directory reads
    /// (`the_board_replays_its_lifecycle_log`'s claim, moved to where the rows
    /// are actually built — `tui::directory`).
    #[test]
    fn the_feed_keeps_what_happened_and_what_was_reported() {
        let mut buffers = Buffers::new();
        buffers.note_watch_event(
            "scout #1 · fix it",
            WatchKind::Agent,
            WatchState::Running,
            None,
            1,
        );
        buffers.note_watch_event(
            "scout #1 · fix it",
            WatchKind::Agent,
            WatchState::Done,
            Some("fixed the parser"),
            2,
        );
        let lines: Vec<String> = buffers.team_log().iter().map(team_line).collect();
        assert_eq!(lines, vec!["running", "done · fixed the parser"]);
    }

    // ---- submit routing -------------------------------------------------

    #[test]
    fn every_conversation_routes_to_its_own_path() {
        let session = test_session();
        seed_room(&session, "build", &["scout"]);
        let buffers = Buffers::new();
        assert_eq!(
            buffers.route_submit(&session, &BufferId::Hub, "hello"),
            SubmitTarget::Turn("hello".to_string())
        );
        assert_eq!(
            buffers.route_submit(&session, &BufferId::Dm("scout".to_string()), "hello"),
            SubmitTarget::Dm {
                agent: "scout".to_string(),
                text: "hello".to_string()
            }
        );
        assert_eq!(
            buffers.route_submit(&session, &BufferId::Channel("build".to_string()), "hello"),
            SubmitTarget::Channel {
                channel: "build".to_string(),
                text: "hello".to_string()
            }
        );
    }

    /// Reading somebody else's room is free; speaking in it is membership. The
    /// refusal names the way in rather than merely saying no.
    #[tokio::test]
    async fn a_room_you_are_watching_refuses_to_be_spoken_in() {
        let session = test_session();
        session
            .channels
            .create("parser", vec!["scout".to_string()], ChannelMode::Free)
            .expect("room created");
        let buffers = Buffers::new();
        let id = BufferId::Channel("parser".to_string());
        let target = buffers.route_submit(&session, &id, "nice work everyone");
        match deliver(&session, target) {
            Delivery::Rejected(why) => assert_eq!(why, OBSERVER_HINT),
            other => panic!("an observed room is read-only, got {other:?}"),
        }
        assert!(
            session.channels.log_of("parser").is_empty(),
            "and nothing was posted under a name that is not on the roster"
        );

        // Joining is the whole difference, and it is one call.
        session
            .channels
            .invite("parser", USER_NAME)
            .expect("joined");
        let target = buffers.route_submit(&session, &id, "nice work everyone");
        assert_eq!(deliver(&session, target), Delivery::Sent);
    }

    #[tokio::test]
    async fn a_dm_reaches_the_agent_under_the_user_marker() {
        let session = test_session();
        seed_agent(&session, "scout", Vec::new());
        let buffers = Buffers::new();
        let target =
            buffers.route_submit(&session, &BufferId::Dm("scout".to_string()), "have a look");
        assert_eq!(deliver(&session, target), Delivery::Sent);

        // The shape the workspace composer produces: an inbox item from `user`,
        // which is what earns the D64 marker when the instance picks it up. The
        // engine must not add the marker itself — that would double it.
        let items = session.agents.take_running("scout", 0);
        let (prompt, _) = crate::tool::agent::absorb_inbox(&session.channels, "scout", &items);
        assert_eq!(
            prompt,
            format!("{}\nhave a look", crate::tool::agent::DM_FROM_USER_MARKER)
        );
    }

    #[tokio::test]
    async fn a_dm_to_nobody_is_reported_not_swallowed() {
        let session = test_session();
        let buffers = Buffers::new();
        let target = buffers.route_submit(&session, &BufferId::Dm("ghost".to_string()), "hello?");
        assert!(matches!(deliver(&session, target), Delivery::Rejected(_)));
    }

    #[tokio::test]
    async fn a_channel_submit_lands_in_the_log_as_the_user() {
        let session = test_session();
        session
            .channels
            .create(
                "build",
                vec!["scout".to_string(), USER_NAME.to_string()],
                ChannelMode::Free,
            )
            .expect("channel created");
        let buffers = Buffers::new();
        let target =
            buffers.route_submit(&session, &BufferId::Channel("build".to_string()), "ship it");
        assert_eq!(deliver(&session, target), Delivery::Sent);

        let log = session.channels.log_of("build");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].from, USER_NAME);
        assert_eq!(log[0].text, "ship it");
    }

    // ---- drafts ---------------------------------------------------------

    #[test]
    fn drafts_round_trip_and_stay_in_their_own_conversation() {
        let session = test_session();
        seed_agent(&session, "scout", Vec::new());
        seed_agent(&session, "zoe", Vec::new());
        let mut buffers = Buffers::new();
        buffers.refresh(&session, 1);

        let scout = BufferId::Dm("scout".to_string());
        let zoe = BufferId::Dm("zoe".to_string());
        buffers.stash_draft(&scout, "half a thought".to_string());
        buffers.stash_draft(&zoe, "a different thought".to_string());

        assert_eq!(buffers.take_draft(&scout), "half a thought");
        assert_eq!(
            buffers.get(&zoe).map(|buffer| buffer.draft.as_str()),
            Some("a different thought"),
            "taking one draft leaves the others alone"
        );
        assert_eq!(
            buffers.take_draft(&scout),
            "",
            "a restored draft does not come back a second time"
        );
        assert_eq!(buffers.take_draft(&BufferId::Dm("ghost".to_string())), "");
    }
}

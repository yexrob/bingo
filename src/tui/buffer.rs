//! The conversation engine (D88): every conversation has the same shape.
//!
//! A buffer is one conversation — the hub, a DM with a subagent, an agent
//! channel, or the team board. The engine holds what is *about* a conversation
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
//!   the transcript and prints a line; the hub's row builder (`build_rows`)
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

use crate::api::types::{ContentBlock, Message, Role as ApiRole};
use crate::channels::{ChannelMessage, USER_NAME};
use crate::query::Session;
use crate::tui::chat::{Role, UiMessage};
use crate::watch::{WatchKind, WatchState};

/// How many lifecycle events the team board keeps. The board is the one buffer
/// with no domain store behind it (spawn/done/ack are broadcast and retained
/// nowhere), so it is also the one that has to bound itself.
const TEAM_LOG_MAX: usize = 200;

/// Which conversation a buffer is.
///
/// The derived ordering *is* the registry's ordering: hub, the team board,
/// channels by name, then DMs by name. Declaration order carries it, so there
/// is no second sort key to keep in step with this enum.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum BufferId {
    /// The main conversation with the model — the transcript already on screen.
    Hub,
    /// The team board: subagent lifecycle, read-only.
    Team,
    /// An agent channel (D29).
    Channel(String),
    /// A direct conversation with one subagent instance.
    Dm(String),
}

impl BufferId {
    /// The name this conversation goes by on screen.
    pub fn label(&self) -> String {
        match self {
            Self::Hub => "hub".to_string(),
            Self::Team => "#team".to_string(),
            Self::Channel(name) => format!("#{name}"),
            Self::Dm(name) => format!("@{name}"),
        }
    }

    /// The rule that opens this conversation in the flow, and the one that
    /// closes it. One formatter, so a replay and the hand-back to the hub can
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
    /// length, team-log length. The hub has no sequence and stays at 0.
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
}

/// One element of a buffer's replay.
///
/// Not comparable: `UiMessage` carries activities and fold state and has never
/// been an equality type. Tests read the fields they mean.
#[derive(Debug, Clone)]
pub enum Replay {
    /// The rule that opens the conversation on switch.
    Divider(String),
    /// A message from the source. `message` is the hub transcript's own unit, so
    /// the existing row builder renders it with the code the live flow uses;
    /// `who` carries the sender the hub's two roles cannot express, for the
    /// decorations D89 hangs on it.
    Message { who: String, message: UiMessage },
}

/// Where a composer submit belongs. Data only — [`deliver`] performs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitTarget {
    /// The hub's normal turn path.
    Turn(String),
    /// `AgentRegistry::deliver` under the user's name. The `[DM from user]`
    /// marker is *not* applied here and must not be: it is added downstream in
    /// `absorb_inbox`, derived from `from`, and adding it at both ends would
    /// double it (D64).
    Dm { agent: String, text: String },
    /// `tool::channel::deliver_post` under the user's name.
    Channel { channel: String, text: String },
    /// The board is a record of what happened, not a room to speak in.
    Refused(&'static str),
}

/// What came of a routed submit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// The host owns this one: run the text as a turn.
    Turn(String),
    Sent,
    /// A notice to put above the composer (English; the wording is final).
    Rejected(String),
}

/// One lifecycle event on the team board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamEvent {
    /// The watch label, which already carries the instance name and run.
    pub label: String,
    pub state: WatchState,
    pub detail: Option<String>,
    /// Unix seconds.
    pub at: u64,
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

/// The registry: the hub plus whatever the domain currently has.
#[derive(Debug, Clone)]
pub struct Buffers {
    /// Hub first, always; the rest in [`BufferId`] order.
    list: Vec<Buffer>,
    active: BufferId,
    team: Vec<TeamEvent>,
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
            // sorted, so the position is a search, and the hub stays at 0
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
    pub fn refresh(&mut self, session: &Arc<Session>, tick: u64) {
        let token = format!("@{USER_NAME}");
        for status in session.channels.list() {
            let id = BufferId::Channel(status.name.clone());
            let read = self.get(&id).map(|b| b.read).unwrap_or(status.seq);
            // Only worth reading the log when something in it is unread — this
            // runs on the hub's poll, not inside a modal that was already
            // cloning logs per frame.
            let mention = status.seq > read
                && session
                    .channels
                    .log_of(&status.name)
                    .iter()
                    .any(|m| m.seq > read && m.text.contains(&token));
            self.observe(id, status.seq, mention, tick);
        }
        for status in session.agents.list() {
            let seq = session
                .agents
                .view_of(&status.name)
                .map(|(history, ..)| history.len() as u64)
                .unwrap_or(0);
            // A DM is addressed to you by construction, so anything new in one
            // is a mention. There is no other kind of message in a DM.
            self.observe(BufferId::Dm(status.name), seq, true, tick);
        }
    }

    /// Tee of the lifecycle stream (`UiEvent::WatchEvent`).
    ///
    /// Only agent events reach the board. Channel events belong to their own
    /// buffer and command events are the hub's own tools, so neither is team
    /// news. The board materializes on the first event rather than standing
    /// empty in a session that never spawned anyone.
    pub fn note_watch_event(
        &mut self,
        label: &str,
        kind: WatchKind,
        state: WatchState,
        detail: Option<&str>,
        tick: u64,
    ) {
        if kind != WatchKind::Agent {
            return;
        }
        self.team.push(TeamEvent {
            label: label.to_string(),
            state,
            detail: detail.map(str::to_string),
            at: crate::channels::now_unix(),
        });
        if self.team.len() > TEAM_LOG_MAX {
            let over = self.team.len() - TEAM_LOG_MAX;
            self.team.drain(..over);
        }
        // The board is born with this event, so its first entry is news. The
        // "first sight is read" rule exists to avoid badging history that
        // predates you, and a board that did not exist a moment ago has none.
        if self.get(&BufferId::Team).is_none() {
            self.entry(BufferId::Team, 0, tick);
        }
        let seq = self.team.len() as u64;
        self.observe(BufferId::Team, seq, false, tick);
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
    /// The hub yields nothing: it is already on screen, and replaying it would
    /// print the transcript a second time under a rule.
    pub fn rehydrate(&self, session: &Arc<Session>, id: &BufferId, budget: usize) -> Vec<Replay> {
        if *id == BufferId::Hub {
            return Vec::new();
        }
        let posts: Vec<Post> = match id {
            BufferId::Hub => Vec::new(),
            BufferId::Team => self
                .team
                .iter()
                .map(|ev| Post {
                    from: ev.label.clone(),
                    you: false,
                    at: ev.at,
                    text: ev
                        .detail
                        .clone()
                        .unwrap_or_else(|| state_word(ev.state).to_string()),
                    kind: PostKind::Note,
                })
                .collect(),
            BufferId::Channel(name) => channel_posts(&session.channels.log_of(name), USER_NAME),
            BufferId::Dm(name) => match session.agents.view_of(name) {
                // Settled history only: the live tail, the in-flight claim and
                // the queued items are states, not record, and the host draws
                // those itself once it is showing the conversation.
                Some((history, stamps, ..)) => {
                    dm_posts(&history, &stamps, &[], &[], &[], name, USER_NAME)
                }
                None => Vec::new(),
            },
        };
        let mut out = vec![Replay::Divider(id.rule())];
        let kept: Vec<&Post> = posts
            .iter()
            .filter(|p| matches!(p.kind, PostKind::Said | PostKind::Note))
            .collect();
        let start = kept.len().saturating_sub(budget);
        out.extend(kept[start..].iter().map(|post| Replay::Message {
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
            },
        }));
        out
    }

    /// Where a composer submit in this conversation belongs.
    pub fn route_submit(&self, id: &BufferId, text: &str) -> SubmitTarget {
        let text = text.to_string();
        match id {
            BufferId::Hub => SubmitTarget::Turn(text),
            BufferId::Team => SubmitTarget::Refused("#team is a board, not a room"),
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
// one place a stored conversation becomes displayable messages — the D64
// `[DM from user]` handling, the scaffolding collapse and the live-turn tail
// all live in `dm_posts`, and a second parser beside it was the one thing worth
// avoiding.
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

/// Channel log → posts.
pub fn channel_posts(log: &[ChannelMessage], me: &str) -> Vec<Post> {
    log.iter()
        .map(|m| Post {
            from: m.from.clone(),
            you: m.from == me,
            at: m.at,
            text: m.text.clone(),
            kind: PostKind::Said,
        })
        .collect()
}

/// One line of runtime scaffolding → how it reads collapsed, or `None` for text
/// a person actually wrote. The shapes are the ones `absorb_inbox` and the task
/// reminder compose, so this stays in step with them by construction: anything
/// the runtime wraps in its own brackets is not a message from the user.
fn scaffold_note(line: &str) -> Option<String> {
    let line = line.trim_end();
    if let Some(rest) = line.strip_prefix("[#")
        && let Some((head, body)) = rest.split_once("] ")
        && let Some((channel, _)) = head.split_once(' ')
    {
        return Some(format!("#{channel} · {body}"));
    }
    if line.starts_with("[follow-up ") {
        return Some("follow-up · waiting for a reply".to_string());
    }
    None
}

/// A user-role message → posts. Runtime scaffolding collapses to [`PostKind::Note`]
/// lines; whatever a person actually wrote stays a message.
fn user_posts(text: &str, at: u64, me: &str) -> Vec<Post> {
    let mut out = Vec::new();
    let mut plain: Vec<&str> = Vec::new();
    let flush = |plain: &mut Vec<&str>, out: &mut Vec<Post>| {
        let joined = plain.join("\n");
        plain.clear();
        if joined.trim().is_empty() {
            return;
        }
        out.push(Post {
            from: me.to_string(),
            you: true,
            at,
            text: joined,
            kind: PostKind::Said,
        });
    };
    for line in text.lines() {
        // The task reminder is a block, not a line: its marker heads a paragraph
        // of instructions and the current task list, and everything after it in
        // this message belongs to it.
        if line.starts_with(crate::query::TASK_REMINDER_MARKER) {
            flush(&mut plain, &mut out);
            out.push(Post {
                from: me.to_string(),
                you: true,
                at: 0,
                text: "system note · task tools".to_string(),
                kind: PostKind::Note,
            });
            return out;
        }
        // The user's own DM marker is transport scaffolding: the bubble already says who
        // spoke, so the line is dropped rather than shown — but it still flushes, so two
        // batched DMs stay two bubbles instead of one merged paragraph.
        if line.trim_end() == crate::tool::agent::DM_FROM_USER_MARKER {
            flush(&mut plain, &mut out);
            continue;
        }
        match scaffold_note(line) {
            Some(note) => {
                flush(&mut plain, &mut out);
                out.push(Post {
                    from: me.to_string(),
                    you: true,
                    at: 0,
                    text: note,
                    kind: PostKind::Note,
                });
            }
            None => plain.push(line),
        }
    }
    flush(&mut plain, &mut out);
    out
}

/// The collapsed reasoning row, exactly the transcript's header: the phase is
/// shown, the stream is not.
const THINKING_ROW: &str = "✻ Thinking";

/// A stored tool-use block → the transcript's call line (`⏺ Bash(git status)`),
/// the same brick `on_tool_ready` builds the live tail from.
fn tool_call_line(name: &str, input: &serde_json::Value) -> String {
    let glyph = crate::tui::activities::tool_glyph(name);
    let shown = crate::tui::activities::display_tool_name(name);
    let summary = crate::query::summarize_input(name, input);
    if summary.is_empty() {
        format!("{glyph}{shown}")
    } else {
        format!("{glyph}{shown}({summary})")
    }
}

/// Subagent history + live turn → posts. User turns and assistant prose form
/// the conversation; the work between them — tool calls and reasoning phases —
/// shows as dim [`PostKind::Process`] rows, mirroring the main transcript
/// (which keeps `⏺ Tool(…)` lines and the collapsed `✻ Thinking` header in the
/// scrollback). `in_flight` is the messages already claimed by the running turn
/// but not yet landed in history: rendered as ordinary sent messages, so a
/// message never vanishes between the send and the turn's end.
pub fn dm_posts(
    history: &[Message],
    stamps: &[u64],
    in_flight: &[String],
    live: &[crate::agents::LiveBlock],
    pending: &[String],
    who: &str,
    me: &str,
) -> Vec<Post> {
    let mut out = Vec::new();
    let process = |text: String| Post {
        from: who.to_string(),
        you: false,
        at: 0,
        text,
        kind: PostKind::Process,
    };
    for (i, msg) in history.iter().enumerate() {
        // `stamps[i]` is when `history[i]` landed in the record (0 = no clock).
        let at = stamps.get(i).copied().unwrap_or(0);
        for block in &msg.content {
            match (msg.role, block) {
                (ApiRole::User, ContentBlock::Text { text }) => {
                    out.extend(user_posts(text, at, me))
                }
                (ApiRole::Assistant, ContentBlock::Text { text }) => out.push(Post {
                    from: who.to_string(),
                    you: false,
                    at,
                    text: text.clone(),
                    kind: PostKind::Said,
                }),
                (ApiRole::Assistant, ContentBlock::ToolUse { name, input, .. }) => {
                    out.push(process(tool_call_line(name, input)));
                }
                (ApiRole::Assistant, ContentBlock::Thinking { .. }) => {
                    out.push(process(THINKING_ROW.to_string()));
                }
                _ => {}
            }
        }
    }
    // Claimed by the running turn, not yet in the record: an ordinary message
    // (it is one — the run's prompt carries it), just without a landing clock.
    for text in in_flight {
        out.push(Post {
            from: me.to_string(),
            you: true,
            at: 0,
            text: text.clone(),
            kind: PostKind::Said,
        });
    }
    for text in pending {
        out.push(Post {
            from: me.to_string(),
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

/// Send-time stamp trailing a message body (issue #41), the same in every view:
/// local `HH:MM` today, `M/D HH:MM` on any other day. Empty when the source
/// carries no timestamp — a missing clock renders as nothing rather than being
/// invented (the [`ChannelMessage`] rule).
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

    fn ids(buffers: &Buffers) -> Vec<String> {
        buffers.iter().map(|b| b.id().label()).collect()
    }

    // ---- registry -------------------------------------------------------

    #[test]
    fn the_hub_is_there_before_anything_else_is() {
        let buffers = Buffers::new();
        assert_eq!(ids(&buffers), vec!["hub"]);
        assert_eq!(*buffers.active(), BufferId::Hub);
        assert_eq!(buffers.get(&BufferId::Hub).map(Buffer::unread), Some(0));
    }

    /// One vocabulary for naming a conversation: the label the id goes by, the
    /// rule the flow opens it with, and `Display`. D88 stated this as a `Source`
    /// enum nothing consulted; D89 made [`BufferId::rule`] load-bearing (the
    /// replay and the hand-back to the hub both read it), so the property is
    /// checked where it is now used.
    #[test]
    fn an_id_names_its_conversation_in_one_vocabulary() {
        assert_eq!(BufferId::Hub.label(), "hub");
        assert_eq!(BufferId::Team.label(), "#team");
        assert_eq!(BufferId::Channel("build".to_string()).label(), "#build");
        assert_eq!(BufferId::Dm("scout".to_string()).label(), "@scout");
        assert_eq!(BufferId::Dm("scout".to_string()).to_string(), "@scout");
        // The rule is the label and nothing else, so a divider can never name a
        // conversation differently from the way it is addressed.
        for id in [
            BufferId::Hub,
            BufferId::Team,
            BufferId::Channel("build".to_string()),
            BufferId::Dm("scout".to_string()),
        ] {
            assert_eq!(id.rule(), format!("── {} ──", id.label()));
        }
        assert_eq!(Buffers::new().iter().count(), 1, "the hub is always there");
    }

    #[test]
    fn conversations_materialize_from_the_domain_in_one_order() {
        let session = test_session();
        session
            .channels
            .create("build", vec!["scout".to_string()], ChannelMode::Free)
            .expect("channel created");
        session
            .channels
            .create("alpha", vec!["scout".to_string()], ChannelMode::Free)
            .expect("channel created");
        seed_agent(&session, "zoe", Vec::new());
        seed_agent(&session, "scout", Vec::new());

        let mut buffers = Buffers::new();
        buffers.refresh(&session, 1);
        buffers.note_watch_event(
            "scout #1 · go",
            WatchKind::Agent,
            WatchState::Running,
            None,
            1,
        );

        // Hub, the board, channels by name, DMs by name — and the hub stays at 0
        // however many conversations arrive after it.
        assert_eq!(
            ids(&buffers),
            vec!["hub", "#team", "#alpha", "#build", "@scout", "@zoe"]
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

    #[test]
    fn unread_counts_one_per_message_and_moves_the_stamp() {
        let session = test_session();
        seed_agent(&session, "scout", Vec::new());
        let mut buffers = Buffers::new();
        buffers.refresh(&session, 1);
        let id = BufferId::Dm("scout".to_string());
        assert_eq!(buffers.get(&id).map(|buffer| buffer.last_activity), Some(1));

        session.agents.finish("scout", vec![assistant("one")], 0);
        buffers.refresh(&session, 7);
        assert_eq!(buffers.get(&id).map(Buffer::unread), Some(1));
        assert_eq!(buffers.get(&id).map(|buffer| buffer.last_activity), Some(7));

        session
            .agents
            .finish("scout", vec![assistant("one"), assistant("two")], 0);
        buffers.refresh(&session, 9);
        assert_eq!(buffers.get(&id).map(Buffer::unread), Some(2));
        assert_eq!(buffers.get(&id).map(|buffer| buffer.last_activity), Some(9));

        // A poll that finds nothing new leaves the stamp where it was.
        buffers.refresh(&session, 30);
        assert_eq!(buffers.get(&id).map(|buffer| buffer.last_activity), Some(9));
    }

    #[test]
    fn a_dm_is_addressed_to_you_by_construction() {
        let mut buffers = Buffers::new();
        buffers.observe(BufferId::Dm("scout".to_string()), 0, true, 1);
        buffers.observe(BufferId::Dm("scout".to_string()), 1, true, 2);
        let buf = buffers.get(&BufferId::Dm("scout".to_string()));
        assert_eq!(buf.map(Buffer::unread), Some(1));
        assert_eq!(buf.map(Buffer::mention), Some(true));
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
    fn the_hub_never_counts_its_own_flow() {
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

    // ---- the team board -------------------------------------------------

    #[test]
    fn the_board_hears_agents_and_nothing_else() {
        let mut buffers = Buffers::new();
        buffers.note_watch_event("ls", WatchKind::Command, WatchState::Done, None, 1);
        buffers.note_watch_event("#build", WatchKind::Channel, WatchState::Running, None, 1);
        assert!(
            buffers.get(&BufferId::Team).is_none(),
            "a command and a room post are not team news"
        );

        buffers.note_watch_event(
            "scout #1 · fix it",
            WatchKind::Agent,
            WatchState::Done,
            Some("done"),
            4,
        );
        let board = buffers.get(&BufferId::Team).expect("the board appeared");
        assert_eq!(board.unread(), 1);
        assert_eq!(board.last_activity, 4);
        assert_eq!(buffers.team.len(), 1);
    }

    #[test]
    fn the_board_bounds_what_it_remembers() {
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
        assert_eq!(buffers.team.len(), TEAM_LOG_MAX);
        assert_eq!(
            buffers.team[0].label,
            format!("scout #{}", 40),
            "the oldest events fall off the front"
        );
    }

    // ---- rehydrate ------------------------------------------------------

    fn texts(replay: &[Replay]) -> Vec<String> {
        replay
            .iter()
            .map(|r| match r {
                Replay::Divider(text) => text.clone(),
                Replay::Message { message, .. } => message.text.clone(),
            })
            .collect()
    }

    #[test]
    fn the_hub_replays_nothing_because_it_is_already_on_screen() {
        let session = test_session();
        let buffers = Buffers::new();
        assert!(
            buffers.rehydrate(&session, &BufferId::Hub, 50).is_empty(),
            "replaying the hub would print the transcript twice"
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
        let posts = dm_posts(&stored, &stamps, &[], &[], &[], "scout", USER_NAME);

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
    fn a_channel_replays_its_log() {
        let session = test_session();
        session
            .channels
            .create("build", vec!["scout".to_string()], ChannelMode::Free)
            .expect("channel created");
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

    #[test]
    fn a_replay_keeps_to_its_budget() {
        let session = test_session();
        seed_agent(
            &session,
            "scout",
            vec![assistant("one"), assistant("two"), assistant("three")],
        );
        let buffers = Buffers::new();
        let replay = buffers.rehydrate(&session, &BufferId::Dm("scout".to_string()), 2);
        assert_eq!(
            texts(&replay),
            vec!["── @scout ──", "two", "three"],
            "the budget keeps the tail, not the head"
        );
    }

    #[test]
    fn the_board_replays_its_lifecycle_log() {
        let session = test_session();
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
        let replay = buffers.rehydrate(&session, &BufferId::Team, 50);
        assert_eq!(
            texts(&replay),
            vec!["── #team ──", "running", "fixed the parser"]
        );
    }

    // ---- submit routing -------------------------------------------------

    #[test]
    fn every_conversation_routes_to_its_own_path() {
        let buffers = Buffers::new();
        assert_eq!(
            buffers.route_submit(&BufferId::Hub, "hello"),
            SubmitTarget::Turn("hello".to_string())
        );
        assert_eq!(
            buffers.route_submit(&BufferId::Dm("scout".to_string()), "hello"),
            SubmitTarget::Dm {
                agent: "scout".to_string(),
                text: "hello".to_string()
            }
        );
        assert_eq!(
            buffers.route_submit(&BufferId::Channel("build".to_string()), "hello"),
            SubmitTarget::Channel {
                channel: "build".to_string(),
                text: "hello".to_string()
            }
        );
        assert!(matches!(
            buffers.route_submit(&BufferId::Team, "hello"),
            SubmitTarget::Refused(_)
        ));
    }

    #[tokio::test]
    async fn the_board_refuses_to_be_spoken_in() {
        let session = test_session();
        let buffers = Buffers::new();
        let target = buffers.route_submit(&BufferId::Team, "nice work everyone");
        match deliver(&session, target) {
            Delivery::Rejected(why) => assert_eq!(why, "#team is a board, not a room"),
            other => panic!("the board is read-only, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_dm_reaches_the_agent_under_the_user_marker() {
        let session = test_session();
        seed_agent(&session, "scout", Vec::new());
        let buffers = Buffers::new();
        let target = buffers.route_submit(&BufferId::Dm("scout".to_string()), "have a look");
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
        let target = buffers.route_submit(&BufferId::Dm("ghost".to_string()), "hello?");
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
        let target = buffers.route_submit(&BufferId::Channel("build".to_string()), "ship it");
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

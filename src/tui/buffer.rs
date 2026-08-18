//! The conversation registry (D88, D103): every conversation has the same
//! accounting shape.
//!
//! A buffer is one conversation — `@main`, a DM with a subagent, or a room the
//! user is in. The registry holds what is *about* a conversation (how far you
//! have read, whether it wants you, when it last moved) and nothing of what is
//! *in* one: a buffer's transcript stays where the domain already keeps it, and
//! [`BufferId`] is the key that reaches it. There is no second copy of any
//! message here, and nothing is written to disk — unread marks are
//! session-local by construction, and the registry rebuilds itself from the
//! domain on the next start.
//!
//! **Unread is derived, not counted.** The workspace has always computed
//! `seq - read_cursor` fresh on every frame rather than incrementing a counter
//! (`entity.rs::snapshot`). The registry keeps that: [`Buffers::refresh`]
//! re-reads the sequence numbers and the badge falls out of the subtraction. A
//! counter fed by events can drift from the thing it counts; a cursor cannot.
//!
//! D89 built a view layer on top of this — buffers you could switch to, spliced
//! into one flow — and D103 retired it whole. What is left is the **book-keeping
//! half**, which is what D104's footer pills and agent tree read. The post
//! extraction rules that used to live at the bottom of this file retired with
//! the alt-screen zoom (v6): a page's content is built by
//! [`crate::tui::conv`] from the domain directly.

use std::sync::Arc;

use crate::channels::{USER_NAME, names};
use crate::query::Session;

/// Which conversation a buffer is.
///
/// The derived ordering *is* the registry's ordering: `@main`, rooms by name,
/// then DMs by name. Declaration order carries it, so there is no second sort
/// key to keep in step with this enum.
///
/// **There is no `Team` variant, and that is the D95 ruling.** The team is the
/// organization, not a conversation: you cannot speak to it, and everything it
/// had to say — who exists, what rooms there are, what just happened — is a
/// directory — the Agents and Rooms sections of the background dialog
/// ([`crate::tui::background`], D107) — rather than a board you visit and a
/// badge that asks you to. A read-only buffer in the bar was a
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
    /// length. Main has no sequence and stays at 0.
    seq: u64,
    /// How far the user has read, in the same unit.
    read: u64,
    /// Something addressed to the user is waiting.
    mention: bool,
    /// Tick of the last observed change in the source.
    last_activity: u64,
}

/// The accounting readers, and the surface that asks for them.
///
/// **The background dialog** (D107). D103 kept these alive through a batch
/// that had no use for them and D104 declined them again, for CC's reason:
/// its `AgentPill` renders `@name` and nothing else, and a tree row is
/// `@name: <what it is doing> · <what that cost>` — no counter, no dot, both
/// taking their dim and bold from the agent's *state*, which is the registry's
/// answer rather than this store's. The dialog is the surface where "three
/// unread from @scout" is the thing being asked for: its rows carry the count
/// and its sections are ordered by what moved last.
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
    /// Tick of the last observed change in the source. A registry sorted by
    /// name answers "what exists"; this is what answers "what just happened",
    /// and it is how the dialog orders its sections.
    ///
    /// **An order, not a duration.** The host's frame counter only advances
    /// while something is happening ([`crate::tui::chat::Chat::needs_tick`]),
    /// so two of these can be compared and neither can be subtracted from the
    /// clock on the wall.
    pub fn last_activity(&self) -> u64 {
        self.last_activity
    }
}

/// Where a direct send belongs. Data only — [`deliver`] performs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitTarget {
    /// `AgentHandle::deliver` under the user's name. The `[DM from user]`
    /// marker is *not* applied here and must not be: it is added downstream in
    /// `absorb_inbox`, derived from `from`, and adding it at both ends would
    /// double it (D64).
    Dm { agent: String, text: String },
    /// `tool::channel::deliver_post` under the user's name.
    Channel { channel: String, text: String },
}

/// What came of a direct send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    /// A notice to put above the composer (English; the wording is final).
    Rejected(String),
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

/// The registry: main plus whatever the domain currently has.
#[derive(Debug, Clone)]
pub struct Buffers {
    /// Home first, always; the rest in [`BufferId`] order.
    list: Vec<Buffer>,
    active: BufferId,
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
                last_activity: 0,
            }],
            active: BufferId::Hub,
            said: std::collections::HashMap::new(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn active(&self) -> &BufferId {
        &self.active
    }

    /// Point the accounting at the conversation being read: entering one reads
    /// it, so nothing in it is unread while it is on screen.
    ///
    /// Between D103 and D105 the answer was always `@main`, because the
    /// transcript was the only thing there was to read; the zoom moves it
    /// again, and D107's dialog is where the badge it clears is read.
    pub fn set_active(&mut self, id: BufferId) {
        self.active = id.clone();
        self.mark_read(&id);
    }

    pub fn get(&self, id: &BufferId) -> Option<&Buffer> {
        self.list.iter().find(|b| b.id == *id)
    }

    /// Every conversation, `@main` first and the rest in [`BufferId`] order.
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
    /// findable in the dialog and readable there. So a room is listed while
    /// the user is a member of it and drops out of the registry when they
    /// leave, which is what makes the store mean "conversations I am in".
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
    /// messages; the measure a reader wants is "process rows are work, not
    /// messages". **And a mention
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
                .filter(|item| item.kind == PostKind::Said)
                .map(|item| !item.you)
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

    /// Mark everything in a conversation read.
    pub fn mark_read(&mut self, id: &BufferId) {
        if let Some(buf) = self.list.iter_mut().find(|b| b.id == *id) {
            buf.read = buf.seq;
            buf.mention = false;
        }
    }
}

/// Perform a direct send against the domain — the same two calls every other
/// path makes, in the same order, so a message the user types into the
/// transcript is indistinguishable at the domain from one a tool delivered.
pub fn deliver(session: &Arc<Session>, target: SubmitTarget) -> Delivery {
    match target {
        SubmitTarget::Dm { agent, text } => {
            match session
                .agents
                .deliver(&agent, USER_NAME, &text, Vec::new(), None)
                .now()
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
// wants — one agent's whole record and its live turn (`zoom_posts`), a room's log
// (`channel_posts`), and the settled elements a replay prints (`pair_replay`).
// ---------------------------------------------------------------------------

/// What a message row shows besides its text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostKind {
    /// An ordinary message.
    Said,
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

/// What one line of a stored user-role message *is*, by the shape the runtime
/// composed it in (`absorb_inbox`, the task reminder, the steer path).
///
/// **One parser, one walk.** The shapes are recognised here and nowhere else,
/// and [`crate::tui::perspective::walk`] is the single reader that turns them
/// into attributed posts. The zoom keeps every lane the walk files; the user's
/// `@X` pair lane keeps the one lane it is in and drops the rest, which is why
/// a room relay or a chase does not count as something the agent said to the
/// user (D99) — it is not the user's conversation to read there.
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
    if let Some(rest) = line.strip_prefix(crate::channels::AGENT_MESSAGE_PREFIX)
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
    /// which is exactly what the pair lane has to tell apart (D99).
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
        session
            .agents
            .insert(
                name,
                AgentKind::Hire,
                None,
                "test instance".to_string(),
                session.clone(),
            )
            .now();
        if !history.is_empty() {
            session.agents.finish(name, history, 0).now();
        }
    }

    /// A crew member (D53): a teammate from the blueprint rather than a hire.
    fn seed_crew(session: &Arc<Session>, name: &str) {
        session
            .agents
            .insert(
                name,
                AgentKind::Crew,
                None,
                "crew member".to_string(),
                session.clone(),
            )
            .now();
    }

    /// A room the user is in — the ordinary case for anything the store
    /// lists.
    fn seed_room(session: &Arc<Session>, name: &str, members: &[&str]) {
        let mut roster = vec![USER_NAME.to_string()];
        roster.extend(members.iter().map(|m| m.to_string()));
        session
            .channels
            .create(name, roster, ChannelMode::Free)
            .now()
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

    /// One vocabulary for naming a conversation: the label the id goes by and
    /// `Display`. D89's divider read the same formatter and retired with the
    /// flow it drew into (D103); the label is what D104's pills and tree print.
    #[test]
    fn an_id_names_its_conversation_in_one_vocabulary() {
        assert_eq!(BufferId::Hub.label(), "@main");
        assert_eq!(BufferId::Channel("build".to_string()).label(), "#build");
        assert_eq!(BufferId::Dm("scout".to_string()).label(), "@scout");
        assert_eq!(BufferId::Dm("scout".to_string()).to_string(), "@scout");
        // D101: the home conversation is spelled the way every participant is.
        assert_eq!(BufferId::Hub.to_string(), "@main");
        for id in [
            BufferId::Hub,
            BufferId::Channel("build".to_string()),
            BufferId::Dm("scout".to_string()),
        ] {
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

        // Home, rooms by name, DMs by name — and main stays at 0 however many
        // conversations arrive after it. The team is not among them: it is the
        // organization, not a conversation (D95), and D107's dialog is where
        // it is read.
        assert_eq!(
            ids(&buffers),
            vec!["@main", "#alpha", "#build", "@scout", "@zoe"]
        );
    }

    /// The store is "conversations I am in". A room formed by two agents is real,
    /// findable in the dialog and readable — but it is not one of the user's
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
            .now()
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
            .now()
            .expect("joined");
        buffers.refresh(&session, 2);
        assert_eq!(ids(&buffers), vec!["@main", "#parser"], "joining lists it");

        session
            .channels
            .kick("parser", USER_NAME)
            .now()
            .expect("left");
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
        let id = BufferId::Dm("scout".to_string());
        buffers.observe(id.clone(), 3, true, 1);
        // Three more polls must not clone the conversation or lose what it
        // knows: the accounting is the thing that has to survive a sweep.
        buffers.refresh(&session, 2);
        buffers.refresh(&session, 3);
        buffers.refresh(&session, 4);
        assert_eq!(buffers.iter().count(), before, "refresh is idempotent");
        assert_eq!(buffers.get(&id).map(Buffer::mention), Some(true));
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
        session.agents.finish("scout", history.clone(), 0).now();
        buffers.refresh(&session, 7);
        assert_eq!(buffers.get(&id).map(Buffer::unread), Some(2));
        assert_eq!(buffers.get(&id).map(|buffer| buffer.last_activity), Some(7));

        history.push(assistant("two"));
        session.agents.finish("scout", history, 0).now();
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
        session.agents.finish("scout", history, 0).now();
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
            .finish("scout", vec![from_user("look at the parser")], 0)
            .now();
        buffers.refresh(&session, 2);
        assert_eq!(buffers.get(&id).map(Buffer::unread), Some(1));
        assert_eq!(buffers.get(&id).map(Buffer::mention), Some(false));

        session
            .agents
            .finish(
                "scout",
                vec![from_user("look at the parser"), assistant("found it")],
                0,
            )
            .now();
        buffers.refresh(&session, 3);
        assert_eq!(buffers.get(&id).map(Buffer::mention), Some(true));

        // And main's traffic through the same instance is not a DM at all.
        buffers.mark_read(&id);
        session
            .agents
            .finish(
                "scout",
                vec![
                    from_user("look at the parser"),
                    assistant("found it"),
                    Message::user_text("also check the lexer"),
                    assistant("checked"),
                ],
                0,
            )
            .now();
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
            .now()
            .expect("channel created");
        let mut buffers = Buffers::new();
        buffers.refresh(&session, 1);
        let id = BufferId::Channel("build".to_string());

        session
            .channels
            .post("scout", "build", "landed the refactor")
            .now()
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
            .now()
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

    // ---- the zoomed view's projection ------------------------------------
    //
    // D103 kept the pair replay for this batch; D105 found a shorter road and
    // these are its tests, moved onto what replaced it. The run folding they
    // were about is still the assertion — over the *whole* record now, which
    // is more than the pair lane ever held — and it lands in a [`Post`] the one
    // row renderer already draws instead of in a `UiMessage` only the console
    // can.

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

    // ---- delivery -------------------------------------------------------

    /// The two halves of a direct send, at the domain. Rewritten for D103: the
    /// router that used to decide between them was the composer-in-a-buffer's,
    /// and the composer is main's again — the target is now decided by the
    /// sigil the user typed, and this is what the sigil resolves to.
    #[tokio::test]
    async fn a_dm_reaches_the_agent_under_the_user_marker() {
        let session = test_session();
        seed_agent(&session, "scout", Vec::new());
        let target = SubmitTarget::Dm {
            agent: "scout".to_string(),
            text: "have a look".to_string(),
        };
        assert_eq!(deliver(&session, target), Delivery::Sent);

        // An inbox item from `user`, which is what earns the D64 marker when
        // the instance picks it up. The send path must not add the marker
        // itself — that would double it.
        let items = session.agents.take_running("scout", 0).await;
        let (prompt, _) = crate::tool::agent::absorb_inbox(&session.channels, "scout", &items);
        assert_eq!(
            prompt,
            format!("{}\nhave a look", crate::tool::agent::DM_FROM_USER_MARKER)
        );
    }

    #[tokio::test]
    async fn a_dm_to_nobody_is_reported_not_swallowed() {
        let session = test_session();
        let target = SubmitTarget::Dm {
            agent: "ghost".to_string(),
            text: "hello?".to_string(),
        };
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
            .await
            .expect("channel created");
        let target = SubmitTarget::Channel {
            channel: "build".to_string(),
            text: "ship it".to_string(),
        };
        assert_eq!(deliver(&session, target), Delivery::Sent);

        let log = session.channels.log_of("build");
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].from, USER_NAME);
        assert_eq!(log[0].text, "ship it");
    }
}

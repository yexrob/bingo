//! What a page's transcript is made of: the builder every run goes through,
//! and the two ways a store gets filled that are not the event channel.
//!
//! **The page itself is no longer here.** v6 answered "look at an agent" with a
//! projection: [`crate::tui::chat::Chat`] kept main's fields, an agent's page
//! was rebuilt from the registry whenever a fingerprint moved, and the result
//! was swapped into those fields for the length of one `build_rows`. That
//! asymmetry is what D130 and D132 were about — the rebuild dropped tool
//! results, and its live half had a renderer of its own — and D134 removes it:
//! every conversation owns a store, every store is fed by addressed `UiEvent`s,
//! and the screen points at one of them.
//!
//! What survives is what a store cannot learn from an event:
//!
//! - **A cold start** ([`agent_history`]). A conversation the console never saw
//!   streaming — an instance restored from a session — has only its API
//!   history. [`crate::app::projection::walk`] builds the store once, and
//!   events append from there. Never per frame, and (D135) never while an
//!   event for that conversation is still in the channel: the walk reads the
//!   registry as it stands *now*, so a queued run that has already committed
//!   would be read once and streamed again.
//! - **An inbound line** ([`inbound_messages`]). Prose enters a conversation
//!   from outside its own turn: the task an instance was dispatched with, the
//!   mail a continuation absorbed. It arrives as `UiEvent::Inbound` and is filed
//!   by the same walk, so the two halves of a page cannot disagree about who
//!   said what.
//! - **A room** ([`room_tail`]). And this one *stays* a projection, deliberately:
//!   a room is a log, not a turn loop. There is no stream to push, so its store
//!   is appended from the channel's own log as it grows — which is incremental,
//!   not a rebuild, so the settled prefix under the flush cursor never moves.
//!
//! **Switching is still a page turn** ([`crate::tui::term::InlineTerm::page_break`]):
//! the page being left banks its printed rows into the terminal's scrollback and
//! the next page starts at the top of a clean screen. Coming home reprints
//! main's recent tail (the resize `rehydrate` machinery, D27's accepted
//! duplicate), because scrollback cannot be un-banked — the D98 ruling.
//!
//! **And one builder for every run** (D132). [`RunBuilder`] fills what the
//! console's own events fill: results, folds, status, stamps. A row looks the
//! same whether it came back a second ago or a session ago.

use std::sync::Arc;

use crate::agents::ToolAnswer;
use crate::api::types::Message;
use crate::app::projection::PostKind;
use crate::app::projection::{Filed, Protagonist, Target, Work, walk};
use crate::channels::USER_NAME;
use crate::query::Session;
use crate::tui::activities::{Activity, ActivityKind, EXPAND_HINT, ToolStatus};
use crate::tui::chat::{
    Chat, Role, UiMessage, group_ready_tool, result_content, result_summary, skill_result_summary,
};
use crate::tui::conversation::Conversation;
use crate::tui::line::Line;
use crate::tui::zoom::ZoomTarget;
use crate::ui::ConvKey;

/// One protagonist run under construction: prose and activities interleaved
/// exactly as the live pipeline records them.
struct RunBuilder {
    msg: UiMessage,
    prose: bool,
}

impl RunBuilder {
    fn new(speaker: &str, at: u64) -> Self {
        Self {
            msg: UiMessage {
                role: Role::Assistant,
                text: String::new(),
                at,
                speaker: Some(speaker.to_string()),
                activities: Vec::new(),
                insert_points: Vec::new(),
                groups: Vec::new(),
                group_of: Vec::new(),
            },
            prose: false,
        }
    }

    fn prose(&mut self, text: &str) {
        if self.prose {
            self.msg.text.push_str("\n\n");
        }
        self.msg.text.push_str(text);
        self.prose = true;
        // Prose closes the open fold, exactly as `UiEvent::TextDelta` does on
        // main's page: a sentence between two tool streaks is a boundary, and a
        // group that swallowed it would summarise across a topic change.
        if let Some(group) = self.msg.groups.last_mut() {
            group.active = false;
        }
    }

    fn tool(
        &mut self,
        name: &str,
        input: &serde_json::Value,
        result: Option<&ToolAnswer>,
        pending: ToolStatus,
    ) {
        // The live path leaks tool names to `'static` too (`UiEvent::ToolStart`);
        // the vocabulary is the tool registry, so the leak is bounded.
        let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
        let call = crate::tui::activities::ToolCall::running(
            leaked,
            crate::query::summarize_input(name, input),
        );
        let idx = self.msg.activities.len();
        let text_len = self.msg.text.chars().count();
        self.msg
            .activities
            .push(Activity::new(ActivityKind::Tool(call)));
        self.msg.insert_points.push(text_len);
        self.msg.group_of.push(None);
        // Grouping first: whether the row is inside a fold is what decides
        // between a counted summary and the raw first line, the same branch the
        // console takes on `ToolDone`.
        group_ready_tool(&mut self.msg, idx, name, input);
        let in_group = self.msg.group_of[idx].is_some();
        let content = result.map(|r| result_content(name, &r.output));
        let summary = result.and_then(|r| {
            if in_group {
                result_summary(name, &r.output)
            } else if name == "Skill" {
                skill_result_summary(&r.output)
            } else {
                None
            }
        });
        let act = &mut self.msg.activities[idx];
        act.expand_hint = Some(EXPAND_HINT.to_string());
        if let Some(content) = content {
            act.set_content(content);
        }
        if let ActivityKind::Tool(call) = &mut act.kind {
            // No id: a row built from a record or from a settled live call is
            // never answered again, so there is nothing left to match.
            call.status = match result {
                Some(r) if r.is_error => ToolStatus::Error,
                Some(_) => ToolStatus::Done,
                // What no answer means is the caller's to say, and the two
                // callers mean opposite things: in a committed history — written
                // when the run ends — the call never got one and the run was cut
                // short; in a run still going it simply has not come back yet.
                None => pending,
            };
            call.result_summary = summary;
        }
    }

    fn thinking(&mut self, text: &str, render: &mut dyn FnMut(&str) -> Vec<Line>) {
        let text_len = self.msg.text.chars().count();
        let mut act = Activity::new(ActivityKind::Thinking(crate::tui::activities::Thinking {
            state: crate::tui::activities::ThinkingState::Done,
            // Not in the history and not invented: the row simply carries no
            // clock, which is what a zero reads as everywhere else.
            duration_ms: 0,
            stage: "Thinking",
            done_verb: None,
            start_tick: 0,
            segments: 1,
            timed: false,
        }));
        act.set_content(render(text));
        act.expand_hint = Some(EXPAND_HINT.to_string());
        self.msg.activities.push(act);
        self.msg.insert_points.push(text_len);
        self.msg.group_of.push(None);
    }

    fn finish(self) -> Option<UiMessage> {
        (self.prose || !self.msg.activities.is_empty()).then_some(self.msg)
    }
}

/// A counterpart's (or the runtime's) single entry as one message.
pub(crate) fn counterpart_message(from: &str, at: u64, text: String) -> UiMessage {
    // The user keeps the bubble they wear on main's page; everybody else —
    // main's instructions, another agent's mail, the runtime's notes — is
    // prose with (or without) a face, exactly as main's own replies are.
    let (role, speaker) = if from == USER_NAME {
        (Role::User, Some(USER_NAME.to_string()))
    } else if from.is_empty() {
        (Role::Assistant, None)
    } else {
        (Role::Assistant, Some(from.to_string()))
    };
    UiMessage {
        role,
        text,
        at,
        speaker,
        activities: Vec::new(),
        insert_points: Vec::new(),
        groups: Vec::new(),
        group_of: Vec::new(),
    }
}

/// An instance's committed record → the store it starts life with.
///
/// The cold start, and only that: a conversation the console has never seen
/// streaming has its whole past in the API history and nowhere else. Everything
/// after this point arrives as an event and is appended, so this runs once per
/// conversation rather than once per frame.
pub(crate) fn agent_history(
    session: &Arc<Session>,
    name: &str,
    render: &mut dyn FnMut(&str) -> Vec<Line>,
) -> Vec<UiMessage> {
    let Some((history, stamps, _state)) = session.agents.view_of(name) else {
        return Vec::new();
    };
    file_walk(
        walk(Protagonist::of(name), &history, &stamps),
        name,
        render,
        true,
    )
}

/// One inbound prompt → the messages it puts in the receiver's transcript.
///
/// `intake` says whether this is the task that created the instance, which is
/// the one shape a continuation's mail can never be — the same flag the walk
/// carries over a whole history, applied to the one message this is.
///
/// **Every direct message is dropped here** (D135). A DM reaches an instance
/// twice: [`crate::ui::UiEvent::Mail`] when it is delivered, and again inside
/// the prompt the run absorbs. The delivery is the one that happened at the
/// moment somebody sent it, so it is the one that stays — and *this* copy would
/// arrive whenever the receiver got round to reading it, which for a busy
/// instance is minutes later and in the wrong place.
///
/// The rule is exactly the DM lane: mail from the user, from main, from another
/// agent. Everything else in an absorbed prompt — the spawn task, a room relay,
/// a chase, the task reminder — never passed through `deliver`, so it has no
/// delivery event and lands here. A cold start keeps all of it, because there
/// was no console to hear the deliveries.
pub(crate) fn inbound_messages(
    name: &str,
    text: &str,
    at: u64,
    intake: bool,
    render: &mut dyn FnMut(&str) -> Vec<Line>,
) -> Vec<UiMessage> {
    let mut who = Protagonist::of(name);
    who.spawned = intake;
    let history = [Message {
        role: crate::api::types::Role::User,
        content: vec![crate::api::types::ContentBlock::Text {
            text: text.to_string(),
        }],
    }];
    let filed = walk(who, &history, &[at])
        .into_iter()
        .filter(|f| !matches!(&f.target, Target::Dm(_)))
        .collect();
    file_walk(filed, name, render, false)
}

/// A filed walk → messages: the protagonist's own rows collected into runs by
/// [`RunBuilder`], everything else a counterpart's line.
///
/// `own_runs` is false for an inbound prompt, which by construction contains
/// none of the receiver's own work — the flag says so rather than letting an
/// empty branch imply it.
fn file_walk(
    filed: Vec<Filed>,
    name: &str,
    render: &mut dyn FnMut(&str) -> Vec<Line>,
    own_runs: bool,
) -> Vec<UiMessage> {
    let mut out: Vec<UiMessage> = Vec::new();
    let mut run: Option<RunBuilder> = None;
    for Filed { target, post, work } in filed {
        let own = own_runs && post.from == name && !matches!(target, Target::Intake);
        if own {
            let builder = run.get_or_insert_with(|| RunBuilder::new(name, post.at));
            match work {
                Some(Work::Tool {
                    name,
                    input,
                    result,
                }) => builder.tool(&name, &input, result.as_ref(), ToolStatus::Interrupted),
                Some(Work::Thinking { text }) => builder.thinking(&text, render),
                None => {
                    if post.kind == PostKind::Said {
                        builder.prose(&post.text);
                    }
                }
            }
            continue;
        }
        // Anything that is not the protagonist's own row ends the run — the
        // D99 boundary: every continuation is triggered by something, and
        // that something is a break.
        if let Some(done) = run.take().and_then(RunBuilder::finish) {
            out.push(done);
        }
        out.push(counterpart_message(&post.from, post.at, post.text));
    }
    if let Some(done) = run.and_then(RunBuilder::finish) {
        out.push(done);
    }
    out
}

/// A room's log from `from` on → the page's messages: **speech only** (the v6
/// ruling), each post wearing its sender. The user's own posts keep their bubble.
///
/// **The one projection that stays, and why.** Every other conversation has a
/// turn loop, so its transcript is what its turn produced and events carry it.
/// A room has no turn: it is a log that other people append to, and the console
/// hears about it by looking. Looking is cheap here because the log is
/// append-only — this returns the tail past what the store already has, so the
/// settled prefix under the flush cursor never moves and nothing is rebuilt.
pub(crate) fn room_tail(session: &Arc<Session>, room: &str, from: usize) -> Vec<UiMessage> {
    session
        .channels
        .log_of(room)
        .iter()
        .skip(from)
        .filter(|m| m.kind == crate::channels::MessageKind::Said)
        .map(|m| counterpart_message(&m.from, m.at, m.text.clone()))
        .collect()
}

/// How many entries of `room`'s log the store has already seen — the cursor
/// [`room_tail`] resumes from. Counted over the whole log rather than over the
/// messages kept, because the filter drops membership lines and a cursor into
/// the filtered list would lose its place the first time somebody joined.
pub(crate) fn room_log_len(session: &Arc<Session>, room: &str) -> usize {
    session.channels.log_of(room).len()
}

impl Chat {
    /// Point the terminal at another conversation — or home (`None`).
    ///
    /// The store the screen leaves is parked, not dropped: main's turn keeps
    /// writing into it while an agent's page is up, exactly as an agent's turn
    /// has been writing into its own while main had the screen. The page being
    /// left is finished into scrollback by the host's page break; this only
    /// resets the cursors the next page starts from. Coming home, main's flush
    /// cursor is parked at the end and `rehydrate` pulls back a windowful — the
    /// reprint is the page, exactly as a resize reprints one (D27's accepted
    /// duplicate).
    pub(crate) fn switch_to(&mut self, target: Option<ZoomTarget>) {
        let key = match &target {
            Some(target) => target.conv_key(),
            None => ConvKey::Main,
        };
        if let Some(target) = target {
            self.enter_zoom(target);
        } else if self.active.is_main() {
            return;
        } else {
            self.leave_zoom(crate::tui::buffer::BufferId::Hub);
        }
        self.point_at(key);
        if self.active.is_main() {
            // Everything of main's is "already flushed": the rows printed
            // before the visit are banked in scrollback, and what the new
            // page owes the reader is a recent tail, not the whole record.
            self.flushed_segments = self.conv.messages.len() + 1;
        } else {
            self.flushed_segments = 0;
        }
        self.tail_start = 0;
        self.mark_base = 0;
        self.scroll = 0;
        self.auto_scroll = true;
        self.doc = crate::tui::statics::layout(Vec::new());
        self.page_turn = true;
        self.force_redraw = true;
        self.dirty = true;
        if self.active.is_main() {
            // The reprint is the page: pull back a windowful of the recent
            // tail (the resize machinery, D27's accepted duplicate). Measured
            // against the real chrome, never guessed.
            let width = self.width.max(40);
            let budget = self
                .height
                .saturating_sub(2)
                .saturating_sub(crate::tui::chrome::chrome_height_of(self, width, false))
                .max(8);
            self.rehydrate(width, budget);
        }
    }

    /// Park the store on screen and bring `key`'s up, opening it on first sight.
    ///
    /// The page it opens on is claimed, not merely looked up: the claim drains
    /// the channel first, which is what makes the one remaining cold-start walk
    /// safe ([`Chat::claim_conversation`]).
    fn point_at(&mut self, key: ConvKey) {
        if key == self.active {
            return;
        }
        let opened = self.claim_conversation(&key);
        let parked = std::mem::replace(&mut self.conv, opened);
        self.parked
            .insert(std::mem::replace(&mut self.active, key), parked);
    }

    /// Take `key`'s store out of the parking, opening it from the domain's
    /// record when the console has never held one.
    ///
    /// **The channel is drained first, and that is the whole point.** A store
    /// built by [`Chat::open_conversation`] is a *cold* start: it assumes the
    /// console has heard nothing about this conversation and reads its whole
    /// past off the registry. Events already queued say otherwise — a run that
    /// committed its history before the console got round to draining (a short
    /// turn, or a console stalled a tick) is in the walk **and** still in the
    /// channel, and the `TurnStart`/`Inbound`/deltas behind it then replay it
    /// into a store nothing ever rebuilds. The turn renders twice, for good.
    ///
    /// Draining is half the answer and [`Chat::detach`] is the other: a store
    /// opened by an event opens **blank**, so the drain can never walk on the
    /// console's behalf either. What is left here is the only walk that is
    /// safe — the channel is empty, so nothing is waiting to replay what it
    /// reads, and no store means the console has genuinely never heard of the
    /// conversation.
    ///
    /// Only the console's own entry points come through here. `detach` is
    /// inside the drain and must not start another.
    pub(super) fn claim_conversation(&mut self, key: &ConvKey) -> Conversation {
        if self.drain_events() {
            // What `drain_all` does with the same answer: the screen moved, and
            // the stall baseline is measured from the last event that reached
            // the TUI wherever it was consumed.
            self.dirty = true;
            self.last_progress_tick = self.tick;
        }
        let mut conv = match self.parked.remove(key) {
            Some(conv) => conv,
            None => return self.open_conversation(key),
        };
        // A store the event path opened blank still owes its record. The drain
        // above is what makes reading it safe: nothing is left to replay, so the
        // history goes in above whatever arrived — it was committed before any
        // of it.
        if !conv.history_read {
            conv.history_read = true;
            if let ConvKey::Agent(name) = key {
                let session = self.session.clone();
                let past = agent_history(&session, name, &mut |text| self.render_thinking(text));
                if !past.is_empty() {
                    conv.intake_seen = true;
                    let arrived = std::mem::replace(&mut conv.messages, past);
                    conv.messages.extend(arrived);
                }
            }
        }
        conv
    }

    /// A store the console has not kept before, filled from whatever the domain
    /// can tell it about the conversation's past.
    pub(super) fn open_conversation(&mut self, key: &ConvKey) -> Conversation {
        let mut conv = self.blank_conversation();
        conv.history_read = true;
        match key {
            ConvKey::Main => {}
            ConvKey::Agent(name) => {
                let session = self.session.clone();
                conv.messages =
                    agent_history(&session, name, &mut |text| self.render_thinking(text));
                // A record that exists at all opens with the task the instance
                // was dispatched with.
                conv.intake_seen = !conv.messages.is_empty();
            }
            ConvKey::Room(room) => {
                let session = self.session.clone();
                conv.projected = room_log_len(&session, room);
                conv.messages = room_tail(&session, room, 0);
            }
        }
        conv
    }

    /// Keep the open page current: append a room's new posts, and close the page
    /// when its subject is gone (the zoom's own rule — a finished agent stays, a
    /// deleted one cannot).
    ///
    /// An agent's page needs nothing here since D134: its rows arrive on the
    /// event channel like main's, so there is no fingerprint to compare and no
    /// rebuild to trigger. What is left is the room — the one conversation
    /// nothing streams into.
    pub(crate) fn sync_away(&mut self) {
        if self.active.is_main() {
            return;
        }
        if self.away_is_gone() {
            self.switch_to(None);
            return;
        }
        let ConvKey::Room(room) = self.active.clone() else {
            return;
        };
        let session = self.session.clone();
        let len = room_log_len(&session, &room);
        if len == self.conv.projected {
            return;
        }
        let tail = room_tail(&session, &room, self.conv.projected);
        self.conv.projected = len;
        self.conv.messages.extend(tail);
        self.dirty = true;
    }

    /// Whether the page's subject has left the domain (instance removed, room
    /// deleted) — the auto-close rule the zoom carried (CC
    /// `useTeammateViewAutoExit`).
    fn away_is_gone(&self) -> bool {
        match &self.active {
            ConvKey::Agent(name) => !self.session.agents.list().iter().any(|s| &s.name == name),
            ConvKey::Room(name) => !self
                .session
                .channels
                .list()
                .iter()
                .any(|status| &status.name == name),
            ConvKey::Main => false,
        }
    }
}

/// The page header: the conversation's name as a rule, in its identity colour
/// — the one row that says whose page the screen has become.
pub(crate) fn page_header(
    label: &str,
    width: usize,
    color: ratatui::style::Color,
) -> crate::tui::chat::Row {
    let body = format!("── {label} ");
    let fill = width.saturating_sub(crate::tui::line::text_width(&body));
    let text = format!("{body}{}", "─".repeat(fill));
    crate::tui::chat::Row::new(crate::tui::line::Line::styled(
        text,
        crate::tui::line::SegStyle::fg(color),
    ))
}

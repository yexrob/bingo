//! The away page (v6): the screen is another conversation for a while, drawn
//! by the transcript's own pipeline.
//!
//! v4/v5 answered "look at an agent" with the alt-screen zoom — a second
//! renderer over flat [`crate::tui::buffer::Post`] rows, leaving no trace in
//! scrollback. v6's ruling is stronger: an agent's page **is** main's page —
//! same `UiMessage` → `Block` → `Doc` pipeline, same collapse machinery, same
//! composer, same write-once flushing into the terminal's own scrollback. Main
//! is just the page the terminal starts on.
//!
//! **One active page.** [`crate::tui::chat::Chat`]'s `messages`/`doc`/flush
//! cursors always describe *main*; while an away page is open, the doc, the
//! cursors and the scroll are the away page's, and `Chat::messages` is only
//! borrowed by the build (swapped in for the duration of one `build_rows`
//! call, swapped straight back out). Main's own events keep landing in its
//! store the whole time — nothing is rerouted, so nothing can be misfiled.
//!
//! **Switching is a page turn** ([`crate::tui::term::InlineTerm::page_break`]):
//! the page being left banks its printed rows into the terminal's scrollback
//! and the next page starts at the top of a clean screen. Coming home reprints
//! main's recent tail (the resize `rehydrate` machinery, D27's accepted
//! duplicate), because scrollback cannot be un-banked — the D98 ruling.
//!
//! **The page's content is the domain's, rebuilt on change.** An agent page is
//! its record ([`crate::tui::perspective::walk`] — one attribution walk, the
//! same one the pair lane used) plus its queued/claimed messages and the run in
//! flight; a room page is the room's log, **speech only** (the v6 ruling: a room
//! shows what members said to it, nothing else — membership lines included in
//! the log stay out of the page). History-derived messages are deterministic
//! and append-only, so the settled prefix never changes under the flush
//! cursor; everything volatile (queued echoes, the running turn) stays in the
//! redrawable tail by construction.
//!
//! **And one builder for both halves (D132).** The running turn used to arrive
//! as three opaque strings with a renderer of its own, so a page had two ways to
//! draw the same call: the settled half through [`RunBuilder`], with results,
//! folds and status, and the moving half through twenty lines that had none of
//! them. The switch between the two at the end of a run is what read as lag.
//! `LiveBlock` now carries the call rather than a rendering of it, and
//! [`live_message`] runs it through the same builder — so a call looks the same
//! whether it came back a second ago or a session ago.

use std::sync::Arc;

use crate::agents::{LiveBlock, ToolAnswer};
use crate::channels::USER_NAME;
use crate::query::Session;
use crate::tui::activities::{Activity, ActivityKind, EXPAND_HINT, ToolStatus};
use crate::tui::buffer::PostKind;
use crate::tui::chat::{
    Chat, Role, UiMessage, group_ready_tool, result_content, result_summary, skill_result_summary,
};
use crate::tui::line::Line;
use crate::tui::perspective::{Filed, Protagonist, Target, Work, walk};
use crate::tui::zoom::ZoomTarget;

/// What one away build differs on from a home build, threaded through the
/// shared core as transient state: the page's header, its settled boundary,
/// and the streaming run's blocks.
#[derive(Clone, Default)]
pub(crate) struct AwayBuild {
    /// `@scout` / `#room` — the header rule and the identity colour key.
    pub label: String,
    /// Messages below this index are settled (committed history); at and
    /// above, volatile.
    pub stable: usize,
}

/// The open away page: which conversation, its rebuilt messages, and the
/// fingerprint of the domain state they were built from.
pub(crate) struct AwayPage {
    pub target: ZoomTarget,
    /// The page's transcript, in the console's own vocabulary. The prefix
    /// derived from committed history is stable across rebuilds; the tail
    /// (queued messages, the streaming run) is volatile and never settles.
    pub messages: Vec<UiMessage>,
    /// How many leading messages are settled (committed history / room log):
    /// only these may flush into scrollback.
    pub stable: usize,
    /// Domain fingerprint of the last build; a change rebuilds the page.
    print: u64,
}

/// What the away page saw last time it looked at the domain. Cheap to compute,
/// stable while nothing happens, guaranteed to move when anything the page
/// shows moves.
fn fingerprint(session: &Arc<Session>, target: &ZoomTarget) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match target {
        ZoomTarget::Agent(name) => {
            if let Some((history, _stamps, live, in_flight, state)) = session.agents.view_of(name) {
                history.len().hash(&mut h);
                for block in &live {
                    match block {
                        LiveBlock::Text(t) | LiveBlock::Thinking(t) => t.len().hash(&mut h),
                        // A call moves twice — when it starts and when it comes
                        // back — and both have to move the print, or a result
                        // would land on a page that never rebuilds to show it.
                        LiveBlock::Tool(call) => {
                            call.id.hash(&mut h);
                            call.answer.is_some().hash(&mut h);
                        }
                    }
                }
                in_flight.len().hash(&mut h);
                std::mem::discriminant(&state).hash(&mut h);
            }
            session.agents.pending_of(name).len().hash(&mut h);
        }
        ZoomTarget::Room(name) => {
            session.channels.log_of(name).len().hash(&mut h);
        }
    }
    h.finish()
}

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
fn counterpart_message(from: &str, at: u64, text: String) -> UiMessage {
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

/// An agent's record → the page's messages, plus how many of them are stable.
///
/// The stable half is a pure function of `history` (append-only), which is
/// what lets the flush cursor trust it; queued and claimed messages follow as
/// volatile echoes (the domain's own record of "you sent this", so no local
/// copy can drift from it), and the streaming run rides separately as the
/// live tail block.
fn agent_messages(
    session: &Arc<Session>,
    name: &str,
    render: &mut dyn FnMut(&str) -> Vec<Line>,
) -> (Vec<UiMessage>, usize) {
    let Some((history, stamps, live, in_flight, _state)) = session.agents.view_of(name) else {
        return (Vec::new(), 0);
    };
    let mut out: Vec<UiMessage> = Vec::new();
    let mut run: Option<RunBuilder> = None;
    for filed in walk(Protagonist::of(name), &history, &stamps) {
        let Filed { target, post, work } = filed;
        let own = post.from == name && !matches!(target, Target::Intake);
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
    let stable = out.len();
    for (from, text) in &in_flight {
        out.push(counterpart_message(from, 0, text.clone()));
    }
    for (from, text) in session.agents.pending_of(name) {
        out.push(counterpart_message(&from, 0, text));
    }
    // The run in flight, built by the same builder as every run above it (D132)
    // and appended past the settled boundary, so it is volatile by construction
    // — which is the whole reason the tail used to be drawn separately.
    if let Some(running) = live_message(name, &live, render) {
        out.push(running);
    }
    (out, stable)
}

/// The streaming run as a message.
///
/// This is the fix D132 is: the tail used to be three opaque strings and a
/// twenty-line renderer of its own, so the moving half of a page had no result
/// rows, no folds and no status while the settled half had all three — and the
/// switch between them at the end of a run is what read as lag. It is the same
/// builder now, so a call renders the same whether it came back a second ago or
/// a session ago.
fn live_message(
    name: &str,
    live: &[LiveBlock],
    render: &mut dyn FnMut(&str) -> Vec<Line>,
) -> Option<UiMessage> {
    if live.is_empty() {
        return None;
    }
    let mut builder = RunBuilder::new(name, 0);
    for block in live {
        match block {
            LiveBlock::Text(text) => {
                if !text.trim().is_empty() {
                    builder.prose(text);
                }
            }
            LiveBlock::Thinking(text) => builder.thinking(text, render),
            // A call with no answer yet is *running*, not interrupted — the one
            // place the two callers of `tool` mean opposite things by silence.
            LiveBlock::Tool(call) => builder.tool(
                &call.name,
                &call.input,
                call.answer.as_ref(),
                ToolStatus::Running,
            ),
        }
    }
    builder.finish()
}

/// A room's log → the page's messages: **speech only** (the v6 ruling), each
/// post wearing its sender. The user's own posts keep their bubble.
fn room_messages(session: &Arc<Session>, room: &str) -> (Vec<UiMessage>, usize) {
    let log = session.channels.log_of(room);
    let out: Vec<UiMessage> = log
        .iter()
        .filter(|m| m.kind == crate::channels::MessageKind::Said)
        .map(|m| counterpart_message(&m.from, m.at, m.text.clone()))
        .collect();
    let stable = out.len();
    (out, stable)
}

impl AwayPage {
    /// `chat` is borrowed for one thing only: the markdown pipeline that turns
    /// an agent's reasoning into rows. It is the console's own — a second
    /// renderer would be a second way for the same text to look different, which
    /// is the whole class of bug D130 closes.
    fn build(session: &Arc<Session>, target: ZoomTarget, chat: &mut Chat) -> Self {
        let print = fingerprint(session, &target);
        let (messages, stable) = match &target {
            ZoomTarget::Agent(name) => {
                agent_messages(session, name, &mut |text| chat.render_thinking(text))
            }
            ZoomTarget::Room(name) => room_messages(session, name),
        };
        Self {
            target,
            messages,
            stable,
            print,
        }
    }
}

impl Chat {
    /// Point the terminal at another conversation — or home (`None`).
    ///
    /// The page being left is finished into scrollback by the host's page
    /// break; this only resets the cursors the next page starts from. Coming
    /// home, main's flush cursor is parked at the end and `rehydrate` pulls
    /// back a windowful — the reprint is the page, exactly as a resize
    /// reprints one (D27's accepted duplicate).
    pub(crate) fn switch_to(&mut self, target: Option<ZoomTarget>) {
        match target {
            Some(target) => {
                self.enter_zoom(target.clone());
                let session = self.session.clone();
                self.away = Some(AwayPage::build(&session, target, self));
                self.flushed_segments = 0;
            }
            None => {
                if self.away.take().is_none() {
                    return;
                }
                self.leave_zoom(crate::tui::buffer::BufferId::Hub);
                // Everything of main's is "already flushed": the rows printed
                // before the visit are banked in scrollback, and what the new
                // page owes the reader is a recent tail, not the whole record.
                self.flushed_segments = self.messages.len() + 1;
            }
        }
        self.tail_start = 0;
        self.mark_base = 0;
        self.scroll = 0;
        self.auto_scroll = true;
        self.doc = crate::tui::statics::layout(Vec::new());
        self.bash_mode = false;
        self.page_turn = true;
        self.force_redraw = true;
        self.dirty = true;
        if self.away.is_none() {
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

    /// Keep the open page current: rebuild when the domain moved, close it
    /// when its subject is gone (the zoom's own rule — a finished agent stays,
    /// a deleted one cannot).
    pub(crate) fn sync_away(&mut self) {
        if self.away.is_none() {
            return;
        }
        if self.away_is_gone() {
            self.switch_to(None);
            return;
        }
        let Some(away) = &self.away else { return };
        let print = fingerprint(&self.session, &away.target);
        if print == away.print {
            return;
        }
        let target = away.target.clone();
        let session = self.session.clone();
        let rebuilt = AwayPage::build(&session, target, self);
        if let Some(away) = &mut self.away {
            *away = rebuilt;
        }
        self.dirty = true;
    }

    /// Whether the page's subject has left the domain (instance removed, room
    /// deleted) — the auto-close rule the zoom carried (CC
    /// `useTeammateViewAutoExit`).
    fn away_is_gone(&self) -> bool {
        match self.away.as_ref().map(|a| &a.target) {
            Some(ZoomTarget::Agent(name)) => {
                !self.session.agents.list().iter().any(|s| &s.name == name)
            }
            Some(ZoomTarget::Room(name)) => !self
                .session
                .channels
                .list()
                .iter()
                .any(|status| &status.name == name),
            None => false,
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

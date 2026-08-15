//! The host side of the conversation engine (D89): one terminal, one flow, one
//! active conversation.
//!
//! [`crate::tui::buffer`] knows what a conversation *is*. This module is what
//! the two hosts do with it, and it is built on one ruling: switching to a
//! conversation stashes the draft you were writing, prints a divider and that
//! conversation's recent history into the flow, and makes it the only
//! conversation whose new activity reaches the screen. Everything else keeps
//! accumulating in the domain store it already lives in and bumps an unread
//! count. Nothing buffers *rows* for a conversation you are not in.
//!
//! **The flow is a projection, not the message list.** `Chat::messages` is one
//! append-only store holding both main's transcript and the rows every
//! excursion has printed. [`Chat::flow_order`] decides what that store looks
//! like on screen: home messages up to the point where you left, then the
//! excursion's rows, and — while the excursion is still open — nothing else, so
//! a main turn that lands while you are reading a DM does not print into the DM.
//! Coming back closes the excursion with a `── @main ──` rule, and main's
//! unprinted tail follows it.
//!
//! Two properties fall out of that shape, and both are why it has this shape:
//!
//! - **The order is append-only.** Once a position in the flow is printed it
//!   never moves, so the write-once flush cursor
//!   (`Chat::flushed_segments`) stays valid and scrollback is never rewritten.
//! - **There is no second renderer.** An excursion's rows are `UiMessage`s in
//!   the same list main's are, so `build_rows`/`assistant_el` render a
//!   replayed DM message with the code that renders a live reply from main — the
//!   markdown, the bubbles, the stamps and the CJK wrapping are the same by
//!   construction rather than by imitation.
//!
//! Scrollback can therefore hold the same conversation twice after a couple of
//! excursions. That is accepted and marked: every segment opens with a rule
//! naming the conversation, and `ctrl+o` (D82) remains the complete authority.

use rsmarkdown_core::{MarkdownProcessor, Renderer};

use crate::channels::USER_NAME;
use crate::tui::buffer::{BufferId, Delivery, Post, PostKind, Replay, dm_posts};
use crate::tui::chat::{Chat, Role, Row, UiMessage, one_line, text_rows, user_message_rows};
use crate::tui::complete::ArgCandidate;
use crate::tui::el::El;
use crate::tui::line::{Line, SegStyle, wrap_words};
use crate::tui::markdown::MarkdownRenderer;

/// The receipt a routed submit leaves in the @main flow: `→ @scout: look at…`.
///
/// Display-only, like the dialog receipts (D80/D81): the model's history never
/// carries it, because nothing was said to the model. It exists so a line that
/// left @main is not simply gone — without it the composer would clear and
/// the flow would show nothing at all, which is indistinguishable from a
/// message that was dropped.
pub const ROUTE_RECEIPT_PREFIX: &str = "→ ";

/// How much of the delivered text the receipt echoes. Enough to recognize
/// which message it was, not enough to reprint it into a flow it never
/// belonged to.
const RECEIPT_CHARS: usize = 40;

/// Whether a line is a delivery receipt. Matched by the prefix *and* the sigil
/// that has to follow it, so a line of prose that happens to open with an arrow
/// is not mistaken for one.
pub(crate) fn is_route_receipt(text: &str) -> bool {
    text.strip_prefix(ROUTE_RECEIPT_PREFIX)
        .is_some_and(|rest| rest.starts_with('@') || rest.starts_with('#'))
}

/// The one line an agent's life still writes into the @main flow (D98):
/// `⚠ @scout · subagent failed: connection reset`.
///
/// Everything else about a run — spawn, progress, completion, cancellation —
/// reaches the user through the dispatch row's own state and through whatever
/// the main agent then says. A failure cannot depend on that narration: the
/// turn that would have narrated it may never run. So bad news, and only bad
/// news, comes straight through.
pub(crate) const AGENT_ALERT_PREFIX: &str = "⚠ @";

pub(crate) fn is_agent_alert(text: &str) -> bool {
    text.starts_with(AGENT_ALERT_PREFIX)
}

/// The alert line for one failed run: who, and one line of why.
pub(crate) fn agent_alert_line(instance: &str, reason: Option<&str>) -> String {
    match reason.map(str::trim).filter(|r| !r.is_empty()) {
        Some(reason) => format!("{AGENT_ALERT_PREFIX}{instance} · {reason}"),
        None => format!("{AGENT_ALERT_PREFIX}{instance} · failed"),
    }
}

/// `→ @scout: look at the parser` — one line, whitespace flattened, cut to
/// [`RECEIPT_CHARS`].
fn receipt_line(id: &BufferId, text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let excerpt = if flat.chars().count() > RECEIPT_CHARS {
        format!("{}…", flat.chars().take(RECEIPT_CHARS).collect::<String>())
    } else {
        flat
    };
    format!("{ROUTE_RECEIPT_PREFIX}{}: {excerpt}", id.label())
}

/// How many messages a switch replays. Messages, not rows: rows exist only
/// after a layout at a known width, and a conversation's last eight messages is
/// a promise that can be kept at any width.
///
/// Eight, not thirty (D99). Thirty was sized when a replay was the only way back
/// into a conversation; it is not any more — the record keeps the whole thing
/// (`ctrl+o`, the observation page) and the flow keeps the scrollback of every
/// visit. What a switch owes the reader is the thread of the last exchange, and
/// a screenful of somebody else's history above the composer is a wall, not a
/// welcome.
pub const REPLAY_BUDGET: usize = 8;

/// A replay minus its opening rule: everything that occupies a position in the
/// conversation, in order. The rule is furniture the host prints once, so it is
/// the one element that must not be counted — `seen` is a cursor into *this*
/// list, and a divider inside it would put the poll one item out of step.
fn replay_items(all: &[Replay]) -> Vec<Replay> {
    all.iter()
        .filter(|replay| !matches!(replay, Replay::Divider(_)))
        .cloned()
        .collect()
}

/// What a flow position is, beyond the home conversation's two roles.
///
/// `@main`'s transcript has exactly two speakers and needs no decoration. A DM
/// or a channel has a name over each message and a rule where it begins, and
/// those are decorations of a *position in the flow* rather than facts about
/// the message — the same `UiMessage` renders undecorated in `@main`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decor {
    /// The home conversation's own message: the user and main, rendered as
    /// always.
    Home,
    /// A rule: the one that opens a conversation, or the one that hands the
    /// flow back home. The message text is the rule.
    Divider,
    /// Spoken by this name, in a DM or a channel.
    Said(String),
}

/// One position in the printed flow: which message, and how it is decorated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowItem {
    /// Index into `Chat::messages`.
    pub index: usize,
    pub decor: Decor,
}

impl FlowItem {
    fn home(index: usize) -> Self {
        Self {
            index,
            decor: Decor::Home,
        }
    }
}

/// One visit to a conversation other than `@main`.
///
/// An excursion is a *segment* of the flow: the rows this conversation printed,
/// spliced in at the home-message index the switch happened at. It is not a
/// copy of the conversation — the rows are indices into the one message store,
/// and the conversation itself stays in its domain store the whole time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excursion {
    pub id: BufferId,
    /// How much of `@main` had been printed when this excursion opened. Home
    /// messages past it wait here until the excursion closes.
    pub at: usize,
    /// The rows this conversation has put on screen, in print order: the
    /// opening rule, the replay, whatever arrived while it was active, and —
    /// once it closes — the `── @main ──` rule that ends it.
    pub rows: Vec<FlowItem>,
    /// Messages the source had already produced when it was last printed, so
    /// the poll appends only what is new. Counted in posts, which is the unit
    /// the replay is built in.
    pub seen: usize,
    /// The flow has been handed back to `@main`.
    pub closed: bool,
}

impl Chat {
    // -- the flow ----------------------------------------------------------

    /// A digest turn that ended in the acknowledgement marker (D102).
    ///
    /// Both halves of the question are asked here, because either alone answers
    /// the wrong one: the turn has to have been a **digest** — nobody typed into
    /// it, so nobody is owed a reply — and the reply has to be the marker and
    /// nothing else. The same marker at the end of a turn the user started is a
    /// misfire, and a misfire the renderer swallows is a bug the user cannot
    /// see; the same turn ending in prose is main speaking, and that is exactly
    /// what `@main` is for.
    ///
    /// Nothing that answers true here ever reaches scrollback. It cannot: the
    /// only message that can be quiet is the one the stream is writing into,
    /// `message_static_settled` refuses to settle that message while
    /// `stream_msg` points at it, and nothing flushes until it settles — so the
    /// answer is already final by the time the flush cursor could have reached
    /// it.
    pub(crate) fn is_quiet(&self, i: usize) -> bool {
        self.messages
            .get(i)
            .is_some_and(|m| m.digest && m.text.trim() == crate::query::QUIET_MARKER)
    }

    /// The print order of [`Chat::messages`].
    ///
    /// Home messages and excursion rows share one store, and this is the single
    /// answer to what that store looks like on screen. Walking it is linear in
    /// the store, runs once per build, and is append-only across builds: an
    /// item that has been emitted keeps its position for the rest of the
    /// session, which is what the write-once flush cursor rests on.
    ///
    /// The one message that never takes a position is a digest turn's silent
    /// acknowledgement ([`Chat::is_quiet`]). Append-only is unharmed: the only
    /// message that can answer to it is the one the stream is still writing, so
    /// it can never have flushed, and the positions it would have shifted are
    /// all above the cursor.
    pub(crate) fn flow_order(&self) -> Vec<FlowItem> {
        if self.excursions.is_empty() {
            return (0..self.messages.len())
                .filter(|&i| !self.is_quiet(i))
                .map(FlowItem::home)
                .collect();
        }
        // Which indices belong to an excursion rather than to main. Main
        // is "everything nobody claimed", so no message needs to carry a flag
        // saying which conversation printed it.
        let mut claimed = vec![false; self.messages.len()];
        for exc in &self.excursions {
            for item in &exc.rows {
                if let Some(slot) = claimed.get_mut(item.index) {
                    *slot = true;
                }
            }
        }
        let mut out: Vec<FlowItem> = Vec::with_capacity(self.messages.len());
        let mut cursor = 0usize;
        let push_main_upto = |upto: usize, cursor: &mut usize, out: &mut Vec<FlowItem>| {
            while *cursor < upto {
                if !claimed[*cursor] && !self.is_quiet(*cursor) {
                    out.push(FlowItem::home(*cursor));
                }
                *cursor += 1;
            }
        };
        for exc in &self.excursions {
            push_main_upto(exc.at.min(self.messages.len()), &mut cursor, &mut out);
            out.extend(exc.rows.iter().cloned());
            // An open excursion holds main's tail: the messages a running
            // turn lands while you are away are main's news, and printing
            // them here would interleave two conversations in one flow.
            if !exc.closed {
                return out;
            }
        }
        push_main_upto(self.messages.len(), &mut cursor, &mut out);
        out
    }

    /// The conversation the composer and the flow belong to.
    pub(crate) fn active_buffer(&self) -> BufferId {
        self.buffers.active().clone()
    }

    /// The excursion currently open, if the active conversation is not main.
    fn open_excursion(&mut self) -> Option<&mut Excursion> {
        self.excursions.last_mut().filter(|exc| !exc.closed)
    }

    // -- switching ---------------------------------------------------------

    /// Point the terminal at another conversation.
    ///
    /// The whole switch, in the order the ruling gives it: the draft you were
    /// writing stays behind and this conversation's comes back, a rule and the
    /// recent history go into the flow, and from here only this conversation's
    /// new activity prints. Switching to the conversation you are already in is
    /// nothing at all — not a redundant replay of what is already on screen.
    pub(crate) fn switch_to(&mut self, id: BufferId) {
        let from = self.active_buffer();
        if from == id {
            return;
        }
        // The draft is the composer's, and the composer belongs to whichever
        // conversation is active. Both halves happen here so a switch can never
        // drop what was typed.
        self.buffers
            .stash_draft(&from, std::mem::take(&mut self.input));
        let draft = self.buffers.take_draft(&id);
        self.set_input(draft);
        self.cursor = self.input.len();
        // Composer modes that mean something to main and nothing to a
        // conversation: `!` runs a command, and the completion surfaces are
        // about the line that was just abandoned.
        self.bash_mode = false;
        self.clear_slash_suggestions();
        self.mention = None;
        self.mention_dismissed = false;

        // Leaving a conversation closes its segment with the rule that hands
        // the flow back; main's own tail follows it, unprinted until now.
        if from != BufferId::Hub {
            let divider = self.push_flow_divider(BufferId::Hub.rule());
            if let Some(exc) = self.open_excursion() {
                exc.rows.push(divider);
                exc.closed = true;
            }
        }
        self.buffers.set_active(id.clone());
        if id != BufferId::Hub {
            self.open_conversation(&id);
        }
        // A switch lands you at the tail, the way opening a chat anywhere does
        // (D93). The rule and the replay print at the end of the document, so a
        // viewer who had scrolled up would otherwise be looking at the old
        // conversation with the new one somewhere below the fold. Re-arming the
        // stick rather than writing an offset leaves the arithmetic to
        // `reconcile_scroll`, which is the only thing that knows the viewport.
        self.auto_scroll = true;
        self.dirty = true;
    }

    /// `tab` on an empty composer: open the record of the conversation you are
    /// in (D100).
    ///
    /// A conversation has a protagonist and its record is that participant's
    /// observation page — the agent in `@agent`, main in the console. A `#room`
    /// has no single protagonist, so the key means nothing there and is left
    /// unconsumed rather than given a surprising second meaning.
    ///
    /// Inert behind a permission ask, for the switcher's and the directory's
    /// reason (D81): a surface that takes the whole screen must not open over a
    /// question that is holding up a turn.
    ///
    /// Returns whether the key was consumed.
    pub(crate) fn open_conversation_record(&mut self) -> bool {
        if self.pending_ask.is_some() {
            return false;
        }
        let who = match self.active_buffer() {
            BufferId::Hub => crate::channels::MAIN_NAME.to_string(),
            BufferId::Dm(name) => name,
            BufferId::Channel(_) => return false,
        };
        self.open_perspective = Some(who);
        self.dirty = true;
        true
    }

    /// Open a segment for a conversation: the rule, then its recent history.
    fn open_conversation(&mut self, id: &BufferId) {
        let session = self.session.clone();
        // Everything the source has, so `seen` is the true total and the poll
        // that follows appends only what arrives after this moment; the budget
        // decides how much of it goes on screen, not how much is counted.
        let all = self.buffers.rehydrate(&session, id, usize::MAX);
        let items: Vec<Replay> = replay_items(&all);
        let seen = items.len();
        let start = seen.saturating_sub(REPLAY_BUDGET);
        let shown = items[start..].to_vec();

        // Where main had got to before any of this was appended: the point
        // its unprinted tail resumes from when the excursion closes.
        let at = self.messages.len();
        // The rule comes from the replay rather than being formatted again
        // here: one shape, decided in one place — including the observer
        // framing, which is a fact about the conversation rather than about
        // the host that opened it.
        let rule = all
            .iter()
            .find_map(|replay| match replay {
                Replay::Divider(text) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_else(|| id.rule());
        let mut rows = vec![self.push_flow_divider(rule)];
        if let Some(note) = self.empty_pair_note(id, &items) {
            rows.push(self.push_flow_divider(note));
        }
        for replay in shown {
            rows.extend(self.push_replay(&replay));
        }
        self.excursions.push(Excursion {
            id: id.clone(),
            at,
            rows,
            seen,
            closed: false,
        });
    }

    /// The one line an empty `@agent` opens with, or `None` when it has earned
    /// none (D100).
    ///
    /// D99's honest consequence is that an agent main spawned and the user never
    /// wrote to has an *empty* pair view: its first message is the task (intake)
    /// and its report answers main. An empty conversation under a rule reads as
    /// a bug; the record is where that agent's life actually is, so the note
    /// says both — nothing here yet, and the door.
    ///
    /// It is furniture rather than replay: it takes the rule's row shape and is
    /// not one of the `seen` items, so the first real message still appends
    /// past the cursor instead of being counted as already printed.
    ///
    /// **Not repeated.** Switching out and back prints the rules again, and a
    /// note that came with them would stack up. The flow itself is the state
    /// that answers whether it is needed: if the last thing anybody printed —
    /// looking past the rules a round trip leaves — is this note, it is still on
    /// screen and is not printed twice.
    fn empty_pair_note(&self, id: &BufferId, items: &[Replay]) -> Option<String> {
        let BufferId::Dm(name) = id else {
            return None;
        };
        if !items.is_empty() {
            return None;
        }
        let note = format!("· no conversation yet · tab opens @{name}'s record ·");
        let shown = self
            .messages
            .iter()
            .rev()
            .find(|message| !(message.text.starts_with("── ") && message.text.ends_with(" ──")))
            .is_some_and(|message| message.text == note);
        (!shown).then_some(note)
    }

    /// One replayed element into the flow. A message keeps its sender; a note
    /// is nobody's, so it takes the rule's row shape — one dim line, no name
    /// and no stamp — which is what a `· scout joined ·` line has to be.
    fn push_replay(&mut self, replay: &Replay) -> Option<FlowItem> {
        match replay {
            Replay::Message { who, message } => {
                Some(self.push_flow_message(who.clone(), message.clone()))
            }
            Replay::Note(text) => Some(self.push_flow_divider(text.clone())),
            Replay::Divider(_) => None,
        }
    }

    /// Append the rule that opens (or closes) a conversation and return its
    /// flow position.
    fn push_flow_divider(&mut self, text: String) -> FlowItem {
        let index = self.messages.len();
        self.messages.push(UiMessage {
            role: Role::Assistant,
            text,
            at: 0,
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
            digest: false,
        });
        FlowItem {
            index,
            decor: Decor::Divider,
        }
    }

    /// Append one conversation message and return its flow position.
    fn push_flow_message(&mut self, who: String, message: UiMessage) -> FlowItem {
        let index = self.messages.len();
        self.messages.push(message);
        FlowItem {
            index,
            decor: Decor::Said(who),
        }
    }

    // -- living in a conversation -----------------------------------------

    /// Print whatever the active conversation has produced since it was last
    /// looked at. Called every tick while a conversation is open, because the
    /// alternative — a message that waits up to fifteen frames to appear —
    /// is the thing the workspace never did.
    pub(crate) fn poll_active_conversation(&mut self) {
        let id = self.active_buffer();
        if id == BufferId::Hub || self.open_excursion().is_none() {
            return;
        }
        let session = self.session.clone();
        let all = self.buffers.rehydrate(&session, &id, usize::MAX);
        let fresh: Vec<Replay> = replay_items(&all);
        let seen = self.open_excursion().map(|exc| exc.seen).unwrap_or(0);
        if fresh.len() <= seen {
            return;
        }
        let mut rows = Vec::new();
        for replay in fresh.into_iter().skip(seen) {
            rows.extend(self.push_replay(&replay));
        }
        let total = seen + rows.len();
        if let Some(exc) = self.open_excursion() {
            exc.rows.extend(rows);
            exc.seen = total;
        }
        self.dirty = true;
    }

    // -- speaking ----------------------------------------------------------

    /// Send the composer's text to the active conversation.
    ///
    /// Never starts a main turn: `busy` belongs to the model conversation, and a
    /// message to a subagent is a delivery, not a turn. The echo is immediate
    /// in both directions — a channel post lands in the log and the poll picks
    /// it up, a DM lands in the instance's inbox and the live tail shows it
    /// before the agent has said anything back.
    pub(crate) fn send_to_active(&mut self, text: String) {
        let id = self.active_buffer();
        let target = self.buffers.route_submit(&self.session, &id, &text);
        match crate::tui::buffer::deliver(&self.session, target) {
            Delivery::Sent => {
                self.poll_active_conversation();
                self.dirty = true;
            }
            // An observed room's refusal and a delivery failure read the same
            // way: information about what did not happen, above the composer.
            Delivery::Rejected(why) => self.push_slash_info(why),
            // Unreachable — only main routes to a turn, and main does not
            // come through here — but a silent drop would be the worse answer.
            Delivery::Turn(text) => self.start_turn(text, true),
        }
    }

    // -- the live tail -----------------------------------------------------

    /// What the active DM is doing right now: the message on its way, the work
    /// it is doing, the reply as it arrives.
    ///
    /// These rows are transient by construction. Everything here is a *state* —
    /// claimed, queued, mid-stream — and the moment any of it becomes record it
    /// arrives through [`Chat::poll_active_conversation`] as a settled message
    /// and disappears from here. That is why they never reach scrollback: the
    /// record is what gets printed, not the states on the way to it.
    pub(crate) fn conversation_tail_el(
        &self,
        width: usize,
        pal: &crate::tui::avatar::Palette,
    ) -> Option<El> {
        let BufferId::Dm(name) = self.active_buffer() else {
            return None;
        };
        let (history, stamps, live, in_flight, pending) = self.dm_state(&name)?;
        // The settled prefix is what the flow already shows; `dm_posts` appends
        // the live states after it, so the difference is exactly the tail.
        let settled = dm_posts(&history, &stamps, &[], &[], &[], &name);
        let all = dm_posts(&history, &stamps, &in_flight, &live, &pending, &name);
        if all.len() <= settled.len() {
            return None;
        }
        let gutter = self.conversation_gutter(pal);
        // Run tracking starts fresh at the tail: everything above it has
        // already settled into the flow, and reaching back across that seam
        // would mean re-deciding rows that are frozen.
        let tail = &all[settled.len()..];
        let runs = sender_runs(tail);
        let rows: Vec<Row> = tail
            .iter()
            .zip(runs)
            .flat_map(|(post, lead)| self.tail_post_rows(post, &name, width, Some(&gutter), lead))
            .collect();
        if rows.is_empty() {
            return None;
        }
        Some(El::Rows(rows))
    }

    /// The avatar gutter this view draws.
    ///
    /// Every conversation has one since D99, @main included: main is a
    /// participant like the rest, and a face is how a participant is
    /// recognised. One value, so the flow, the live tail and the perspective
    /// page cannot drift on width, on who wears what, or on which skin the
    /// terminal is in.
    pub(crate) fn conversation_gutter<'a>(
        &'a self,
        pal: &'a crate::tui::avatar::Palette,
    ) -> crate::tui::avatar::Gutter<'a> {
        crate::tui::avatar::Gutter::new(self.image_cap.is_some(), pal, &self.faces_pinned)
    }

    /// The instance's live state *for the pair view*, or `None` when the
    /// registry has never heard of it (a DM whose agent was deleted still has a
    /// conversation to read).
    ///
    /// Two filters, both D99, both about the same thing — this conversation has
    /// two participants in it:
    ///
    /// - **The messages in flight and queued are the user's own.** An
    ///   instruction main sent, sitting in the same inbox, is not a bubble the
    ///   user wrote and must not be drawn as one.
    /// - **The live tail belongs to the run the user started.** A run triggered
    ///   by main, by a room or by a chase is that agent working for somebody
    ///   else; its stream is on its own page, and here it is not even a typing
    ///   row — the indicator would be a promise of a reply nobody asked for.
    #[allow(clippy::type_complexity)]
    fn dm_state(
        &self,
        name: &str,
    ) -> Option<(
        Vec<crate::api::types::Message>,
        Vec<u64>,
        Vec<crate::agents::LiveBlock>,
        Vec<String>,
        Vec<String>,
    )> {
        let (history, stamps, live, in_flight, _state) = self.session.agents.view_of(name)?;
        let mine = |(from, text): (String, String)| (from == USER_NAME).then_some(text);
        let in_flight: Vec<String> = in_flight.into_iter().filter_map(mine).collect();
        let pending: Vec<String> = self
            .session
            .agents
            .pending_of(name)
            .into_iter()
            .filter_map(mine)
            .collect();
        let live = if self.session.agents.run_is_the_users(name) {
            live
        } else {
            Vec::new()
        };
        Some((history, stamps, live, in_flight, pending))
    }

    /// One live post as rows. The vocabulary is the transcript's own: a message
    /// you sent is your bubble, a step of the agent's work is one dim line, and
    /// the wait is the same spinner the rest of the app waits with (D87
    /// `pulse`), so a DM in flight and a main turn in flight read alike.
    fn tail_post_rows(
        &self,
        post: &Post,
        who: &str,
        width: usize,
        gutter: Option<&crate::tui::avatar::Gutter<'_>>,
        lead: bool,
    ) -> Vec<Row> {
        let theme = &self.theme;
        // The two live-only kinds are states, not messages: they get the
        // indentation so the column does not jog, and no face, because nobody
        // has said anything yet.
        let indent = |rows: &mut Vec<Row>| {
            if let Some(g) = gutter {
                g.apply(rows, g.index_for(who), who, false);
            }
        };
        let inner = match gutter {
            Some(g) => width.saturating_sub(g.width()),
            None => width,
        };
        match post.kind {
            // The bare indicator: a reply is owed and nothing has arrived yet.
            // With text it *is* the stream, and renders as the reply it is
            // becoming.
            PostKind::Typing if post.text.trim().is_empty() => {
                let glyph = self.motion.pulse(self.tick);
                // The instance's live output rate, where the workspace's DM
                // composer used to carry it: the same fact at the same moment,
                // on the row that already says the agent owes a reply.
                let rate = self
                    .session
                    .agents
                    .token_rate_label(who, std::time::Instant::now(), self.motion.off())
                    .map(|rate| format!(" · {rate}"))
                    .unwrap_or_default();
                // The wait wears the same colour the composer does (D90), so
                // the spinner, the prompt and the teammate's name in the bar
                // are one teammate rather than three unrelated accents.
                let mut rows = vec![Row::new(Line::styled(
                    one_line(&format!("{glyph} {who} is replying…{rate}"), inner),
                    SegStyle::fg(self.teammate_tint().unwrap_or(theme.claude)),
                ))];
                indent(&mut rows);
                rows
            }
            // Sent, not yet claimed by a run: the agent has it, the turn has
            // not started. Dim, so it reads as in transit rather than answered.
            PostKind::Queued => {
                let mut rows: Vec<Row> = wrap_words(&post.text, inner)
                    .into_iter()
                    .map(|line| Row::new(Line::styled(line, SegStyle::fg(theme.text_secondary))))
                    .collect();
                indent(&mut rows);
                rows
            }
            _ => {
                let sender = gutter.map(|g| Sender {
                    gutter: *g,
                    index: g.index_for(&post.from),
                    lead,
                });
                settled_post_rows(theme, post, width, sender.as_ref())
            }
        }
    }

    // -- /open -------------------------------------------------------------

    // -- line-leading routing ---------------------------------------------

    /// A main submit that opens with another conversation's name.
    ///
    /// `@scout look at the parser` delivers `look at the parser` to scout and
    /// leaves the flow where it is: the point is to say one thing to a teammate
    /// *without* the cost of going there and coming back, which is the whole
    /// difference between this and `ctrl+k`.
    ///
    /// **Only from main.** In a DM or a channel the buffer already *is* the
    /// target, so a leading `@name` there is what it looks like — a person
    /// being addressed inside a message — and treating it as an envelope would
    /// silently redirect a sentence the user meant to send where they were.
    /// The asymmetry is deliberate and it is the reason this is not a general
    /// composer feature.
    ///
    /// **Names resolve exactly.** The sigil is required and the name is matched
    /// case-sensitively against the registry, so `@unknown hi` is not an error
    /// and not magic — it is prose, and it submits to main verbatim. D85's
    /// completion offers the names that do resolve, which is where discovery
    /// belongs.
    pub(crate) fn leading_route(&self, text: &str) -> Option<(BufferId, String)> {
        if *self.buffers.active() != BufferId::Hub {
            return None;
        }
        let (head, rest) = text.split_once(' ')?;
        let rest = rest.trim();
        if rest.is_empty() {
            return None;
        }
        let sigil = head.chars().next()?;
        let name = &head[sigil.len_utf8()..];
        if name.is_empty() {
            return None;
        }
        let id = match sigil {
            '@' => BufferId::Dm(name.to_string()),
            '#' => BufferId::Channel(name.to_string()),
            _ => return None,
        };
        self.buffers.get(&id)?;
        Some((id, rest.to_string()))
    }

    /// Deliver a routed submit and leave the receipt for it.
    ///
    /// The delivery is [`crate::tui::buffer::deliver`], the same call a submit
    /// made from inside that conversation performs — one path, so a message
    /// routed from main is indistinguishable at the domain from one typed
    /// in the DM itself.
    pub(crate) fn route_from_main(&mut self, id: BufferId, text: String) {
        let target = self.buffers.route_submit(&self.session, &id, &text);
        match crate::tui::buffer::deliver(&self.session, target) {
            Delivery::Sent => {
                let receipt = receipt_line(&id, &text);
                self.messages.push(UiMessage {
                    role: Role::User,
                    text: receipt,
                    at: crate::channels::now_unix(),
                    activities: Vec::new(),
                    insert_points: Vec::new(),
                    groups: Vec::new(),
                    group_of: Vec::new(),
                    digest: false,
                });
                self.dirty = true;
            }
            // The board's refusal and a failed delivery both say what did not
            // happen, above the composer — never as a receipt, which would
            // claim something was delivered.
            Delivery::Rejected(why) => self.push_slash_info(why),
            // Unreachable: a leading name always carries a sigil, and main
            // has none — but a silent drop would be the worse answer.
            Delivery::Turn(text) => self.start_turn(text, true),
        }
    }

    /// `/open <target>`: a conversation by name.
    ///
    /// It stays beside `ctrl+k` (D90) rather than being replaced by it: the
    /// switcher is recognition and this is recall, it is scriptable and
    /// completable, and it is the spelling the docs can quote.
    pub(crate) fn slash_open(&mut self, arg: &str) {
        let arg = arg.trim();
        if arg.is_empty() {
            self.push_slash_info(
                "usage: /open @agent · /open #room · /open @main · ctrl+t for the team directory"
                    .to_string(),
            );
            return;
        }
        match self.resolve_target(arg) {
            Some(id) => self.switch_to(id),
            None => self.push_slash_info(format!(
                "no conversation called {arg} · /open lists what is open"
            )),
        }
    }

    // -- rooms -------------------------------------------------------------

    /// Which room a `/join` or `/leave` is about: the one named, or the one you
    /// are standing in. Standing in it is the common case — you found it in the
    /// directory, read it, and decided to speak — so naming it again would be
    /// ceremony.
    fn room_arg(&self, arg: &str) -> Option<String> {
        let named = arg.trim().trim_start_matches('#').trim();
        if !named.is_empty() {
            return Some(named.to_string());
        }
        match self.active_buffer() {
            BufferId::Channel(name) => Some(name),
            _ => None,
        }
    }

    /// `/join [#room]` — stop watching and become a member.
    ///
    /// There is no quiet way in: the domain writes the join into the room's
    /// record, so every member sees the same line the joiner does. That is the
    /// whole reason observing is allowed to be free — the moment it stops being
    /// reading and starts being participation, it is announced.
    pub(crate) fn slash_join(&mut self, arg: &str) {
        let Some(room) = self.room_arg(arg) else {
            self.push_slash_info(
                "usage: /join #room — or press j on a room in the team directory (ctrl+t)"
                    .to_string(),
            );
            return;
        };
        match self.session.channels.invite(&room, USER_NAME) {
            Ok(()) => {
                self.refresh_conversations();
                // The membership line lands in the room's log, so the flow you
                // are looking at picks it up like any other arrival.
                self.poll_active_conversation();
                self.dirty = true;
            }
            Err(why) => self.push_slash_info(why),
        }
    }

    /// `/leave [#room]` — stop being a member; the room stays readable.
    pub(crate) fn slash_leave(&mut self, arg: &str) {
        let Some(room) = self.room_arg(arg) else {
            self.push_slash_info("usage: /leave #room".to_string());
            return;
        };
        match self.session.channels.kick(&room, USER_NAME) {
            Ok(()) => {
                self.refresh_conversations();
                self.poll_active_conversation();
                self.dirty = true;
            }
            Err(why) => self.push_slash_info(why),
        }
    }

    /// The standing hint under a room the user is only watching, or `None`
    /// anywhere else. The composer is live — slash commands and `ctrl+k` mean
    /// what they mean everywhere — but Enter will not put words in a room whose
    /// roster the user is not on, and the hint says so before they try.
    pub(crate) fn observer_hint(&self) -> Option<&'static str> {
        match self.active_buffer() {
            BufferId::Channel(name) if !self.session.channels.is_member(&name, USER_NAME) => {
                Some(crate::tui::buffer::OBSERVER_HINT)
            }
            _ => None,
        }
    }

    /// A typed target → the conversation it names, or `None` when nothing by
    /// that name exists. The sigil is accepted and not required: `@scout`,
    /// `scout` and `#build` all name what the user obviously means, and a name
    /// that is both a channel and an instance resolves to the channel, which is
    /// what the `#` reading of a bare word would have given.
    ///
    /// The home conversation answers to the same grammar as any participant —
    /// `@main` and a bare `main` — because it *is* a participant's pair view
    /// (D101). `hub` is not accepted: the word retired with the concept, and a
    /// retired spelling kept alive in the grammar is a second name for the one
    /// thing this batch existed to give a single name. `#main` still reads as a
    /// room, so a room may keep that name without shadowing the floor.
    pub(crate) fn resolve_target(&self, arg: &str) -> Option<BufferId> {
        let arg = arg.trim();
        let bare = arg.trim_start_matches(['@', '#']);
        if !arg.starts_with('#') && bare.eq_ignore_ascii_case(crate::channels::MAIN_NAME) {
            return Some(BufferId::Hub);
        }
        if bare.is_empty() {
            return None;
        }
        let channel = BufferId::Channel(bare.to_string());
        let dm = BufferId::Dm(bare.to_string());
        let wants_dm = arg.starts_with('@');
        let wants_channel = arg.starts_with('#');
        if !wants_dm && self.buffers.get(&channel).is_some() {
            return Some(channel);
        }
        if !wants_channel && self.buffers.get(&dm).is_some() {
            return Some(dm);
        }
        None
    }

    /// `/open`'s candidates: the registry itself, so a name the dropdown offers
    /// is a conversation that exists (D85's rule for every argument source).
    pub(crate) fn open_candidates(&self) -> Vec<ArgCandidate> {
        self.buffers
            .iter()
            .map(|buffer| {
                let unread = buffer.unread();
                let description = match (buffer.id(), unread) {
                    // The one candidate described by what it *is* rather than by
                    // what is waiting in it, and named the way D100's directory
                    // row names it, so the two doors into home agree.
                    (BufferId::Hub, _) => "the console".to_string(),
                    (_, 0) => String::new(),
                    // A conversation that named you is worth saying out loud,
                    // because it is the one you were going to open anyway.
                    (_, n) if buffer.mention() => format!("{n} unread · wants you"),
                    (_, n) => format!("{n} unread"),
                };
                ArgCandidate::new(buffer.id().label(), description)
            })
            .collect()
    }
}

/// A *settled* post as rows — the shapes any stored conversation can contain:
/// a message somebody sent, a line nobody said, a step of an agent's work.
///
/// Split out of [`Chat::tail_post_rows`] for D96: the perspective page renders
/// the same posts with no instance behind them, and a second renderer beside
/// this one is exactly the thing the conversation engine has avoided since D89.
/// The two live-only kinds stay with the host, because they need the running
/// instance's clock and colour: [`PostKind::Typing`] and [`PostKind::Queued`].
pub(crate) fn settled_post_rows(
    theme: &crate::tui::theme::Theme,
    post: &Post,
    width: usize,
    sender: Option<&Sender<'_>>,
) -> Vec<Row> {
    let width = match sender {
        Some(s) => width.saturating_sub(s.gutter.width()),
        None => width,
    };
    let mut rows = match post.kind {
        // One dim line per work step, like the transcript's tool rows: cut,
        // not wrapped, so a long command stays one row.
        PostKind::Process => vec![Row::new(Line::styled(
            one_line(&post.text, width),
            SegStyle::fg(theme.text_secondary),
        ))],
        // Nobody said it, so it is furniture: the muted tier, one line, no name
        // over it and no stamp beside it (the source puts its own clock in the
        // text where it has one).
        PostKind::Note => vec![Row::new(Line::styled(
            one_line(&post.text, width),
            SegStyle::fg(theme.text_muted),
        ))],
        _ if post.you => user_message_rows(&post.text, width, theme),
        _ => agent_text_rows(theme, &post.text, width),
    };
    if let Some(s) = sender {
        // Process and note rows take the indentation and none of the face: the
        // message column stays one straight edge, and only somebody who spoke
        // gets a portrait beside what they said.
        let lead = s.lead && wears_a_face(post);
        s.gutter.apply(&mut rows, s.index, &post.from, lead);
    }
    rows
}

/// Whether a post is somebody speaking — the only kind that wears a portrait.
/// Work steps and runtime notes are furniture: nobody said them.
fn wears_a_face(post: &Post) -> bool {
    !matches!(post.kind, PostKind::Process | PostKind::Note)
}

/// Who a post is drawn as in a view that has an avatar gutter (D97): the
/// sender's portrait, and whether this post opens their run.
pub(crate) struct Sender<'a> {
    pub gutter: crate::tui::avatar::Gutter<'a>,
    pub index: usize,
    pub lead: bool,
}

/// Which posts open a sender's run, in order.
///
/// A run is broken by somebody else speaking and by nothing else: an agent's
/// tool rows sit inside its own turn, so a reply that resumes after them is
/// still the same person talking and does not earn a second portrait.
pub(crate) fn sender_runs(posts: &[Post]) -> Vec<bool> {
    let mut out = Vec::with_capacity(posts.len());
    let mut previous: Option<(bool, String)> = None;
    for post in posts {
        if !wears_a_face(post) {
            out.push(false);
            continue;
        }
        let key = (post.you, post.from.clone());
        out.push(previous.as_ref() != Some(&key));
        previous = Some(key);
    }
    out
}

/// An agent's prose, rendered the way main renders the model's.
fn agent_text_rows(theme: &crate::tui::theme::Theme, text: &str, width: usize) -> Vec<Row> {
    let mut processor = MarkdownProcessor::default();
    let mut renderer = MarkdownRenderer::with_theme(width.saturating_sub(2), theme.clone());
    let doc = processor.process_streaming(text);
    renderer.render(&doc);
    text_rows(theme, renderer.lines().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;
    use crate::api::types::{ContentBlock, Message as ApiMessage, Role as ApiRole};
    use crate::channels::ChannelMode;
    use crate::tui::chat::UiMessage;
    use crate::tui::test_util::chat_at;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn test_chat() -> Chat {
        chat_at(100, 40)
    }

    /// Every row the flow puts on screen, in order. The document *is* the
    /// assertion surface here: what these tests are about is which rows exist
    /// and which do not.
    fn flow(chat: &mut Chat) -> String {
        chat.build_rows(100);
        chat.doc
            .rows
            .iter()
            .map(|row| row.line.plain_text())
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn assistant(text: &str) -> ApiMessage {
        ApiMessage {
            role: ApiRole::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    fn user(text: &str) -> ApiMessage {
        ApiMessage {
            role: ApiRole::User,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    /// A message the *user* sent, in the shape `absorb_inbox` records it: the
    /// D64 marker heading the text. Unmarked prose in an instance's record is
    /// the main agent talking, and the pair view keeps the two apart (D99).
    fn from_user(text: &str) -> ApiMessage {
        user(&format!(
            "{}\n{text}",
            crate::tool::agent::DM_FROM_USER_MARKER
        ))
    }

    /// An instance with history already behind it.
    fn seed_agent(chat: &Chat, name: &str, history: Vec<ApiMessage>) {
        chat.session.agents.insert(
            name,
            AgentKind::Hire,
            None,
            "test instance".to_string(),
            chat.session.clone(),
        );
        if !history.is_empty() {
            chat.session.agents.finish(name, history, 0);
        }
    }

    /// A crew member (D53): a teammate from the blueprint rather than a hire.
    fn seed_crew(chat: &Chat, name: &str) {
        chat.session.agents.insert(
            name,
            AgentKind::Crew,
            None,
            "crew member".to_string(),
            chat.session.clone(),
        );
    }

    fn main_message(chat: &mut Chat, role: Role, text: &str) {
        chat.messages.push(UiMessage {
            role,
            text: text.to_string(),
            at: 0,
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
            digest: false,
        });
    }

    // -- switching ---------------------------------------------------------

    /// The switch puts a rule and the conversation's own history on screen, and
    /// the history is the same text the extraction produces — one path, not a
    /// second reader that could disagree with it.
    #[test]
    fn a_switch_opens_the_conversation_under_a_rule() {
        let mut chat = test_chat();
        seed_agent(
            &chat,
            "scout",
            vec![from_user("look at the parser"), assistant("found it")],
        );
        chat.refresh_conversations();
        chat.switch_to(BufferId::Dm("scout".to_string()));

        let text = flow(&mut chat);
        assert!(text.contains("── @scout ──"), "{text}");
        assert!(text.contains("look at the parser"), "{text}");
        assert!(text.contains("found it"), "{text}");

        // The same text the extraction yields, in the same order: the flow has
        // no reader of its own.
        let (history, stamps, ..) = chat
            .session
            .agents
            .view_of("scout")
            .expect("the instance is registered");
        let want: Vec<String> = dm_posts(&history, &stamps, &[], &[], &[], "scout")
            .into_iter()
            .filter(|post| matches!(post.kind, PostKind::Said | PostKind::Note))
            .map(|post| post.text)
            .collect();
        for line in want {
            assert!(text.contains(&line), "replay is missing {line:?}: {text}");
        }
    }

    /// The replay is bounded, and it keeps the end of the conversation rather
    /// than the beginning — the recent tail is what a reader coming back needs.
    #[test]
    fn a_replay_keeps_to_its_budget_and_keeps_the_tail() {
        let mut chat = test_chat();
        // Alternating, because a run of the agent's replies is one message now
        // (D99) and a budget counted over messages needs messages to count.
        let history: Vec<ApiMessage> = (0..REPLAY_BUDGET + 6)
            .map(|i| {
                if i % 2 == 0 {
                    from_user(&format!("message {i}"))
                } else {
                    assistant(&format!("message {i}"))
                }
            })
            .collect();
        seed_agent(&chat, "scout", history);
        chat.refresh_conversations();
        chat.switch_to(BufferId::Dm("scout".to_string()));

        let text = flow(&mut chat);
        assert!(!text.contains("message 0"), "the head is dropped: {text}");
        assert!(
            text.contains(&format!("message {}", REPLAY_BUDGET + 5)),
            "the tail is kept: {text}"
        );
        let shown = (0..REPLAY_BUDGET + 6)
            .filter(|i| {
                text.contains(&format!("message {i}\n")) || text.ends_with(&format!("message {i}"))
            })
            .count();
        assert!(shown <= REPLAY_BUDGET, "{shown} rows exceed the budget");
    }

    /// The composer belongs to whichever conversation is active, so what was
    /// half-typed in one is waiting in it on the way back.
    #[test]
    fn a_draft_waits_in_the_conversation_it_was_typed_in() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.refresh_conversations();

        chat.set_input("half a main thought");
        chat.switch_to(BufferId::Dm("scout".to_string()));
        assert_eq!(chat.input, "", "the DM starts empty");

        chat.set_input("half a scout thought");
        chat.switch_to(BufferId::Hub);
        assert_eq!(chat.input, "half a main thought", "main's draft is back");

        chat.switch_to(BufferId::Dm("scout".to_string()));
        assert_eq!(chat.input, "half a scout thought", "and so is the DM's");
    }

    /// Switching to where you already are is not a switch. It used to be worth
    /// saying out loud: the alternative prints the conversation twice for a
    /// keypress that changed nothing.
    #[test]
    fn switching_to_the_active_conversation_does_nothing() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", vec![assistant("hello")]);
        chat.refresh_conversations();
        chat.switch_to(BufferId::Dm("scout".to_string()));
        let once = flow(&mut chat);
        let rows = chat.messages.len();

        chat.switch_to(BufferId::Dm("scout".to_string()));
        assert_eq!(chat.messages.len(), rows, "nothing was appended");
        assert_eq!(flow(&mut chat), once, "and nothing was reprinted");

        // Main is not special: re-entering it from main is the same no-op.
        chat.switch_to(BufferId::Hub);
        let back = chat.messages.len();
        chat.switch_to(BufferId::Hub);
        assert_eq!(chat.messages.len(), back);
    }

    // -- the record's doors (D100) -----------------------------------------

    /// `tab` on an empty composer opens the record of the conversation you are
    /// in: the agent's page in a DM, main's in the console. A room has no single
    /// protagonist, so the key means nothing there and is not consumed.
    #[test]
    fn tab_on_an_empty_composer_opens_the_conversations_record() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", vec![from_user("hi"), assistant("hello")]);
        chat.session
            .channels
            .create(
                "build",
                vec![crate::channels::USER_NAME.to_string()],
                ChannelMode::Free,
            )
            .expect("room created");
        chat.refresh_conversations();

        assert!(chat.on_key(KeyCode::Tab, KeyModifiers::NONE), "the console");
        assert_eq!(chat.open_perspective.as_deref(), Some("main"));

        chat.open_perspective = None;
        chat.switch_to(BufferId::Dm("scout".to_string()));
        assert!(chat.on_key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(chat.open_perspective.as_deref(), Some("scout"));

        chat.open_perspective = None;
        chat.switch_to(BufferId::Channel("build".to_string()));
        assert!(
            !chat.on_key(KeyCode::Tab, KeyModifiers::NONE),
            "a room has no protagonist, so the key is left unclaimed"
        );
        assert_eq!(chat.open_perspective, None);
    }

    /// With text in the composer `tab` is still completion: the slash and `@`
    /// dropdowns are judged well above the door, and a bare word reaches it only
    /// to be ignored because the composer is not empty.
    #[test]
    fn tab_with_a_draft_still_completes_and_opens_nothing() {
        let mut chat = test_chat();
        chat.set_input("/mo");
        assert!(chat.on_key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(chat.input, "/model ");
        assert_eq!(chat.open_perspective, None, "completion, not a page");

        chat.set_input("half a sentence");
        assert!(!chat.on_key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(chat.open_perspective, None);
        assert_eq!(chat.input, "half a sentence", "and the draft is untouched");
    }

    /// The door respects the modality every full-screen surface does (D81): a
    /// permission question is holding up a turn, and nothing opens over it.
    #[test]
    fn the_record_door_is_inert_behind_a_permission_ask() {
        let mut chat = test_chat();
        let (tx, _rx) = tokio::sync::oneshot::channel();
        chat.pending_ask = Some((
            crate::ui::PermissionRequest::new(
                "Allow Bash",
                "cargo test",
                vec![crate::ui::ASK_YES.into(), crate::ui::ASK_NO.into()],
            ),
            tx,
        ));
        assert!(!chat.on_key(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(chat.open_perspective, None);
    }

    /// D99's honest consequence gets a door: an agent main spawned and the user
    /// never wrote to opens with its rule and one dim line pointing at the
    /// record. It is not a replay item, so the first real message still prints.
    #[test]
    fn an_empty_pair_opens_with_a_note_pointing_at_the_record() {
        let mut chat = test_chat();
        // Spawn prompt and a report to main: both belong to lanes that are not
        // the user's, so the pair view is empty.
        seed_agent(
            &chat,
            "scout",
            vec![user("map the parser"), assistant("mapped it")],
        );
        chat.refresh_conversations();
        chat.switch_to(BufferId::Dm("scout".to_string()));

        let text = flow(&mut chat);
        assert!(text.contains("── @scout ──"), "{text}");
        assert!(
            text.contains("· no conversation yet · tab opens @scout's record ·"),
            "{text}"
        );
        assert!(
            !text.contains("mapped it"),
            "the pair view stays pure: {text}"
        );

        // Switching out and back does not stack the note up: the flow already
        // shows it, and the rules a round trip prints are not content.
        chat.switch_to(BufferId::Hub);
        chat.switch_to(BufferId::Dm("scout".to_string()));
        let text = flow(&mut chat);
        assert_eq!(
            text.matches("no conversation yet").count(),
            1,
            "the note is printed once: {text}"
        );

        // And a message makes it moot: the note is gone from the next opening,
        // and the message itself still prints.
        chat.switch_to(BufferId::Hub);
        chat.session.agents.finish(
            "scout",
            vec![
                user("map the parser"),
                assistant("mapped it"),
                from_user("what did you find?"),
                assistant("a missing case"),
            ],
            0,
        );
        chat.switch_to(BufferId::Dm("scout".to_string()));
        let text = flow(&mut chat);
        assert_eq!(
            text.matches("no conversation yet").count(),
            1,
            "no second note once the pair has content: {text}"
        );
        assert!(text.contains("what did you find?"), "{text}");
        assert!(text.contains("a missing case"), "{text}");
    }

    /// The note is furniture, not a replay item. If it were counted the poll's
    /// cursor would start at one and the first message the pair ever gets would
    /// be read as already printed.
    #[test]
    fn the_empty_pair_note_does_not_swallow_the_first_message() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", vec![user("map the parser")]);
        chat.refresh_conversations();
        chat.switch_to(BufferId::Dm("scout".to_string()));
        assert!(flow(&mut chat).contains("no conversation yet"));

        chat.session.agents.finish(
            "scout",
            vec![user("map the parser"), from_user("are you there?")],
            0,
        );
        chat.poll_active_conversation();
        let text = flow(&mut chat);
        assert!(text.contains("are you there?"), "{text}");
    }

    // -- the excursion -----------------------------------------------------

    /// The ruling's central promise: while you are in a conversation, nothing
    /// else prints into it. A main turn that lands while you are away is
    /// main's news, and it waits at @main for you.
    #[test]
    fn an_excursion_holds_mains_tail_until_you_come_back() {
        let mut chat = test_chat();
        seed_agent(
            &chat,
            "scout",
            vec![from_user("have a look"), assistant("on it")],
        );
        chat.refresh_conversations();
        main_message(&mut chat, Role::Assistant, "before you left");

        chat.switch_to(BufferId::Dm("scout".to_string()));
        // A main turn completes while the DM is on screen.
        main_message(&mut chat, Role::Assistant, "landed while you were away");

        let away = flow(&mut chat);
        assert!(away.contains("before you left"), "{away}");
        assert!(away.contains("── @scout ──"), "{away}");
        assert!(away.contains("on it"), "{away}");
        assert!(
            !away.contains("landed while you were away"),
            "main printed into the DM: {away}"
        );

        chat.switch_to(BufferId::Hub);
        let home = flow(&mut chat);
        assert!(
            home.contains("── @main ──"),
            "the rule hands the flow back: {home}"
        );
        assert!(
            home.contains("landed while you were away"),
            "main's tail follows it: {home}"
        );
        // Order: the DM segment closes before main's tail resumes.
        let rule = home.find("── @main ──").expect("the closing rule");
        let tail = home.find("landed while you were away").expect("the tail");
        assert!(rule < tail, "the tail follows the rule: {home}");
        assert!(
            home.find("on it").expect("the replay") < rule,
            "and the replay precedes it: {home}"
        );
    }

    /// The flow only ever grows at the end. That is what the write-once flush
    /// cursor rests on: a position that has been printed is the terminal's
    /// property, and re-ordering it would mean rewriting scrollback.
    #[test]
    fn the_print_order_only_ever_grows_at_the_end() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", vec![assistant("first")]);
        chat.refresh_conversations();
        main_message(&mut chat, Role::Assistant, "main one");

        let mut seen = chat.flow_order();
        let step = |chat: &Chat, seen: &mut Vec<FlowItem>| {
            let now = chat.flow_order();
            assert!(
                now.starts_with(seen),
                "the printed prefix changed:\n before {seen:?}\n after  {now:?}"
            );
            *seen = now;
        };
        chat.switch_to(BufferId::Dm("scout".to_string()));
        step(&chat, &mut seen);
        main_message(&mut chat, Role::Assistant, "main two");
        step(&chat, &mut seen);
        chat.switch_to(BufferId::Hub);
        step(&chat, &mut seen);
        main_message(&mut chat, Role::Assistant, "main three");
        step(&chat, &mut seen);
        chat.switch_to(BufferId::Dm("scout".to_string()));
        step(&chat, &mut seen);
        chat.switch_to(BufferId::Hub);
        step(&chat, &mut seen);
    }

    // -- speaking ----------------------------------------------------------

    /// A DM submit is a delivery, not a turn: it reaches the instance, it shows
    /// at once, and `busy` — which belongs to the model conversation — is
    /// untouched.
    #[tokio::test]
    async fn a_dm_submit_delivers_and_echoes_without_starting_a_turn() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.refresh_conversations();
        chat.switch_to(BufferId::Dm("scout".to_string()));

        chat.set_input("have a look at the parser");
        chat.submit();
        assert!(!chat.busy, "no main turn was started");
        assert!(chat.input.is_empty(), "the composer cleared");
        assert!(chat.queued.is_empty(), "and it did not queue behind main");

        // The shape the workspace composer produced: an inbox item from
        // `user`, which is what earns the D64 marker when the instance picks it
        // up — added downstream, never here (D88).
        let items = chat.session.agents.take_running("scout", 0);
        let (prompt, _) = crate::tool::agent::absorb_inbox(&chat.session.channels, "scout", &items);
        assert_eq!(
            prompt,
            format!(
                "{}\nhave a look at the parser",
                crate::tool::agent::DM_FROM_USER_MARKER
            )
        );
    }

    /// A channel submit posts as the user and the post is on screen before the
    /// next poll: the log is the store, and the flow follows it immediately.
    #[tokio::test]
    async fn a_channel_submit_posts_and_shows_at_once() {
        let mut chat = test_chat();
        chat.session
            .channels
            .create(
                "build",
                vec![USER_NAME.to_string(), "scout".to_string()],
                ChannelMode::Free,
            )
            .expect("channel created");
        chat.refresh_conversations();
        chat.switch_to(BufferId::Channel("build".to_string()));

        chat.set_input("ship it");
        chat.submit();
        assert!(!chat.busy, "no main turn was started");

        let log = chat.session.channels.log_of("build");
        assert_eq!(log.len(), 1, "one post: {log:?}");
        assert_eq!(log[0].from, USER_NAME);
        assert!(flow(&mut chat).contains("ship it"), "and it is on screen");
    }

    // -- observing a room --------------------------------------------------

    /// The whole observer contract in one test: a room the user is not in opens
    /// read-only under its own rule, the composer says so before and after the
    /// attempt, and nothing lands in the log.
    #[tokio::test]
    async fn a_room_you_are_not_in_opens_read_only_and_says_why() {
        let mut chat = test_chat();
        chat.session
            .channels
            .create(
                "parser",
                vec!["scout".to_string(), "zoe".to_string()],
                ChannelMode::Free,
            )
            .expect("room created");
        chat.session
            .channels
            .post("scout", "parser", "the tokenizer is the problem")
            .expect("posted");
        chat.refresh_conversations();
        assert!(
            chat.buffers
                .get(&BufferId::Channel("parser".to_string()))
                .is_none(),
            "it is not one of the user's conversations"
        );

        chat.switch_to(BufferId::Channel("parser".to_string()));
        let text = flow(&mut chat);
        assert!(
            text.contains("── #parser · observer · read-only ──"),
            "the rule frames it: {text}"
        );
        assert!(
            text.contains("the tokenizer is the problem"),
            "and reading is free: {text}"
        );
        assert_eq!(
            chat.observer_hint(),
            Some(crate::tui::buffer::OBSERVER_HINT),
            "the composer says so standing still"
        );

        chat.set_input("nice work everyone");
        chat.submit();
        assert!(!chat.busy);
        assert!(
            chat.slash_info_lines
                .iter()
                .any(|line| line.contains("/join")),
            "the refusal names the way in: {:?}",
            chat.slash_info_lines
        );
        assert!(
            chat.slash_error_lines.is_empty(),
            "and it is not an error: nothing failed"
        );
        assert!(
            chat.session.channels.log_of("parser").len() == 1,
            "nothing was posted"
        );

        // Esc goes home, exactly as it does from every other conversation: the
        // destination must not depend on how you arrived (D89's BackToMain). The
        // info line is peeled first, as it is anywhere else.
        chat.slash_info_lines.clear();
        assert!(chat.on_key(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE
        ));
        assert_eq!(*chat.buffers.active(), BufferId::Hub);
        assert_eq!(chat.observer_hint(), None, "and the hint went with it");
    }

    /// Joining is a membership event everyone in the room sees, and it is what
    /// turns the room into one of the user's conversations.
    #[tokio::test]
    async fn joining_announces_itself_and_opens_the_composer() {
        let mut chat = test_chat();
        chat.session
            .channels
            .create("parser", vec!["scout".to_string()], ChannelMode::Free)
            .expect("room created");
        chat.refresh_conversations();
        chat.switch_to(BufferId::Channel("parser".to_string()));

        chat.run_slash("join");
        assert!(
            chat.session.channels.is_member("parser", USER_NAME),
            "the user is on the roster"
        );
        assert!(
            chat.buffers
                .get(&BufferId::Channel("parser".to_string()))
                .is_some(),
            "the room is in the bar"
        );
        assert_eq!(chat.observer_hint(), None, "and the composer is live");
        let text = flow(&mut chat);
        assert!(
            text.contains("· user joined ·"),
            "everyone in the room is told: {text}"
        );

        // Speaking now lands, in the same room, through the ordinary path.
        chat.set_input("ship it");
        chat.submit();
        let log = chat.session.channels.log_of("parser");
        assert_eq!(log.last().map(|m| m.text.as_str()), Some("ship it"));

        // And leaving is the counterpart: announced, and off the bar again.
        chat.run_slash("leave");
        assert!(!chat.session.channels.is_member("parser", USER_NAME));
        assert!(
            chat.buffers
                .get(&BufferId::Channel("parser".to_string()))
                .is_none(),
            "a room you left is not your conversation"
        );
        assert!(flow(&mut chat).contains("· user left ·"));
    }

    /// Slash commands act on the application, so they mean the same thing in
    /// every conversation — including the one where the composer is otherwise
    /// speaking to a subagent.
    #[test]
    fn a_slash_command_still_reaches_the_app_from_a_conversation() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.refresh_conversations();
        chat.switch_to(BufferId::Dm("scout".to_string()));

        chat.set_input("/help");
        chat.submit();
        assert!(
            !chat.slash_info_lines.is_empty(),
            "the command ran rather than being sent to scout"
        );
        assert!(
            chat.session.agents.pending_of("scout").is_empty(),
            "and the instance heard nothing"
        );
    }

    // -- living in a conversation -----------------------------------------

    /// What arrives for the conversation you are in prints; what arrives for
    /// one you are not in counts. Both halves of the same rule.
    #[test]
    fn arrivals_print_here_and_count_there() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        seed_agent(&chat, "zoe", Vec::new());
        chat.refresh_conversations();
        chat.switch_to(BufferId::Dm("scout".to_string()));

        chat.session.agents.finish(
            "scout",
            vec![from_user("how is it?"), assistant("the parser is fine")],
            0,
        );
        chat.session.agents.finish(
            "zoe",
            vec![from_user("and you?"), assistant("nobody is reading this")],
            0,
        );
        // The active conversation follows every frame; the registry's unread
        // accounting rides the fifteen-tick poll, so run a full cadence.
        for _ in 0..15 {
            chat.tick();
        }

        let text = flow(&mut chat);
        assert!(text.contains("the parser is fine"), "{text}");
        assert!(
            !text.contains("nobody is reading this"),
            "an inactive conversation printed into this one: {text}"
        );
        let zoe = chat
            .buffers
            .get(&BufferId::Dm("zoe".to_string()))
            .expect("zoe has a buffer");
        assert_eq!(zoe.unread(), 2, "it counted instead");
        assert!(zoe.mention(), "and it answered you, so it wants you (D99)");
    }

    /// The in-flight state is a state, not a record: it renders in the tail and
    /// never settles, so nothing on the way to an answer reaches scrollback.
    #[tokio::test]
    async fn a_dm_in_flight_renders_in_the_tail() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.refresh_conversations();
        chat.switch_to(BufferId::Dm("scout".to_string()));

        chat.set_input("are you there");
        chat.submit();

        chat.build_rows(100);
        let text = chat
            .doc
            .rows
            .iter()
            .map(|row| row.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            text.contains("are you there"),
            "the message shows the moment it is sent: {text}"
        );
        assert!(
            text.contains("scout is replying…"),
            "and the wait is stated: {text}"
        );
        assert!(
            chat.doc.transient_rows > 0,
            "the tail is transient, so it never freezes into scrollback"
        );
    }

    /// D99: the DM's live tail is the *user's* run. An agent working on a room
    /// relay, on main's instruction or on a chase is not answering the person
    /// reading this conversation, so its stream shows nothing here — not even
    /// the typing row, which would promise a reply nobody asked for. Its own
    /// page is where that run is watched.
    #[tokio::test]
    async fn a_run_that_is_not_yours_has_no_tail_in_your_dm() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.refresh_conversations();
        chat.switch_to(BufferId::Dm("scout".to_string()));

        // Main sends, and the run that drains it starts.
        chat.session
            .agents
            .deliver(
                "scout",
                crate::channels::MAIN_NAME,
                "look at the parser",
                Vec::new(),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        let items = chat.session.agents.take_running("scout", 0);
        chat.session
            .agents
            .set_run_trigger("scout", crate::tool::agent::wakes_owner(&items));
        chat.session.agents.set_live(
            "scout",
            Some(std::sync::Arc::new(std::sync::Mutex::new(vec![
                crate::agents::LiveBlock::Text("main, I am on it".to_string()),
            ]))),
            None,
        );

        let text = tail_text(&mut chat);
        assert!(
            !text.contains("main, I am on it"),
            "main's run streams onto main's page, not into the DM: {text}"
        );
        assert!(
            !text.contains("look at the parser"),
            "and main's message is not the user's bubble: {text}"
        );
        assert!(
            !text.contains("is replying…"),
            "no typing row for a reply that is not owed to you: {text}"
        );

        // The same instance, the same stream, once the run draining it is the
        // user's own: the tail is back, and so is what they sent.
        chat.set_input("and for me?");
        chat.submit();
        let mine = chat.session.agents.take_running("scout", 0);
        assert!(
            !crate::tool::agent::wakes_owner(&mine),
            "a batch of the user's own DMs is nobody else's business"
        );
        chat.session
            .agents
            .set_run_trigger("scout", crate::tool::agent::wakes_owner(&mine));
        let text = tail_text(&mut chat);
        assert!(text.contains("and for me?"), "{text}");
        assert!(text.contains("main, I am on it"), "{text}");
    }

    /// Everything the flow has printed plus the transient tail, as text.
    fn tail_text(chat: &mut Chat) -> String {
        chat.build_rows(100);
        chat.doc
            .rows
            .iter()
            .map(|row| row.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The budget is eight, and it is stated where the reasoning is (D99): the
    /// record keeps the whole conversation and the flow keeps the scrollback of
    /// every visit, so a switch owes the reader the last exchange rather than a
    /// screenful of history above the composer.
    #[test]
    fn a_switch_replays_the_last_exchange_rather_than_a_screenful() {
        assert_eq!(REPLAY_BUDGET, 8);
    }

    // -- the team feed -----------------------------------------------------

    /// `/team` answers into the team's feed, and main keeps one line saying
    /// where the answer went. The feed is a column of the directory now, so the
    /// pointer names the key that opens it rather than a buffer to switch to
    /// (`the_board_renders_its_lifecycle_log` moved to `tui::directory`, which
    /// is where those rows are now built).
    #[test]
    fn team_output_lands_in_the_feed_and_says_where() {
        let mut chat = test_chat();
        seed_crew(&chat, "dev");
        chat.refresh_conversations();
        chat.run_slash("team");

        assert!(
            chat.buffers
                .team_log()
                .iter()
                .any(|event| event.label == "/team"),
            "the answer is in the feed: {:?}",
            chat.buffers.team_log()
        );
        assert!(
            chat.slash_info_lines
                .iter()
                .any(|line| line == "→ team (ctrl+t)"),
            "and main points at the key that opens it: {:?}",
            chat.slash_info_lines
        );
        assert!(
            !chat
                .messages
                .iter()
                .any(|message| message.text.contains("→ team")),
            "the pointer is an info line, not a message in the transcript"
        );
        assert_eq!(
            *chat.buffers.active(),
            BufferId::Hub,
            "and nothing was opened: the team is not a conversation"
        );

        // With the directory already open, the answer is where you are looking
        // and there is nothing to point at.
        chat.open_directory();
        chat.slash_info_lines.clear();
        chat.run_slash("team");
        assert!(
            chat.slash_info_lines.is_empty(),
            "no pointer is needed when you are already there: {:?}",
            chat.slash_info_lines
        );
        assert!(
            chat.directory_rows()
                .iter()
                .any(|row| row.text.starts_with("/team")),
            "the entry names its command"
        );
    }

    // -- line-leading routing ---------------------------------------------

    /// The central promise: the message reaches the teammate, the flow does not
    /// move, and no turn starts. The domain assertion is the one the DM-buffer
    /// submit makes, because it has to be the same delivery.
    #[tokio::test]
    async fn a_leading_name_delivers_from_main_without_moving() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.refresh_conversations();

        chat.set_input("@scout have a look at the parser");
        chat.submit();

        assert!(!chat.busy, "a delivery is not a turn");
        assert_eq!(*chat.buffers.active(), BufferId::Hub, "the flow stayed put");
        assert!(chat.queued.is_empty(), "and it did not queue behind main");

        // Byte-identical to what a submit inside the DM produces (D88/D89).
        let items = chat.session.agents.take_running("scout", 0);
        let (prompt, _) = crate::tool::agent::absorb_inbox(&chat.session.channels, "scout", &items);
        assert_eq!(
            prompt,
            format!(
                "{}\nhave a look at the parser",
                crate::tool::agent::DM_FROM_USER_MARKER
            )
        );

        let text = flow(&mut chat);
        assert!(
            text.contains("→ @scout: have a look at the parser"),
            "the receipt says where it went: {text}"
        );
        assert!(
            !text.contains("── @scout ──"),
            "and nothing opened a conversation: {text}"
        );
    }

    /// A channel is the same rule with the other sigil.
    #[tokio::test]
    async fn a_leading_channel_name_posts_without_moving() {
        let mut chat = test_chat();
        chat.session
            .channels
            .create(
                "build",
                vec!["scout".to_string(), USER_NAME.to_string()],
                ChannelMode::Free,
            )
            .expect("channel created");
        chat.refresh_conversations();

        chat.set_input("#build ship it");
        chat.submit();

        assert!(!chat.busy);
        assert_eq!(*chat.buffers.active(), BufferId::Hub);
        let log = chat.session.channels.log_of("build");
        assert_eq!(log.len(), 1, "one post: {log:?}");
        assert_eq!(log[0].from, USER_NAME);
        assert_eq!(log[0].text, "ship it");
        assert!(flow(&mut chat).contains("→ #build: ship it"));
    }

    /// No magic and no error: a name that resolves to nothing is prose, and
    /// prose submits to main exactly as typed.
    #[tokio::test]
    async fn an_unknown_name_is_just_prose() {
        let mut chat = test_chat();
        chat.refresh_conversations();

        chat.set_input("@nobody are you there");
        chat.submit();

        assert!(chat.busy, "it opened an ordinary main turn");
        assert_eq!(
            chat.last_prompt, "@nobody are you there",
            "verbatim, envelope and all"
        );
        assert!(
            chat.slash_error_lines.is_empty(),
            "nothing failed, so nothing is reported: {:?}",
            chat.slash_error_lines
        );
        assert!(
            !flow(&mut chat).contains("→ @nobody"),
            "and no receipt was written for a delivery that never happened"
        );
    }

    /// The asymmetry, stated as a test: inside a conversation the buffer *is*
    /// the target, so a leading name is a person being addressed in a sentence
    /// rather than an envelope around one.
    #[tokio::test]
    async fn a_conversation_reads_a_leading_name_as_text() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        seed_agent(&chat, "zoe", Vec::new());
        chat.refresh_conversations();
        chat.switch_to(BufferId::Dm("scout".to_string()));

        chat.set_input("@zoe should look at this too");
        chat.submit();

        let items = chat.session.agents.take_running("scout", 0);
        let (prompt, _) = crate::tool::agent::absorb_inbox(&chat.session.channels, "scout", &items);
        assert!(
            prompt.contains("@zoe should look at this too"),
            "the whole line went to scout: {prompt}"
        );
        assert!(
            chat.session.agents.pending_of("zoe").is_empty(),
            "zoe heard nothing"
        );
    }

    /// A name with nothing after it is not an envelope — it is someone being
    /// mentioned, and it belongs to main like any other sentence.
    #[test]
    fn a_bare_name_is_not_a_route() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.refresh_conversations();
        assert_eq!(chat.leading_route("@scout"), None);
        assert_eq!(chat.leading_route("@scout   "), None);
        assert_eq!(
            chat.leading_route("scout hello"),
            None,
            "the sigil is required"
        );
        assert_eq!(
            chat.leading_route("@Scout hello"),
            None,
            "and the name resolves exactly, not case-insensitively"
        );
        assert_eq!(
            chat.leading_route("@scout hello"),
            Some((BufferId::Dm("scout".to_string()), "hello".to_string()))
        );
    }

    /// The receipt is a state line: one dim row, no `❯` bubble putting the
    /// envelope in the user's mouth, no send stamp, and long messages cut.
    #[test]
    fn the_receipt_is_a_state_line() {
        let long = "x".repeat(80);
        let id = BufferId::Dm("scout".to_string());
        let line = receipt_line(&id, &long);
        assert!(line.starts_with("→ @scout: "), "{line}");
        assert!(line.ends_with('…'), "a long message is cut: {line}");
        assert!(
            crate::tui::chat::is_state_line(&line),
            "so it renders as a state, not as a message: {line}"
        );
        assert!(is_route_receipt(&line));
        // Newlines are flattened: the receipt is one row by construction.
        assert_eq!(
            receipt_line(&id, "two\nlines"),
            "→ @scout: two lines".to_string()
        );
        // Prose that merely opens with an arrow is not a receipt.
        assert!(!is_route_receipt("→ and then we shipped it"));
    }

    // -- /open -------------------------------------------------------------

    /// `/open` reaches every kind of conversation, with or without its sigil,
    /// and says so when it cannot.
    #[test]
    fn open_reaches_every_conversation_and_reports_the_ones_it_cannot() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        seed_crew(&chat, "dev");
        chat.session
            .channels
            .create(
                "build",
                vec![USER_NAME.to_string(), "scout".to_string()],
                ChannelMode::Free,
            )
            .expect("channel created");
        chat.buffers.note_watch_event(
            "scout #1 · go",
            crate::watch::WatchKind::Agent,
            crate::watch::WatchState::Running,
            None,
            1,
        );
        chat.refresh_conversations();

        chat.run_slash("open @scout");
        assert_eq!(*chat.buffers.active(), BufferId::Dm("scout".to_string()));
        chat.run_slash("open #build");
        assert_eq!(
            *chat.buffers.active(),
            BufferId::Channel("build".to_string())
        );
        chat.run_slash("open @main");
        assert_eq!(*chat.buffers.active(), BufferId::Hub);
        // The sigil is an accepted spelling, not a requirement.
        chat.run_slash("open scout");
        assert_eq!(*chat.buffers.active(), BufferId::Dm("scout".to_string()));
        // …and home follows the same grammar as any participant (D101).
        chat.run_slash("open main");
        assert_eq!(*chat.buffers.active(), BufferId::Hub);

        // The retired word is not a second spelling of it: `hub` names nothing.
        chat.run_slash("open @scout");
        chat.slash_info_lines.clear();
        chat.run_slash("open hub");
        assert_eq!(
            *chat.buffers.active(),
            BufferId::Dm("scout".to_string()),
            "the retired word moves nothing"
        );
        assert!(
            chat.slash_info_lines
                .iter()
                .any(|line| line.contains("hub")),
            "and it is refused by name: {:?}",
            chat.slash_info_lines
        );

        chat.slash_info_lines.clear();
        chat.run_slash("open @nobody");
        assert_eq!(
            *chat.buffers.active(),
            BufferId::Dm("scout".to_string()),
            "an unknown target moves nothing"
        );
        assert!(
            chat.slash_info_lines
                .iter()
                .any(|line| line.contains("nobody")),
            "and it says which one: {:?}",
            chat.slash_info_lines
        );
    }

    /// The dropdown's candidates are the registry, so a name it offers is a
    /// conversation that exists (D85's rule for every argument source).
    #[test]
    fn open_completes_from_the_registry() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.session
            .channels
            .create(
                "build",
                vec![USER_NAME.to_string(), "scout".to_string()],
                ChannelMode::Free,
            )
            .expect("channel created");
        chat.refresh_conversations();

        chat.set_input("/open ");
        let offered: Vec<String> = chat
            .slash_suggestions
            .iter()
            .map(|item| item.name.clone())
            .collect();
        assert!(offered.contains(&"@main".to_string()), "{offered:?}");
        assert!(offered.contains(&"@scout".to_string()), "{offered:?}");
        assert!(offered.contains(&"#build".to_string()), "{offered:?}");
        assert!(
            !offered.iter().any(|name| name.contains("hub")),
            "the retired word left the grammar with the concept: {offered:?}"
        );

        // And every one of them resolves — the offer is the registry, so it
        // cannot name something `/open` would then refuse.
        for name in offered {
            assert!(
                chat.resolve_target(&name).is_some(),
                "{name} was offered but does not resolve"
            );
        }
    }
    // -- the avatar gutter (D97) ------------------------------------------

    /// Every document row with its leading columns intact — the gutter is
    /// exactly what a `plain_text()` that filters blanks would throw away, so
    /// these tests read the rows raw.
    fn raw_rows(chat: &mut Chat) -> Vec<String> {
        chat.build_rows(100);
        chat.doc
            .rows
            .iter()
            .map(|row| row.line.plain_text())
            .collect()
    }

    /// The row a piece of text landed on, gutter and all.
    fn row_with(rows: &[String], needle: &str) -> String {
        rows.iter()
            .find(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("no row contains {needle:?}: {rows:#?}"))
            .clone()
    }

    /// A DM's messages sit in a gutter and the first row of a sender's run
    /// carries their chip. Without the run rule a burst of replies would be a
    /// column of repeated portraits, which is what Slack's grouping is for.
    #[test]
    fn a_dm_wears_a_face_on_the_first_row_of_each_run() {
        let mut chat = test_chat();
        seed_agent(
            &chat,
            "scout",
            vec![
                from_user("look at the parser"),
                assistant("found it"),
                assistant("and fixed it"),
            ],
        );
        chat.refresh_conversations();
        chat.switch_to(BufferId::Dm("scout".to_string()));
        let rows = raw_rows(&mut chat);

        let gutter = crate::tui::avatar::gutter_width(false);
        // The name row opens the run, so it is the row wearing the chip. The
        // rule that opened the conversation names it too, and is not a message.
        let name_row = rows
            .iter()
            .find(|row| row.contains("scout") && !row.contains('─'))
            .unwrap_or_else(|| panic!("no name row: {rows:#?}"))
            .clone();
        assert!(
            name_row.starts_with(" S") && name_row.trim_end().ends_with("scout"),
            "the chip is the sender's initial on its colour, then the name: {name_row:?}"
        );
        // The second reply is the same sender continuing: no second chip.
        let second = row_with(&rows, "and fixed it");
        assert_eq!(
            second.chars().take(gutter).collect::<String>(),
            " ".repeat(gutter),
            "a continuation row's gutter is blank: {second:?}"
        );
        let first_body = row_with(&rows, "found it");
        assert_eq!(
            first_body.chars().take(gutter).collect::<String>(),
            " ".repeat(gutter),
            "and so is a body row under the name that opened the run: {first_body:?}"
        );
    }

    /// Rewritten for D99: the console wears the gutter too. It used to be the
    /// one conversation without one, on the argument that its grammar is Claude
    /// Code's; the better reading is that main is a participant like any other,
    /// and a face is how a participant is recognised. The user's rows wear here
    /// exactly what they wear in a DM, because it is the same machinery at one
    /// more call site.
    #[test]
    fn the_console_wears_the_same_gutter_every_conversation_does() {
        let mut chat = test_chat();
        main_message(&mut chat, Role::User, "a question");
        main_message(&mut chat, Role::Assistant, "main prose");
        let rows = raw_rows(&mut chat);
        let gutter = crate::tui::avatar::gutter_width(false);

        let mine = row_with(&rows, "a question");
        assert!(
            mine.starts_with(" U "),
            "the user's chip is the same one a DM draws: {mine:?}"
        );
        let main = row_with(&rows, "main prose");
        assert!(
            main.starts_with(" M "),
            "and main wears its own reserved face: {main:?}"
        );
        assert_eq!(
            crate::tui::avatar::Gutter::new(
                false,
                &crate::tui::avatar::Palette::new(&chat.theme),
                &chat.faces_pinned
            )
            .index_for(crate::channels::MAIN_NAME),
            crate::tui::avatar::MAIN_INDEX
        );
        // Everything below the opening row of a run takes the indentation and
        // no face, exactly as in a DM.
        main_message(&mut chat, Role::Assistant, "a second paragraph");
        let rows = raw_rows(&mut chat);
        let body = row_with(&rows, "a second paragraph");
        assert_eq!(
            body.chars().take(gutter).collect::<String>(),
            " ".repeat(gutter),
            "a continuation of main's run has a blank gutter: {body:?}"
        );
    }

    /// The D97 invariant, extended to the console: the two skins differ in the
    /// gutter and nowhere else, so a terminal that cannot place images lays the
    /// window out exactly as one that can.
    #[test]
    fn the_console_lays_out_identically_in_both_skins() {
        let mut chip = test_chat();
        let mut placed = test_chat();
        placed.image_cap = Some(crate::tui::gfx::ImageCap::default_cells());
        for chat in [&mut chip, &mut placed] {
            main_message(chat, Role::User, "a question");
            main_message(chat, Role::Assistant, "main prose that runs on a while");
        }
        let chip_rows = raw_rows(&mut chip);
        let placed_rows = raw_rows(&mut placed);
        assert_eq!(
            chip_rows.len(),
            placed_rows.len(),
            "the row count is the same in both skins"
        );
        use crate::tui::line::text_width;
        // The message column opens at the gutter's own width in either skin, and
        // the body that follows is the same text: what changes between them is
        // the picture, never where the picture leaves off.
        let column = |rows: &[String], needle: &str, images: bool| {
            let row = row_with(rows, needle);
            let cut = row.find(needle).unwrap_or(0);
            assert_eq!(
                text_width(&row[..cut]),
                crate::tui::avatar::gutter_width(images),
                "{needle:?} does not start at the gutter's edge: {row:?}"
            );
            row[cut..].to_string()
        };
        for needle in ["❯ a question", "⏺ main prose"] {
            assert_eq!(
                column(&chip_rows, needle, false),
                column(&placed_rows, needle, true),
                "the message column differs between the skins"
            );
        }
    }

    /// A tool row and a membership line take the indent and no face: the column
    /// stays one straight edge, and only somebody who spoke gets a portrait.
    #[test]
    fn process_and_note_rows_take_the_indent_and_no_face() {
        let theme = crate::tui::theme::Theme::dark();
        let pal = crate::tui::avatar::Palette::new(&theme);
        let pinned = std::collections::HashMap::new();
        let gutter = crate::tui::avatar::Gutter::new(false, &pal, &pinned);
        let width = gutter.width();
        for kind in [PostKind::Process, PostKind::Note] {
            let post = Post {
                from: "scout".to_string(),
                you: false,
                at: 0,
                text: "ran the tests".to_string(),
                kind,
            };
            let sender = Sender {
                gutter,
                index: gutter.index_for("scout"),
                lead: true,
            };
            let rows = settled_post_rows(&theme, &post, 60, Some(&sender));
            let text = rows[0].line.plain_text();
            assert_eq!(
                text.chars().take(width).collect::<String>(),
                " ".repeat(width),
                "{kind:?} takes the indent and no face: {text:?}"
            );
        }
    }

    /// A run is broken by somebody else speaking, and by nothing else — an
    /// agent's own tool rows are inside its turn.
    #[test]
    fn a_tool_row_does_not_break_a_senders_run() {
        let said = |from: &str, you: bool, kind: PostKind| Post {
            from: from.to_string(),
            you,
            at: 0,
            text: "x".to_string(),
            kind,
        };
        let posts = vec![
            said("scout", false, PostKind::Said),
            said("scout", false, PostKind::Process),
            said("scout", false, PostKind::Said),
            said("user", true, PostKind::Said),
            said("scout", false, PostKind::Said),
        ];
        assert_eq!(
            sender_runs(&posts),
            vec![true, false, false, true, true],
            "one face per run, and a work step is not a speaker"
        );
    }

    /// The gutter comes out of the width before anything wraps. A body wrapped
    /// at the full width and then indented would overrun the terminal by
    /// exactly the gutter — and CJK, being two cells a character, is where that
    /// shows up first.
    #[test]
    fn the_gutter_comes_out_of_the_width_before_cjk_wraps() {
        let theme = crate::tui::theme::Theme::dark();
        let pal = crate::tui::avatar::Palette::new(&theme);
        let pinned = std::collections::HashMap::new();
        let gutter = crate::tui::avatar::Gutter::new(false, &pal, &pinned);
        let post = Post {
            from: crate::channels::USER_NAME.to_string(),
            you: true,
            at: 0,
            text: "他在解析器里找到了一个真正的问题".repeat(6),
            kind: PostKind::Said,
        };
        let sender = Sender {
            gutter,
            index: gutter.index_for(crate::channels::USER_NAME),
            lead: true,
        };
        let width = 40;
        let rows = settled_post_rows(&theme, &post, width, Some(&sender));
        assert!(
            rows.len() > 1,
            "the text has to wrap for this to mean anything"
        );
        for row in &rows {
            let text = row.line.plain_text();
            assert!(
                crate::tui::line::text_width(&text) <= width,
                "a gutter row must still fit the terminal: {} cells in {width}: {text:?}",
                crate::tui::line::text_width(&text)
            );
        }
    }

    /// Where the terminal can place images the gutter cells are the portrait's
    /// own — the kitty placeholder run, with the image id in the foreground.
    /// Asserted at the sequence level, the way `avatar.rs` asserts its own.
    #[test]
    fn the_image_skin_puts_placeholder_cells_in_the_gutter() {
        let theme = crate::tui::theme::Theme::dark();
        let pal = crate::tui::avatar::Palette::new(&theme);
        let pinned = std::collections::HashMap::new();
        let gutter = crate::tui::avatar::Gutter::new(true, &pal, &pinned);
        let index = gutter.index_for("scout");
        let cells = gutter.cells(index, "scout", true);
        assert_eq!(
            cells.len(),
            crate::tui::avatar::ROWS,
            "two rows of portrait"
        );
        for (row, cell) in cells.iter().enumerate() {
            let text = cell.plain_text();
            assert!(
                text.contains(crate::tui::gfx::PLACEHOLDER),
                "row {row} is placeholder cells: {text:?}"
            );
            assert_eq!(
                crate::tui::line::text_width(&text),
                gutter.width(),
                "and it measures the gutter exactly"
            );
        }
        assert!(
            gutter.cells(index, "scout", false).is_empty(),
            "a continuation message spends no portrait"
        );
        let blank = gutter.blank().plain_text();
        assert_eq!(
            blank.trim(),
            "",
            "and the continuation gutter is blank: {blank:?}"
        );
    }
}

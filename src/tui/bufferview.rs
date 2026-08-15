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
//! append-only store holding both the hub's transcript and the rows every
//! excursion has printed. [`Chat::flow_order`] decides what that store looks
//! like on screen: hub messages up to the point where you left, then the
//! excursion's rows, and — while the excursion is still open — nothing else, so
//! a hub turn that lands while you are reading a DM does not print into the DM.
//! Coming back closes the excursion with a `── hub ──` rule, and the hub's
//! unprinted tail follows it.
//!
//! Two properties fall out of that shape, and both are why it has this shape:
//!
//! - **The order is append-only.** Once a position in the flow is printed it
//!   never moves, so the write-once flush cursor
//!   (`Chat::flushed_segments`) stays valid and scrollback is never rewritten.
//! - **There is no second renderer.** An excursion's rows are `UiMessage`s in
//!   the same list the hub's are, so `build_rows`/`assistant_el` render a
//!   replayed DM message with the code that renders a live hub reply — the
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

/// The receipt a routed submit leaves in the hub flow: `→ @scout: look at…`.
///
/// Display-only, like the dialog receipts (D80/D81): the model's history never
/// carries it, because nothing was said to the model. It exists so a line that
/// left the hub is not simply gone — without it the composer would clear and
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

/// The one line an agent's life still writes into the hub flow (D98):
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
/// after a layout at a known width, and a conversation's last thirty messages
/// is a promise that can be kept at any width.
pub const REPLAY_BUDGET: usize = 30;

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

/// What a flow position is, beyond the hub's two roles.
///
/// The hub's transcript has exactly two speakers and needs no decoration. A DM
/// or a channel has a name over each message and a rule where it begins, and
/// those are decorations of a *position in the flow* rather than facts about
/// the message — the same `UiMessage` renders undecorated in the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decor {
    /// The hub's own message: the user and the model, rendered as always.
    Hub,
    /// A rule: the one that opens a conversation, or the one that hands the
    /// flow back to the hub. The message text is the rule.
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
    fn hub(index: usize) -> Self {
        Self {
            index,
            decor: Decor::Hub,
        }
    }
}

/// One visit to a conversation other than the hub.
///
/// An excursion is a *segment* of the flow: the rows this conversation printed,
/// spliced in at the hub-message index the switch happened at. It is not a copy
/// of the conversation — the rows are indices into the one message store, and
/// the conversation itself stays in its domain store the whole time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Excursion {
    pub id: BufferId,
    /// How much of the hub had been printed when this excursion opened. Hub
    /// messages past it wait here until the excursion closes.
    pub at: usize,
    /// The rows this conversation has put on screen, in print order: the
    /// opening rule, the replay, whatever arrived while it was active, and —
    /// once it closes — the `── hub ──` rule that ends it.
    pub rows: Vec<FlowItem>,
    /// Messages the source had already produced when it was last printed, so
    /// the poll appends only what is new. Counted in posts, which is the unit
    /// the replay is built in.
    pub seen: usize,
    /// The flow has been handed back to the hub.
    pub closed: bool,
}

impl Chat {
    // -- the flow ----------------------------------------------------------

    /// The print order of [`Chat::messages`].
    ///
    /// Hub messages and excursion rows share one store, and this is the single
    /// answer to what that store looks like on screen. Walking it is linear in
    /// the store, runs once per build, and is append-only across builds: an
    /// item that has been emitted keeps its position for the rest of the
    /// session, which is what the write-once flush cursor rests on.
    pub(crate) fn flow_order(&self) -> Vec<FlowItem> {
        if self.excursions.is_empty() {
            return (0..self.messages.len()).map(FlowItem::hub).collect();
        }
        // Which indices belong to an excursion rather than to the hub. The hub
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
        let push_hub_upto = |upto: usize, cursor: &mut usize, out: &mut Vec<FlowItem>| {
            while *cursor < upto {
                if !claimed[*cursor] {
                    out.push(FlowItem::hub(*cursor));
                }
                *cursor += 1;
            }
        };
        for exc in &self.excursions {
            push_hub_upto(exc.at.min(self.messages.len()), &mut cursor, &mut out);
            out.extend(exc.rows.iter().cloned());
            // An open excursion holds the hub's tail: the messages a running
            // turn lands while you are away are the hub's news, and printing
            // them here would interleave two conversations in one flow.
            if !exc.closed {
                return out;
            }
        }
        push_hub_upto(self.messages.len(), &mut cursor, &mut out);
        out
    }

    /// The conversation the composer and the flow belong to.
    pub(crate) fn active_buffer(&self) -> BufferId {
        self.buffers.active().clone()
    }

    /// The excursion currently open, if the active conversation is not the hub.
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
        // Composer modes that mean something to the hub and nothing to a
        // conversation: `!` runs a command, and the completion surfaces are
        // about the line that was just abandoned.
        self.bash_mode = false;
        self.clear_slash_suggestions();
        self.mention = None;
        self.mention_dismissed = false;

        // Leaving a conversation closes its segment with the rule that hands
        // the flow back; the hub's own tail follows it, unprinted until now.
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

        // Where the hub had got to before any of this was appended: the point
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
    /// Never starts a hub turn: `busy` belongs to the model conversation, and a
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
            // Unreachable — only the hub routes to a turn, and the hub does not
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
        let settled = dm_posts(&history, &stamps, &[], &[], &[], &name, USER_NAME);
        let all = dm_posts(
            &history, &stamps, &in_flight, &live, &pending, &name, USER_NAME,
        );
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
            .flat_map(|(post, lead)| self.tail_post_rows(post, &name, width, gutter.as_ref(), lead))
            .collect();
        if rows.is_empty() {
            return None;
        }
        Some(El::Rows(rows))
    }

    /// The avatar gutter this view draws, or `None` where there is none.
    ///
    /// The hub is the one conversation without one: its grammar is Claude
    /// Code's — two speakers, `⏺` markers, bodies running the full width — and
    /// a portrait column beside it would be a second convention in the same
    /// window. Rooms, DMs and the perspective page are group-shaped, and there
    /// the face is what says who is talking.
    pub(crate) fn conversation_gutter<'a>(
        &'a self,
        pal: &'a crate::tui::avatar::Palette,
    ) -> Option<crate::tui::avatar::Gutter<'a>> {
        if self.active_buffer() == BufferId::Hub {
            return None;
        }
        Some(crate::tui::avatar::Gutter::new(
            self.image_cap.is_some(),
            pal,
            &self.faces_pinned,
        ))
    }

    /// The instance's live state, or `None` when the registry has never heard
    /// of it (a DM whose agent was deleted still has a conversation to read).
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
        let pending = self.session.agents.pending_of(name);
        Some((history, stamps, live, in_flight, pending))
    }

    /// One live post as rows. The vocabulary is the transcript's own: a message
    /// you sent is your bubble, a step of the agent's work is one dim line, and
    /// the wait is the same spinner the rest of the app waits with (D87
    /// `pulse`), so a DM in flight and a hub turn in flight read alike.
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

    /// A hub submit that opens with another conversation's name.
    ///
    /// `@scout look at the parser` delivers `look at the parser` to scout and
    /// leaves the flow where it is: the point is to say one thing to a teammate
    /// *without* the cost of going there and coming back, which is the whole
    /// difference between this and `ctrl+k`.
    ///
    /// **Only from the hub.** In a DM or a channel the buffer already *is* the
    /// target, so a leading `@name` there is what it looks like — a person
    /// being addressed inside a message — and treating it as an envelope would
    /// silently redirect a sentence the user meant to send where they were.
    /// The asymmetry is deliberate and it is the reason this is not a general
    /// composer feature.
    ///
    /// **Names resolve exactly.** The sigil is required and the name is matched
    /// case-sensitively against the registry, so `@unknown hi` is not an error
    /// and not magic — it is prose, and it submits to the hub verbatim. D85's
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
    /// routed from the hub is indistinguishable at the domain from one typed
    /// in the DM itself.
    pub(crate) fn route_from_hub(&mut self, id: BufferId, text: String) {
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
                });
                self.dirty = true;
            }
            // The board's refusal and a failed delivery both say what did not
            // happen, above the composer — never as a receipt, which would
            // claim something was delivered.
            Delivery::Rejected(why) => self.push_slash_info(why),
            // Unreachable: a leading name always carries a sigil, and the hub
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
                "usage: /open @agent · /open #room · /open hub · ctrl+t for the team directory"
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
    pub(crate) fn resolve_target(&self, arg: &str) -> Option<BufferId> {
        let arg = arg.trim();
        if arg.eq_ignore_ascii_case("hub") {
            return Some(BufferId::Hub);
        }
        let bare = arg.trim_start_matches(['@', '#']);
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
                    (BufferId::Hub, _) => "the conversation with the model".to_string(),
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

/// An agent's prose, rendered the way the hub renders the model's.
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

    fn hub_message(chat: &mut Chat, role: Role, text: &str) {
        chat.messages.push(UiMessage {
            role,
            text: text.to_string(),
            at: 0,
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
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
            vec![user("look at the parser"), assistant("found it")],
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
        let want: Vec<String> = dm_posts(&history, &stamps, &[], &[], &[], "scout", USER_NAME)
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
        let history: Vec<ApiMessage> = (0..REPLAY_BUDGET + 6)
            .map(|i| assistant(&format!("message {i}")))
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

        chat.set_input("half a hub thought");
        chat.switch_to(BufferId::Dm("scout".to_string()));
        assert_eq!(chat.input, "", "the DM starts empty");

        chat.set_input("half a scout thought");
        chat.switch_to(BufferId::Hub);
        assert_eq!(chat.input, "half a hub thought", "the hub's draft is back");

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

        // The hub is not special: re-entering it from the hub is the same no-op.
        chat.switch_to(BufferId::Hub);
        let back = chat.messages.len();
        chat.switch_to(BufferId::Hub);
        assert_eq!(chat.messages.len(), back);
    }

    // -- the excursion -----------------------------------------------------

    /// The ruling's central promise: while you are in a conversation, nothing
    /// else prints into it. A hub turn that lands while you are away is the
    /// hub's news, and it waits at the hub for you.
    #[test]
    fn an_excursion_holds_the_hubs_tail_until_you_come_back() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", vec![assistant("on it")]);
        chat.refresh_conversations();
        hub_message(&mut chat, Role::Assistant, "before you left");

        chat.switch_to(BufferId::Dm("scout".to_string()));
        // A hub turn completes while the DM is on screen.
        hub_message(&mut chat, Role::Assistant, "landed while you were away");

        let away = flow(&mut chat);
        assert!(away.contains("before you left"), "{away}");
        assert!(away.contains("── @scout ──"), "{away}");
        assert!(away.contains("on it"), "{away}");
        assert!(
            !away.contains("landed while you were away"),
            "the hub printed into the DM: {away}"
        );

        chat.switch_to(BufferId::Hub);
        let home = flow(&mut chat);
        assert!(
            home.contains("── hub ──"),
            "the rule hands the flow back: {home}"
        );
        assert!(
            home.contains("landed while you were away"),
            "the hub's tail follows it: {home}"
        );
        // Order: the DM segment closes before the hub's tail resumes.
        let rule = home.find("── hub ──").expect("the closing rule");
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
        hub_message(&mut chat, Role::Assistant, "hub one");

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
        hub_message(&mut chat, Role::Assistant, "hub two");
        step(&chat, &mut seen);
        chat.switch_to(BufferId::Hub);
        step(&chat, &mut seen);
        hub_message(&mut chat, Role::Assistant, "hub three");
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
        assert!(!chat.busy, "no hub turn was started");
        assert!(chat.input.is_empty(), "the composer cleared");
        assert!(
            chat.queued.is_empty(),
            "and it did not queue behind the hub"
        );

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
        assert!(!chat.busy, "no hub turn was started");

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
        // destination must not depend on how you arrived (D89's BackToHub). The
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

        chat.session
            .agents
            .finish("scout", vec![assistant("the parser is fine")], 0);
        chat.session
            .agents
            .finish("zoe", vec![assistant("nobody is reading this")], 0);
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
        assert_eq!(zoe.unread(), 1, "it counted instead");
        assert!(zoe.mention(), "and a DM always wants you");
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

    // -- the team feed -----------------------------------------------------

    /// `/team` answers into the team's feed, and the hub keeps one line saying
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
            "and the hub points at the key that opens it: {:?}",
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
    async fn a_leading_name_delivers_from_the_hub_without_moving() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.refresh_conversations();

        chat.set_input("@scout have a look at the parser");
        chat.submit();

        assert!(!chat.busy, "a delivery is not a turn");
        assert_eq!(*chat.buffers.active(), BufferId::Hub, "the flow stayed put");
        assert!(
            chat.queued.is_empty(),
            "and it did not queue behind the hub"
        );

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
    /// prose submits to the hub exactly as typed.
    #[tokio::test]
    async fn an_unknown_name_is_just_prose() {
        let mut chat = test_chat();
        chat.refresh_conversations();

        chat.set_input("@nobody are you there");
        chat.submit();

        assert!(chat.busy, "it opened an ordinary hub turn");
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
    /// mentioned, and it belongs to the hub like any other sentence.
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
        chat.run_slash("open hub");
        assert_eq!(*chat.buffers.active(), BufferId::Hub);
        // The sigil is an accepted spelling, not a requirement.
        chat.run_slash("open scout");
        assert_eq!(*chat.buffers.active(), BufferId::Dm("scout".to_string()));

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
        assert!(offered.contains(&"hub".to_string()), "{offered:?}");
        assert!(offered.contains(&"@scout".to_string()), "{offered:?}");
        assert!(offered.contains(&"#build".to_string()), "{offered:?}");

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
                user("look at the parser"),
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

    /// The hub keeps Claude Code's grammar: two speakers, no portrait column.
    /// A gutter there would be a second convention in the same window.
    #[test]
    fn the_hub_flow_wears_no_gutter() {
        let mut chat = test_chat();
        hub_message(&mut chat, Role::Assistant, "hub prose");
        let rows = raw_rows(&mut chat);
        let row = row_with(&rows, "hub prose");
        assert!(
            !row.starts_with("    "),
            "the hub's body is not indented into a gutter: {row:?}"
        );
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

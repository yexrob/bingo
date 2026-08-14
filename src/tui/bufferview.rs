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

/// How many messages a switch replays. Messages, not rows: rows exist only
/// after a layout at a known width, and a conversation's last thirty messages
/// is a promise that can be kept at any width.
pub const REPLAY_BUDGET: usize = 30;

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
        self.dirty = true;
    }

    /// Open a segment for a conversation: the rule, then its recent history.
    fn open_conversation(&mut self, id: &BufferId) {
        let session = self.session.clone();
        // Everything the source has, so `seen` is the true total and the poll
        // that follows appends only what arrives after this moment; the budget
        // decides how much of it goes on screen, not how much is counted.
        let all = self.buffers.rehydrate(&session, id, usize::MAX);
        let messages: Vec<&Replay> = all
            .iter()
            .filter(|replay| matches!(replay, Replay::Message { .. }))
            .collect();
        let seen = messages.len();
        let start = seen.saturating_sub(REPLAY_BUDGET);
        let shown: Vec<(String, UiMessage)> = messages[start..]
            .iter()
            .filter_map(|replay| match replay {
                Replay::Message { who, message } => Some((who.clone(), message.clone())),
                Replay::Divider(_) => None,
            })
            .collect();

        // Where the hub had got to before any of this was appended: the point
        // its unprinted tail resumes from when the excursion closes.
        let at = self.messages.len();
        // The rule comes from the replay rather than being formatted again
        // here: one shape, decided in one place.
        let rule = all
            .iter()
            .find_map(|replay| match replay {
                Replay::Divider(text) => Some(text.clone()),
                Replay::Message { .. } => None,
            })
            .unwrap_or_else(|| id.rule());
        let mut rows = vec![self.push_flow_divider(rule)];
        for (who, message) in shown {
            rows.push(self.push_flow_message(who, message));
        }
        self.excursions.push(Excursion {
            id: id.clone(),
            at,
            rows,
            seen,
            closed: false,
        });
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
        let fresh: Vec<(String, UiMessage)> = all
            .iter()
            .filter_map(|replay| match replay {
                Replay::Message { who, message } => Some((who.clone(), message.clone())),
                Replay::Divider(_) => None,
            })
            .collect();
        let seen = self.open_excursion().map(|exc| exc.seen).unwrap_or(0);
        if fresh.len() <= seen {
            return;
        }
        let mut rows = Vec::new();
        for (who, message) in fresh.into_iter().skip(seen) {
            rows.push(self.push_flow_message(who, message));
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
        let target = self.buffers.route_submit(&id, &text);
        match crate::tui::buffer::deliver(&self.session, target) {
            Delivery::Sent => {
                self.poll_active_conversation();
                self.dirty = true;
            }
            // The board's refusal and a delivery failure read the same way:
            // information about what did not happen, above the composer.
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
    pub(crate) fn conversation_tail_el(&self, width: usize) -> Option<El> {
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
        let rows: Vec<Row> = all[settled.len()..]
            .iter()
            .flat_map(|post| self.tail_post_rows(post, &name, width))
            .collect();
        if rows.is_empty() {
            return None;
        }
        Some(El::Rows(rows))
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
    fn tail_post_rows(&self, post: &Post, who: &str, width: usize) -> Vec<Row> {
        let theme = &self.theme;
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
                vec![Row::new(Line::styled(
                    one_line(&format!("{glyph} {who} is replying…{rate}"), width),
                    SegStyle::fg(theme.claude),
                ))]
            }
            // Sent, not yet claimed by a run: the agent has it, the turn has
            // not started. Dim, so it reads as in transit rather than answered.
            PostKind::Queued => wrap_words(&post.text, width)
                .into_iter()
                .map(|line| Row::new(Line::styled(line, SegStyle::fg(theme.inactive))))
                .collect(),
            // One dim line per work step, like the transcript's tool rows: cut,
            // not wrapped, so a long command stays one row.
            PostKind::Process => vec![Row::new(Line::styled(
                one_line(&post.text, width),
                SegStyle::fg(theme.inactive),
            ))],
            _ if post.you => user_message_rows(&post.text, width, theme),
            _ => self.agent_text_rows(&post.text, width),
        }
    }

    /// A subagent's prose, rendered the way the hub renders the model's.
    ///
    /// Uncached on purpose: this is the streaming tail, its text changes on
    /// almost every frame, and a cache keyed on text that never repeats is a
    /// leak with extra steps. The settled copy that lands in the flow goes
    /// through the transcript's own cached path.
    fn agent_text_rows(&self, text: &str, width: usize) -> Vec<Row> {
        let mut processor = MarkdownProcessor::default();
        let mut renderer =
            MarkdownRenderer::with_theme(width.saturating_sub(2), self.theme.clone());
        let doc = processor.process_streaming(text);
        renderer.render(&doc);
        text_rows(&self.theme, renderer.lines().to_vec())
    }

    // -- /open -------------------------------------------------------------

    /// `/open <target>`: the interim door to a conversation until D90 draws the
    /// bar and binds `ctrl+k`.
    pub(crate) fn slash_open(&mut self, arg: &str) {
        let arg = arg.trim();
        if arg.is_empty() {
            self.push_slash_info(
                "usage: /open @agent · /open #channel · /open #team · /open hub".to_string(),
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
        if arg == "#team" || arg.eq_ignore_ascii_case("team") {
            return self
                .buffers
                .get(&BufferId::Team)
                .map(|buffer| buffer.id().clone());
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
                    (BufferId::Team, _) => "subagent lifecycle, read-only".to_string(),
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
        chat.refresh_entities();
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
        chat.refresh_entities();
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
        chat.refresh_entities();

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
        chat.refresh_entities();
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
        chat.refresh_entities();
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
        chat.refresh_entities();
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
        chat.refresh_entities();
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
            .create("build", vec!["scout".to_string()], ChannelMode::Free)
            .expect("channel created");
        chat.refresh_entities();
        chat.switch_to(BufferId::Channel("build".to_string()));

        chat.set_input("ship it");
        chat.submit();
        assert!(!chat.busy, "no hub turn was started");

        let log = chat.session.channels.log_of("build");
        assert_eq!(log.len(), 1, "one post: {log:?}");
        assert_eq!(log[0].from, USER_NAME);
        assert!(flow(&mut chat).contains("ship it"), "and it is on screen");
    }

    /// The board is a record of what happened. Refusing to be spoken in is
    /// information, so it lands in the info tier rather than as an error.
    #[tokio::test]
    async fn the_board_refuses_to_be_spoken_in() {
        let mut chat = test_chat();
        chat.buffers.note_watch_event(
            "scout #1 · go",
            crate::watch::WatchKind::Agent,
            crate::watch::WatchState::Running,
            None,
            1,
        );
        chat.switch_to(BufferId::Team);

        chat.set_input("nice work everyone");
        chat.submit();
        assert!(!chat.busy);
        assert!(
            chat.slash_info_lines
                .iter()
                .any(|line| line.contains("board")),
            "the refusal is said out loud: {:?}",
            chat.slash_info_lines
        );
        assert!(
            chat.slash_error_lines.is_empty(),
            "and it is not an error: nothing failed"
        );
    }

    /// Slash commands act on the application, so they mean the same thing in
    /// every conversation — including the one where the composer is otherwise
    /// speaking to a subagent.
    #[test]
    fn a_slash_command_still_reaches_the_app_from_a_conversation() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.refresh_entities();
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
        chat.refresh_entities();
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
        chat.refresh_entities();
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

    // -- /open -------------------------------------------------------------

    /// `/open` reaches every kind of conversation, with or without its sigil,
    /// and says so when it cannot.
    #[test]
    fn open_reaches_every_conversation_and_reports_the_ones_it_cannot() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.session
            .channels
            .create("build", vec!["scout".to_string()], ChannelMode::Free)
            .expect("channel created");
        chat.buffers.note_watch_event(
            "scout #1 · go",
            crate::watch::WatchKind::Agent,
            crate::watch::WatchState::Running,
            None,
            1,
        );
        chat.refresh_entities();

        chat.run_slash("open @scout");
        assert_eq!(*chat.buffers.active(), BufferId::Dm("scout".to_string()));
        chat.run_slash("open #build");
        assert_eq!(
            *chat.buffers.active(),
            BufferId::Channel("build".to_string())
        );
        chat.run_slash("open #team");
        assert_eq!(*chat.buffers.active(), BufferId::Team);
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
            .create("build", vec!["scout".to_string()], ChannelMode::Free)
            .expect("channel created");
        chat.refresh_entities();

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
}

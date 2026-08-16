//! The conversations that are not the transcript (D103): how their posts are
//! drawn, the one line their failures write into `@main`, and the one way the
//! composer speaks to one of them without leaving the transcript.
//!
//! D89 made every conversation a *place* the terminal could be pointed at, and
//! D90 gave the places a bar. D103 retires both on the CC-parity ruling: there
//! is one transcript — the user's conversation with main — and it is the only
//! thing the flow prints. Nothing switches, nothing splices, nothing replays.
//! What survives is what a conversation *is* to a reader who is not standing in
//! it:
//!
//! - **A post as rows** ([`settled_post_rows`], [`sender_runs`]). One renderer,
//!   so a message on the observation page (D96/D100) is drawn by the code that
//!   drew it in the pair view — the markdown, the bubbles, the gutter and the
//!   CJK wrapping are the same by construction rather than by imitation.
//! - **The failure alert** ([`agent_alert_line`]). A crash cannot wait for a
//!   narration that may never run.
//! - **The completion notice** ([`agent_notice_line`], D106). One dim `●` line
//!   where the task notification of a finished run reaches main's context,
//!   before main says anything about it.
//! - **The teammate line** ([`teammate_line`], D106). One line for a message an
//!   agent sent main, in the sender's colour, its body kept for `ctrl+o`.
//! - **The direct send** ([`Chat::parse_direct_send`]). A composer line shaped
//!   `@scout fix the lexer` or `#build tests are green` bypasses the model and
//!   goes straight to that inbox or that room, under the user's own name, with
//!   a transient receipt and nothing in main's history.
//!
//! The live tail of one agent's turn is kept here unused: D105's zoomed view is
//! the surface that draws it, and it is the same machinery, not a second one.

use rsmarkdown_core::{MarkdownProcessor, Renderer};

use crate::channels::USER_NAME;
use crate::tui::buffer::{Delivery, Post, PostKind, SubmitTarget};
use crate::tui::chat::{Chat, Row, one_line, text_rows, user_message_rows};
use crate::tui::line::{Line, SegStyle, wrap_words};
use crate::tui::markdown::MarkdownRenderer;

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

/// The line a task notification leaves when it reaches main's context (D106):
/// `● @scout completed · fix the parser`.
///
/// CC prints `<BLACK_CIRCLE> <summary>` for exactly this event
/// (`components/messages/UserAgentNotificationMessage.tsx:55-81`, over the
/// `<summary>` its `LocalAgentTask` writes at `LocalAgentTask.tsx:246`:
/// `Agent "<description>" completed`). Two departures, both deliberate:
/// `BLACK_CIRCLE` is `⏺` on macOS and `●` elsewhere
/// (`constants/figures.ts:4`) and bingo already spends `⏺` on tool rows and on
/// main's own prose, so the other of CC's two glyphs is the one that does not
/// collide; and the summary names the **instance**, because bingo's agents are
/// addressable and `@scout` is what the reader would type next.
pub(crate) const AGENT_NOTICE_PREFIX: &str = "● ";

pub(crate) fn is_agent_notice(text: &str) -> bool {
    text.starts_with(AGENT_NOTICE_PREFIX)
}

/// `● @scout completed · fix the parser`, from a run's watch label.
pub(crate) fn agent_notice_line(label: &str) -> String {
    let instance = crate::tui::activities::watch_instance(label);
    let description = crate::tui::activities::watch_description(label);
    if description.is_empty() {
        format!("{AGENT_NOTICE_PREFIX}@{instance} completed")
    } else {
        format!("{AGENT_NOTICE_PREFIX}@{instance} completed · {description}")
    }
}

/// How much of a message the transcript's one-line form shows — CC's own
/// fallback when a sender left the `summary` field off
/// (`tools/SendMessageTool/SendMessageTool.ts:765`: `truncate(input.message,
/// 50)`). bingo's `SendMessage` has no `summary` field to prefer, so this is
/// always the path taken.
pub(crate) const TEAMMATE_SUMMARY_WIDTH: usize = 50;

/// The pointer that closes the address on a teammate's transcript line —
/// `figures.pointer`, which is what CC writes after `@name`
/// (`components/messages/UserTeammateMessage.tsx:159`).
pub(crate) const TEAMMATE_POINTER: char = '❯';

/// One line for a message an agent sent `main` (D106): `@scout❯ found it` in
/// the sender's identity colour, with the whole body underneath **only** in the
/// `ctrl+o` transcript — which is CC's `isTranscriptMode` gate, letter for
/// letter (`UserTeammateMessage.tsx:186`).
///
/// v3 (D98) rendered nothing at all here and let the wake speak for itself.
/// v4 restores CC's line: the wake and its debounce are unchanged, only the
/// screen is.
pub(crate) fn teammate_line(from: &str, text: &str) -> String {
    format!(
        "@{from}{TEAMMATE_POINTER} {}\n{text}",
        one_line(text, TEAMMATE_SUMMARY_WIDTH)
    )
}

/// Who a teammate line addresses, and what it summarises — `None` for anything
/// that is not one. The shape is `@name❯ `, with the name a plain identifier,
/// so an ordinary message that happens to open with an `@` cannot be mistaken
/// for one (the same textual convention [`is_agent_alert`] has carried since
/// D98).
pub(crate) fn parse_teammate_line(text: &str) -> Option<(&str, &str, &str)> {
    let head = text.lines().next()?;
    let rest = head.strip_prefix('@')?;
    let (name, tail) = rest.split_once(TEAMMATE_POINTER)?;
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
    {
        return None;
    }
    let body = text.split_once('\n').map(|(_, b)| b).unwrap_or("");
    Some((name, tail.trim_start(), body))
}

pub(crate) fn is_teammate_line(text: &str) -> bool {
    parse_teammate_line(text).is_some()
}

/// Who a composer line addresses when it opens with a sigil (D103).
///
/// CC's `parseDirectMemberMessage` (2.1.88 `utils/directMemberMessage.ts`) is
/// the shape: a bare `@name`, whitespace, and a non-empty rest. bingo adds the
/// room half, because a room is the one thing it has that CC does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectTarget {
    /// An instance's inbox, written to as the user.
    Agent(String),
    /// A room's log, posted to as the user (joining first if need be).
    Room(String),
}

impl DirectTarget {
    /// The address as it is written: the sigil is part of the name.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Agent(name) => format!("@{name}"),
            Self::Room(name) => format!("#{name}"),
        }
    }
}

impl Chat {
    /// The rows a line about somebody else's life takes in the flow (D106) —
    /// `None` for anything the ordinary user-message renderer should draw.
    ///
    /// Both shapes are one row: what a reader needs from an arriving message is
    /// who and roughly what, and the transcript is one keystroke away. That
    /// second row exists — CC hangs the whole body under the summary in
    /// transcript mode (`UserTeammateMessage.tsx:186`) — and so does bingo's,
    /// through [`Chat::transcript_mode`].
    pub(crate) fn agent_flow_rows(&self, text: &str, width: usize) -> Option<Vec<Row>> {
        let theme = &self.theme;
        if is_agent_notice(text) {
            let mut line = Line::styled(AGENT_NOTICE_PREFIX.to_string(), theme.tool_done());
            line.push_styled(
                one_line(text.trim_start_matches(AGENT_NOTICE_PREFIX), width),
                theme.dim(),
            );
            return Some(vec![Row::new(line)]);
        }
        let (from, summary, body) = parse_teammate_line(text)?;
        let mut line = Line::styled(
            format!("@{from}{TEAMMATE_POINTER}"),
            SegStyle::fg(self.identity_color(from)),
        );
        if !summary.is_empty() {
            line.push_styled(format!(" {summary}"), SegStyle::fg(theme.text));
        }
        let mut rows = vec![Row::new(line)];
        if self.transcript_mode && !body.trim().is_empty() {
            for wrapped in wrap_words(body, width.saturating_sub(2).max(1)) {
                rows.push(Row::new(Line::styled(format!("  {wrapped}"), theme.dim())));
            }
        }
        Some(rows)
    }

    /// A composer line that is a direct send, or `None` when it is a prompt.
    ///
    /// `@scout fix the lexer too` reaches scout without the model ever seeing
    /// it; `#build tests are green` reaches the room the same way. Everything
    /// else — including `@scout` on its own, which addresses somebody and says
    /// nothing — is an ordinary turn to main.
    ///
    /// **An unresolved name is prose.** `@utils explain this code` is a
    /// question about a directory, not a failed delivery, and CC settles it the
    /// same way: `sendDirectMemberMessage` answers `unknown_recipient` and the
    /// caller falls through to a normal prompt rather than raising an error.
    /// The typeahead is where discovery belongs; the parser refuses to guess.
    ///
    /// **The whitespace may be a newline**, matching CC's `/^@([\w-]+)\s+(.+)$/s`
    /// — a pasted block under a name is still a message to that name.
    pub(crate) fn parse_direct_send(&self, text: &str) -> Option<(DirectTarget, String)> {
        let sigil = text.chars().next()?;
        if sigil != '@' && sigil != '#' {
            return None;
        }
        let cut = text.find(char::is_whitespace)?;
        let name = text.get(sigil.len_utf8()..cut)?;
        if name.is_empty() {
            return None;
        }
        let body = text[cut..].trim();
        if body.is_empty() {
            return None;
        }
        // Resolved against the domain registries rather than against the
        // accounting store: the store is refreshed on a poll, and an agent
        // spawned two frames ago is already addressable.
        let known = match sigil {
            '@' => self
                .session
                .agents
                .list()
                .iter()
                .any(|status| status.name == name),
            _ => self
                .session
                .channels
                .list()
                .iter()
                .any(|status| status.name == name),
        };
        if !known {
            return None;
        }
        let target = match sigil {
            '@' => DirectTarget::Agent(name.to_string()),
            _ => DirectTarget::Room(name.to_string()),
        };
        Some((target, body.to_string()))
    }

    /// Perform a direct send. The delivery and nothing else.
    ///
    /// A room the user is not in **joins first**. Speaking is participation and
    /// participation is announced — the domain writes the membership line into
    /// the room's own log, so every member sees the same arrival the joiner
    /// does. That is the v3 ruling, and it is what keeps reading a room free.
    /// One join path, so a zoom and a `#room` line announce identically.
    pub(crate) fn deliver_direct(&mut self, target: &DirectTarget, text: String) -> Delivery {
        let submit = match target {
            DirectTarget::Agent(name) => SubmitTarget::Dm {
                agent: name.clone(),
                text,
            },
            DirectTarget::Room(name) => {
                if !self.session.channels.is_member(name, USER_NAME)
                    && let Err(why) = self.session.channels.invite(name, USER_NAME)
                {
                    return Delivery::Rejected(why);
                }
                SubmitTarget::Channel {
                    channel: name.clone(),
                    text,
                }
            }
        };
        let outcome = crate::tui::buffer::deliver(&self.session, submit);
        if outcome == Delivery::Sent {
            self.refresh_conversations();
        }
        outcome
    }

    /// A direct send from the transcript, with its receipt.
    ///
    /// The receipt is **transient** and lives on the info tier: nothing was said
    /// to the model, so nothing belongs in main's history, and a flow line
    /// would put an envelope in the user's mouth for the rest of the session.
    /// CC does the same thing with a 3s notification (`Sent to @scout`).
    ///
    /// It is the *transcript's* receipt and not the delivery's, which is why it
    /// sits out here: the zoomed view sends down the same path and needs none,
    /// because the message it just sent is drawn on the screen it was sent from
    /// (D105).
    pub(crate) fn direct_send(&mut self, target: DirectTarget, text: String) {
        let receipt = format!("Sent to {}", target.label());
        match self.deliver_direct(&target, text) {
            Delivery::Sent => self.push_slash_info(receipt),
            // A refusal says what did not happen, on the same tier and never as
            // a receipt — a receipt claims something was delivered.
            Delivery::Rejected(why) => self.push_slash_info(why),
        }
    }

    /// The avatar gutter this view draws.
    ///
    /// Every conversation has one since D99, @main included: main is a
    /// participant like the rest, and a face is how a participant is
    /// recognised. One value, so the flow, the zoomed view and the perspective
    /// page cannot drift on width, on who wears what, or on which skin the
    /// terminal is in.
    pub(crate) fn conversation_gutter<'a>(
        &'a self,
        pal: &'a crate::tui::avatar::Palette,
    ) -> crate::tui::avatar::Gutter<'a> {
        crate::tui::avatar::Gutter::new(self.image_cap.is_some(), pal, &self.faces_pinned)
    }

    /// One post of a zoomed conversation as rows (D105). The vocabulary is the
    /// transcript's own: a message somebody sent is a bubble or prose under
    /// their portrait, a step of the agent's work is one dim line, and the wait
    /// is the same spinner the rest of the app waits with (D87 `pulse`), so a
    /// reply in flight here and a main turn in flight read alike.
    ///
    /// The two live-only kinds are the reason this is not just
    /// [`settled_post_rows`]: they need the running instance's clock and its
    /// colour, which a stored post does not have.
    pub(crate) fn zoom_post_rows(
        &self,
        post: &Post,
        who: &str,
        width: usize,
        gutter: &crate::tui::avatar::Gutter<'_>,
        lead: bool,
    ) -> Vec<Row> {
        let theme = &self.theme;
        // The two live-only kinds are states, not messages: they get the
        // indentation so the column does not jog, and no face, because nobody
        // has said anything yet.
        let indent = |rows: &mut Vec<Row>| {
            gutter.apply(rows, gutter.index_for(who), who, false);
        };
        let inner = width.saturating_sub(gutter.width());
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
                // The wait wears the agent's own colour: the composer above it
                // does too while a zoom is open, and one identity should look
                // like one identity on both rows.
                let palette = crate::tui::avatar::Palette::new(theme);
                let identity = palette.avatars[gutter.index_for(who) % palette.avatars.len()];
                let mut rows = vec![Row::new(Line::styled(
                    one_line(&format!("{glyph} {who} is replying…{rate}"), inner),
                    SegStyle::fg(identity),
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
            _ => settled_post_rows(
                theme,
                post,
                width,
                Some(&Sender {
                    gutter: *gutter,
                    index: gutter.index_for(&post.from),
                    lead,
                }),
            ),
        }
    }

    /// Which room a `/join` or `/leave` is about. Named outright since D103:
    /// there is no room to be standing in any more, so the argument that used
    /// to be optional is the only thing that can say which one.
    fn room_arg(&self, arg: &str) -> Option<String> {
        let named = arg.trim().trim_start_matches('#').trim();
        (!named.is_empty()).then(|| named.to_string())
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
                "usage: /join #room — the room typeahead lists what there is".to_string(),
            );
            return;
        };
        match self.session.channels.invite(&room, USER_NAME) {
            Ok(()) => {
                self.refresh_conversations();
                self.push_slash_info(format!("joined #{room}"));
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
                self.push_slash_info(format!("left #{room}"));
            }
            Err(why) => self.push_slash_info(why),
        }
    }
}

/// A *settled* post as rows — the shapes any stored conversation can contain:
/// a message somebody sent, a line nobody said, a step of an agent's work.
///
/// Split out of the live tail for D96: the perspective page renders the same
/// posts with no instance behind them, and a second renderer beside this one is
/// exactly the thing the flow has avoided since D89. The two live-only kinds
/// stay with the host, because they need the running instance's clock and
/// colour: [`PostKind::Typing`] and [`PostKind::Queued`].
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
    use crate::tui::chat::{Role, UiMessage};
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

    /// The teammate line is recognised by its shape and only its shape: an `@`,
    /// a plain name, the pointer. Everything else is an ordinary message, which
    /// matters because the user's own submissions arrive in the same store —
    /// the same textual convention `is_agent_alert` has carried since D98.
    #[test]
    fn a_teammate_line_is_recognised_by_its_shape_alone() {
        let line = teammate_line("scout", "found it in the lexer\nsecond paragraph");
        assert_eq!(
            parse_teammate_line(&line),
            Some((
                "scout",
                "found it in the lexer second paragraph",
                "found it in the lexer\nsecond paragraph"
            )),
            "the summary is one line and the body is whole"
        );

        for prose in [
            "@scout fix the lexer",
            "@ ❯ hi",
            "@src/lexer.rs❯ why",
            "look at @scout❯ that",
            "❯ scout",
            "",
        ] {
            assert!(
                parse_teammate_line(prose).is_none(),
                "ordinary text: {prose:?}"
            );
        }
    }

    /// The notice line and the alert line are different tiers of news and are
    /// never each other.
    #[test]
    fn a_completion_notice_names_the_instance_and_its_task() {
        assert_eq!(
            agent_notice_line("scout · fix the parser"),
            "● @scout completed · fix the parser"
        );
        assert_eq!(
            agent_notice_line("scout #7 receipt"),
            "● @scout completed",
            "a label with no description says nothing rather than repeating the name"
        );
        assert!(is_agent_notice(&agent_notice_line("scout · x")));
        assert!(!is_agent_alert(&agent_notice_line("scout · x")));
        assert!(!is_agent_notice(&agent_alert_line("scout", Some("boom"))));
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

    fn main_message(chat: &mut Chat, role: Role, text: &str) {
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

    /// `/team` answers on the info tier, and only there.
    ///
    /// Rewritten twice, each time onto what survived. D95 sent the answer to
    /// the team feed alone and printed a pointer at the key that opened it;
    /// D104 took that key away and brought the lines back to the tier every
    /// other slash command answers on, keeping the feed copy for the dialog
    /// that was going to show it. D107's dialog does not: CC's has no
    /// recent-events column, so the feed retired with the directory column
    /// that was its only reader. What is asserted is what is left — the whole
    /// answer, on the tier, and nothing of it in the transcript.
    #[test]
    fn team_output_lands_on_the_info_tier() {
        let mut chat = test_chat();
        chat.session.agents.insert(
            "dev",
            AgentKind::Crew,
            None,
            "crew member".to_string(),
            chat.session.clone(),
        );
        chat.refresh_conversations();
        chat.run_slash("team");

        let answer = crate::team_cmd::run(&chat.session, &std::path::PathBuf::from(&chat.cwd), "");
        assert!(!chat.slash_info_lines.is_empty(), "the command answers");
        assert_eq!(
            chat.slash_info_lines, answer,
            "the tier carries every line the command produced, in order"
        );
        assert!(
            !chat
                .messages
                .iter()
                .any(|message| message.text.contains("/team")),
            "on the info tier, never as a message in the transcript"
        );
    }

    // -- the direct send (D103) --------------------------------------------

    /// The central promise: the message reaches the teammate's inbox as the
    /// user, the model never sees it, and main's history is untouched. The
    /// domain assertion is the one v3's DM submit made, because it has to be
    /// the same delivery.
    #[tokio::test]
    async fn a_direct_send_reaches_the_inbox_and_says_nothing_in_main() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.refresh_conversations();
        let before = chat.messages.len();

        chat.set_input("@scout have a look at the parser");
        chat.submit();

        assert!(!chat.busy, "a direct send is not a turn");
        assert!(chat.queued.is_empty(), "and it did not queue behind main");
        assert_eq!(
            chat.messages.len(),
            before,
            "nothing was written into main's history"
        );

        let items = chat.session.agents.take_running("scout", 0);
        let (prompt, _) = crate::tool::agent::absorb_inbox(&chat.session.channels, "scout", &items);
        assert_eq!(
            prompt,
            format!(
                "{}\nhave a look at the parser",
                crate::tool::agent::DM_FROM_USER_MARKER
            ),
            "delivered under the user's name, with the D64 marker applied downstream"
        );
    }

    /// The receipt is transient: the info tier, gone at the next keystroke, and
    /// never a flow message. CC does the same with a 3s notification.
    #[tokio::test]
    async fn the_receipt_is_transient_and_never_a_flow_line() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.refresh_conversations();

        chat.set_input("@scout have a look");
        chat.submit();

        assert_eq!(chat.slash_info_lines, vec!["Sent to @scout".to_string()]);
        assert!(
            chat.messages.iter().all(|m| !m.text.contains("Sent to")),
            "the receipt is not a message in the flow, so it never settles into \
             scrollback and never reaches the model's history"
        );
        assert!(chat.input.is_empty(), "and the composer cleared");

        // The info tier's own rule (D80): it lives until the user acts.
        chat.set_input("n");
        chat.after_edit();
        assert!(
            chat.slash_info_lines.is_empty(),
            "and it is gone the moment they type again"
        );
    }

    /// A room is the same rule with the other sigil, and speaking in one you
    /// are not in joins you first — announced in the room's own log, which is
    /// what keeps reading free.
    #[tokio::test]
    async fn a_room_send_posts_and_joins_first_when_it_has_to() {
        let mut chat = test_chat();
        chat.session
            .channels
            .create("build", vec!["scout".to_string()], ChannelMode::Free)
            .expect("channel created");
        chat.refresh_conversations();
        assert!(
            !chat.session.channels.is_member("build", USER_NAME),
            "the user starts outside the room"
        );

        chat.set_input("#build tests are green");
        chat.submit();

        assert!(!chat.busy);
        assert!(
            chat.session.channels.is_member("build", USER_NAME),
            "speaking made them a member"
        );
        assert_eq!(chat.slash_info_lines, vec!["Sent to #build".to_string()]);
        let log = chat.session.channels.log_of("build");
        let texts: Vec<&str> = log.iter().map(|m| m.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| t.contains("joined")),
            "the membership line is in the room's log: {texts:?}"
        );
        let post = log.last().expect("the post");
        assert_eq!(post.from, USER_NAME);
        assert_eq!(post.text, "tests are green");
    }

    /// No magic and no error: a name that resolves to nothing is prose, and
    /// prose submits to main exactly as typed. CC settles `@utils explain this`
    /// the same way — `unknown_recipient` falls through to a normal prompt.
    #[tokio::test]
    async fn an_unknown_name_falls_through_to_a_normal_turn() {
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
            chat.slash_error_lines.is_empty() && chat.slash_info_lines.is_empty(),
            "nothing failed, so nothing is reported"
        );
    }

    /// The parser's own edges, stated once. A bare name is somebody being
    /// mentioned, not an envelope; the sigil is required; the name resolves
    /// exactly; and the whitespace may be a newline, which is CC's `/s` flag —
    /// a pasted block under a name is still a message to that name.
    #[test]
    fn the_direct_send_grammar_is_a_name_a_space_and_a_message() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.session
            .channels
            .create("build", vec![USER_NAME.to_string()], ChannelMode::Free)
            .expect("channel created");
        chat.refresh_conversations();

        assert_eq!(chat.parse_direct_send("@scout"), None);
        assert_eq!(chat.parse_direct_send("@scout   "), None);
        assert_eq!(
            chat.parse_direct_send("scout hello"),
            None,
            "the sigil is required"
        );
        assert_eq!(
            chat.parse_direct_send("@Scout hello"),
            None,
            "and the name resolves exactly, not case-insensitively"
        );
        assert_eq!(
            chat.parse_direct_send("@main hello"),
            None,
            "main is who you are already talking to, so it is never an envelope"
        );
        assert_eq!(
            chat.parse_direct_send("@scout hello"),
            Some((
                DirectTarget::Agent("scout".to_string()),
                "hello".to_string()
            ))
        );
        assert_eq!(
            chat.parse_direct_send("@scout\nlook at this\nand this"),
            Some((
                DirectTarget::Agent("scout".to_string()),
                "look at this\nand this".to_string()
            )),
            "a newline is whitespace, as in CC's own regex"
        );
        assert_eq!(
            chat.parse_direct_send("#build ship it"),
            Some((
                DirectTarget::Room("build".to_string()),
                "ship it".to_string()
            ))
        );
        assert_eq!(
            chat.parse_direct_send("#nowhere ship it"),
            None,
            "an unknown room is prose too, symmetrically"
        );
    }

    /// A slash command is a slash command wherever it is typed: `/` decides
    /// what a line *is* before its first word decides where it goes.
    #[tokio::test]
    async fn a_slash_line_is_never_a_direct_send() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", Vec::new());
        chat.refresh_conversations();

        chat.set_input("/status @scout");
        chat.submit();

        assert!(
            chat.session.agents.pending_of("scout").is_empty(),
            "scout heard nothing"
        );
    }

    /// The flow is main's transcript in order, and there is no divider
    /// machinery left to splice anything else into it.
    #[test]
    fn the_flow_is_mains_own_messages_in_order() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", vec![user("a task"), assistant("done")]);
        chat.refresh_conversations();
        main_message(&mut chat, Role::User, "first");
        main_message(&mut chat, Role::Assistant, "second");

        let text = flow(&mut chat);
        let first = text.find("first").expect("the user's message");
        let second = text.find("second").expect("main's reply");
        assert!(first < second, "in order: {text}");
        assert!(
            !text.contains("── "),
            "and nothing draws a conversation rule any more: {text}"
        );
        assert!(
            !text.contains("done"),
            "an agent's own record is not spliced into the transcript: {text}"
        );
    }

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

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
//! - **The failure alert** ([`agent_alert_line`]). A crash cannot wait for a
//!   narration that may never run.
//! - **The completion notice** ([`agent_notice_line`], D106). One dim `●` line
//!   where the task notification of a finished run reaches main's context,
//!   before main says anything about it.
//! - **The direct send** ([`Chat::parse_direct_send`]). A composer line shaped
//!   `@scout fix the lexer` or `#build tests are green` bypasses the model and
//!   goes straight to that inbox or that room, under the user's own name, with
//!   a transient receipt and nothing in main's history.
//!
//! The post-row renderer that used to live here retired with the alt-screen
//! zoom (v6): a conversation you are standing in is an away page now, drawn
//! by the transcript's own pipeline ([`crate::tui::conv`]).

use crate::channels::USER_NAME;
use crate::tui::buffer::{Delivery, SubmitTarget};
use crate::tui::chat::{Chat, Row, one_line};
use crate::tui::line::Line;

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

/// Who a composer line addresses when it opens with a sigil (D103). The grammar
/// and its resolution are the core's since B3; this is the name the console
/// reads them under.
pub use crate::app::submit::DirectTarget;

impl Chat {
    /// The rows a line about somebody else's life takes in the flow (D106) —
    /// `None` for anything the ordinary user-message renderer should draw.
    ///
    /// One shape is left: the dim `●` completion notice, and only for a run
    /// main's own turn dispatched (D114). The `@name❯` arrival line retired
    /// with the inbox turn — a message an agent sends main is main's mail,
    /// shown as the sender's dot in the status layer, not as a flow row.
    pub(crate) fn agent_flow_rows(&self, text: &str, width: usize) -> Option<Vec<Row>> {
        let theme = &self.theme;
        if !is_agent_notice(text) {
            return None;
        }
        let mut line = Line::styled(AGENT_NOTICE_PREFIX.to_string(), theme.tool_done());
        line.push_styled(
            one_line(text.trim_start_matches(AGENT_NOTICE_PREFIX), width),
            theme.dim(),
        );
        Some(vec![Row::new(line)])
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
                    && let Err(why) = self.session.channels.invite(name, USER_NAME).now()
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
            // The user's line enters the receiver's transcript the moment it is
            // sent, and so does everybody else's — but not from here. D134 put
            // the echo on this path and covered only the user, which left main's
            // own mail invisible on the page of the instance it was sent to.
            // The delivery emits `UiEvent::Mail` for every sender instead
            // (D135), and a room needs none of it: its page is projected from
            // the log the post just entered.
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
        match self.session.channels.invite(&room, USER_NAME).now() {
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
        match self.session.channels.kick(&room, USER_NAME).now() {
            Ok(()) => {
                self.refresh_conversations();
                self.push_slash_info(format!("left #{room}"));
            }
            Err(why) => self.push_slash_info(why),
        }
    }
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
        chat.session
            .agents
            .insert(
                name,
                AgentKind::Hire,
                None,
                "test instance".to_string(),
                chat.session.clone(),
            )
            .now();
        if !history.is_empty() {
            chat.session.agents.finish(name, history, 0).now();
        }
    }

    fn main_message(chat: &mut Chat, role: Role, text: &str) {
        chat.conv.messages.push(UiMessage {
            speaker: None,
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
        chat.session
            .agents
            .insert(
                "dev",
                AgentKind::Crew,
                None,
                "crew member".to_string(),
                chat.session.clone(),
            )
            .now();
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
                .conv
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
        let before = chat.conv.messages.len();

        chat.set_input("@scout have a look at the parser");
        chat.submit();

        assert!(!chat.conv.busy, "a direct send is not a turn");
        assert!(
            chat.main_queue().is_empty(),
            "and it did not queue behind main"
        );
        assert_eq!(
            chat.conv.messages.len(),
            before,
            "nothing was written into main's history"
        );

        let items = chat.session.agents.take_running("scout", 0).await;
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
            chat.conv
                .messages
                .iter()
                .all(|m| !m.text.contains("Sent to")),
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
            .await
            .expect("channel created");
        chat.refresh_conversations();
        assert!(
            !chat.session.channels.is_member("build", USER_NAME),
            "the user starts outside the room"
        );

        chat.set_input("#build tests are green");
        chat.submit();

        assert!(!chat.conv.busy);
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

        assert!(chat.conv.busy, "it opened an ordinary main turn");
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
            .now()
            .expect("channel created");
        chat.refresh_conversations();

        // The grammar is the core's, and these are the names it resolves
        // against: one live instance and one live room.
        let addressed = |chat: &mut Chat, line: &str| match chat.route_submission(line) {
            crate::app::submit::Route::Deliver {
                target,
                text,
                addressed: true,
            } => Some((target, text)),
            _ => None,
        };

        assert_eq!(addressed(&mut chat, "@scout"), None);
        assert_eq!(addressed(&mut chat, "@scout   "), None);
        assert_eq!(
            addressed(&mut chat, "scout hello"),
            None,
            "the sigil is required"
        );
        assert_eq!(
            addressed(&mut chat, "@Scout hello"),
            None,
            "and the name resolves exactly, not case-insensitively"
        );
        assert_eq!(
            addressed(&mut chat, "@main hello"),
            None,
            "main is who you are already talking to, so it is never an envelope"
        );
        assert_eq!(
            addressed(&mut chat, "@scout hello"),
            Some((
                DirectTarget::Agent("scout".to_string()),
                "hello".to_string()
            ))
        );
        assert_eq!(
            addressed(&mut chat, "@scout\nlook at this\nand this"),
            Some((
                DirectTarget::Agent("scout".to_string()),
                "look at this\nand this".to_string()
            )),
            "a newline is whitespace, as in CC's own regex"
        );
        assert_eq!(
            addressed(&mut chat, "#build ship it"),
            Some((
                DirectTarget::Room("build".to_string()),
                "ship it".to_string()
            ))
        );
        assert_eq!(
            addressed(&mut chat, "#nowhere ship it"),
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
        chat.chat_avatars = true; // the gutter under test follows the one avatar switch (D110)
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
            chat.chat_avatars = true; // the skins under test follow the one avatar switch (D110)
        }
        for chat in [&mut chip, &mut placed] {
            main_message(chat, Role::User, "a question");
            main_message(chat, Role::Assistant, "main prose that runs on a while");
        }
        let chip_rows = raw_rows(&mut chip);
        let placed_rows = raw_rows(&mut placed);
        // The one licensed height difference: a lead message shorter than the
        // portrait is padded to the portrait's two rows in the image skin only
        // (the chip is one row and pads nothing). Here that is the one-line
        // user question, so the placed skin is exactly one row taller — any
        // other drift between the skins is still a bug this test catches.
        assert_eq!(
            placed_rows.len(),
            chip_rows.len() + 1,
            "the skins differ by exactly the short message's pad row"
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

    /// The image skin sends each portrait's gutter cells as the terminal's
    /// own — the kitty placeholder run, with the image id in the foreground.
    /// Asserted at the sequence level, the way `avatar.rs` asserts its own.
    #[test]
    fn the_image_skin_puts_placeholder_cells_in_the_gutter() {
        let theme = crate::tui::theme::Theme::dark();
        let pal = crate::tui::avatar::Palette::new(&theme);
        let pinned = std::collections::HashMap::new();
        let gutter = crate::tui::avatar::Gutter::new(true, true, &pal, &pinned);
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

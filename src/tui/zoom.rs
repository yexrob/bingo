//! The conversation targets (v6): which page the screen can become, and the
//! semantics a page shares with the console — entering reads, the composer
//! addresses the subject, shift+tab cycles the subject's permission mode.
//!
//! This module used to be the alt-screen zoom (D105): a second renderer over
//! flat post rows, gone the moment it closed. v6 retired the modal — an away
//! page is drawn by the transcript's own pipeline and banks into the
//! terminal's own scrollback ([`crate::tui::conv`]) — and what remains here is
//! the part that was always about *meaning* rather than machinery: the target
//! vocabulary, the accounting hand-off, the delivery path a page's composer
//! shares with the `@name` grammar, and the tree's enter.

use crate::agents::AgentState;
use crate::tui::buffer::BufferId;
use crate::tui::bufferview::DirectTarget;
use crate::tui::chat::Chat;

/// Which conversation the screen is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZoomTarget {
    /// A subagent instance: its whole record, and the composer routes to its
    /// inbox as the user.
    Agent(String),
    /// A room's log: the composer posts to it as the user, joining first.
    ///
    /// **The door is the background dialog's `f`** (D107). D105 built this view
    /// with no way in, deliberately: CC has no rooms at all — the concept does
    /// not exist anywhere in 2.1.88 — so there was no key of its to copy, and
    /// inventing a global binding for one surface is how a keymap rots. The
    /// dialog has a Rooms section and CC's `f to foreground` on it
    /// (`BackgroundTasksDialog.tsx:414`), so the room is foregrounded by the
    /// same key and the same verb as an agent.
    Room(String),
}

impl ZoomTarget {
    /// The name, without its sigil.
    pub fn name(&self) -> &str {
        match self {
            Self::Agent(name) | Self::Room(name) => name,
        }
    }

    /// The header's subject — the sigil is part of the identity.
    pub fn label(&self) -> String {
        match self {
            Self::Agent(name) => format!("@{name}"),
            Self::Room(name) => format!("#{name}"),
        }
    }

    /// Which conversation the accounting store is being pointed at.
    pub fn buffer(&self) -> BufferId {
        match self {
            Self::Agent(name) => BufferId::Dm(name.clone()),
            Self::Room(name) => BufferId::Channel(name.clone()),
        }
    }

    /// Where a submitted line goes. One grammar with the transcript's direct
    /// send (D103), because it is the same delivery — the zoom only spells the
    /// address differently.
    fn direct(&self) -> DirectTarget {
        match self {
            Self::Agent(name) => DirectTarget::Agent(name.clone()),
            Self::Room(name) => DirectTarget::Room(name.clone()),
        }
    }
}

impl Chat {
    /// The agent whose conversation has the screen — read by the tree's stem
    /// and the pills' bold state (D104), both of which the zoom draws.
    ///
    /// A room zoom answers `None`: the pills and the tree list agents, and the
    /// row that would be lit is not on them.
    pub(crate) fn zoomed(&self) -> Option<&str> {
        match self.zoom.as_ref()? {
            ZoomTarget::Agent(name) => Some(name.as_str()),
            ZoomTarget::Room(_) => None,
        }
    }

    /// `shift+tab` while zoomed: cycle the **viewed agent's** permission mode
    /// and leave the console's alone (CC `PromptInput.tsx:1410-1447`).
    ///
    /// Same ladder the console cycles through
    /// ([`Chat::cycle_permission_mode`]), read and written on the instance's own
    /// session. A room has no mode to cycle, and neither has a name the registry
    /// has forgotten.
    pub(crate) fn cycle_zoom_permission_mode(&mut self) -> bool {
        let Some(ZoomTarget::Agent(name)) = self.zoom.clone() else {
            return false;
        };
        let Some(mode) = self.session.agents.permission_mode_of(&name) else {
            return false;
        };
        let next = crate::tui::chat::next_permission_mode(mode, self.session.permission_mode);
        self.session.agents.set_permission_mode(&name, next);
        self.dirty = true;
        true
    }

    /// The permission mode the zoom's chrome reports: the viewed agent's, which
    /// is what CC swaps into the footer while a teammate has the screen
    /// (`PromptInput.tsx:342-351`).
    pub(crate) fn zoom_permission_mode(&self) -> Option<crate::permission::PermissionMode> {
        match self.zoom.as_ref()? {
            ZoomTarget::Agent(name) => self.session.agents.permission_mode_of(name),
            ZoomTarget::Room(_) => None,
        }
    }

    /// Who the composer is addressing, when it is not main: the zoom's subject,
    /// room included. The chrome tints itself with this.
    pub(crate) fn zoom_subject(&self) -> Option<String> {
        self.zoom.as_ref().map(|t| t.name().to_string())
    }

    /// Whether `esc` would stop a run rather than leave — what the hint row has
    /// to say out loud, because one key with two meanings has to declare which
    /// one it has right now (D39).
    pub(crate) fn zoom_stoppable(&self) -> bool {
        self.zoom_is_running()
    }

    /// Whether the viewed agent is running right now.
    pub(crate) fn zoom_is_running(&self) -> bool {
        let Some(name) = self.zoomed() else {
            return false;
        };
        self.tree_instances()
            .iter()
            .any(|s| s.name == name && s.state == AgentState::Running)
    }

    /// Point the screen at a conversation: the accounting follows the reader,
    /// so entering one **reads** it and nothing in it is unread while it is up
    /// ([`crate::tui::buffer::Buffers::set_active`], which also clears the
    /// mention flag).
    pub(crate) fn enter_zoom(&mut self, target: ZoomTarget) {
        self.refresh_conversations();
        self.buffers.set_active(target.buffer());
        // The sender's mail dot clears the same way (D115): its whole meaning
        // is "you have not looked since this agent wrote to main", and this
        // is the looking.
        if let ZoomTarget::Agent(name) = &target {
            self.agent_mail.remove(name.as_str());
        }
        self.zoom = Some(target);
        self.dirty = true;
    }

    /// Give the screen back. The accounting returns to whatever it was pointed
    /// at before, and the tree's cursor is dropped — it belongs to the gesture
    /// that opened the view, not to the transcript underneath.
    pub(crate) fn leave_zoom(&mut self, home: BufferId) {
        self.zoom = None;
        self.buffers.set_active(home);
        self.dirty = true;
    }

    /// Submit the draft to the conversation on screen.
    ///
    /// One delivery path with the transcript's `@name`/`#room` send (D103):
    /// [`Chat::direct_send`] puts it in the inbox (or the room's log) under the
    /// user's own name, which is where the domain's own rules take over —
    /// queue while the run is busy, drain at the next round, resume an instance
    /// that had stopped, announce a join before speaking in a room.
    ///
    /// **The draft is plain text.** A `/` line is not a command and a `!` line
    /// is not a shell: neither ever reaches the console's parser, because the
    /// console is not what this composer is addressing. CC does the same by
    /// construction — the teammate route returns before any slash handling
    /// (`PromptInput.tsx:1086-1097`, `REPL.tsx:3548-3578`) — and here it is by
    /// construction too, since nothing but this function reads the draft.
    ///
    /// **The echo is the receipt.** A message the user just sent is in the
    /// instance's inbox, so the very next frame draws it as a queued bubble;
    /// the transcript needs a `Sent to @scout` line because it has nowhere else
    /// to show one, and this view is the somewhere else.
    pub(crate) fn submit_to_zoom(&mut self) {
        let Some(target) = self.zoom.clone() else {
            return;
        };
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.undo.clear();
        self.last_edit = None;
        if text.trim().is_empty() {
            self.set_input(text);
            return;
        }
        let body = self.expand_pastes(&text);
        let body = self.expand_image_paths(&body);
        // The whole line goes into history, exactly as the transcript's own
        // submit records it: ↑ brings back what was typed.
        self.record_history(&text);
        // A refusal is news and takes the warning tier the stop takes; the
        // *receipt* the transcript prints is not needed here, because what it
        // would announce is drawn on this screen the moment the frame redraws.
        if let crate::tui::buffer::Delivery::Rejected(why) =
            self.deliver_direct(&target.direct(), body)
        {
            self.push_warning(why);
        }
        self.update_slash_suggestions();
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;
    use crate::api::types::{ContentBlock, Message as ApiMessage, Role as ApiRole};
    use crate::channels::ChannelMode;
    use crate::permission::PermissionMode;
    use crate::tui::test_util::chat_at;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn test_chat() -> Chat {
        chat_at(100, 40)
    }

    /// An instance the registry has just been told about — which is a
    /// *running* one: `insert` is the spawn path and a spawned agent is working.
    fn seed(chat: &Chat, name: &str) {
        chat.session.agents.insert(
            name,
            AgentKind::Hire,
            None,
            "fix the parser".to_string(),
            chat.session.clone(),
        );
    }

    /// The same instance, parked: `finish` with an empty inbox is the one
    /// legal door to `Idle`.
    fn seed_idle(chat: &Chat, name: &str) {
        seed(chat, name);
        let _ = chat.session.agents.finish(name, Vec::new(), 0);
    }

    fn assistant(text: &str) -> ApiMessage {
        ApiMessage {
            role: ApiRole::Assistant,
            content: vec![ContentBlock::Text {
                text: text.to_string(),
            }],
        }
    }

    fn seed_with_history(chat: &Chat, name: &str, history: Vec<ApiMessage>) {
        seed(chat, name);
        let _ = chat.session.agents.finish(name, history, 0);
    }

    fn seed_room(chat: &Chat, name: &str, members: &[&str]) {
        chat.session
            .channels
            .create(
                name,
                members.iter().map(|m| m.to_string()).collect(),
                ChannelMode::Free,
            )
            .unwrap_or_else(|e| panic!("{e}"));
    }

    fn page_rows(chat: &mut Chat) -> Vec<String> {
        chat.build_rows(96);
        chat.doc
            .rows
            .iter()
            .map(|r| r.line.plain_text().trim_end().to_string())
            .collect()
    }

    /// The v6 headline: an agent's page is drawn by the transcript's own
    /// pipeline — its header rule, the task as intake, its prose as markdown —
    /// and the room's page is speech only.
    #[test]
    fn a_page_is_the_transcripts_own_pipeline() {
        let mut chat = test_chat();
        seed_with_history(
            &chat,
            "scout",
            vec![
                ApiMessage::user_text("fix the parser"),
                assistant("the lexer drops a token at EOF"),
            ],
        );
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        let rows = page_rows(&mut chat);
        assert!(
            rows.iter().any(|r| r.starts_with("── @scout ")),
            "the page opens with its name as a rule: {rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("fix the parser")),
            "the task is on the page: {rows:?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("the lexer drops a token")),
            "the agent's prose is on the page: {rows:?}"
        );

        // Home again: the transcript's document, not the page's.
        chat.switch_to(None);
        let rows = page_rows(&mut chat);
        assert!(
            !rows.iter().any(|r| r.starts_with("── @scout ")),
            "home has no page header: {rows:?}"
        );
    }

    /// The room page shows what members said to the room, and nothing else —
    /// the v6 ruling: no membership lines, no process.
    #[test]
    fn a_room_page_is_speech_only() {
        let mut chat = test_chat();
        seed(&chat, "zoe");
        seed_room(&chat, "crew", &["user", "zoe"]);
        chat.session
            .channels
            .invite("crew", "late")
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = chat.session.channels.post("zoe", "crew", "tests are green");
        chat.switch_to(Some(ZoomTarget::Room("crew".into())));
        let rows = page_rows(&mut chat);
        assert!(
            rows.iter().any(|r| r.contains("tests are green")),
            "speech is on the page: {rows:?}"
        );
        assert!(
            !rows.iter().any(|r| r.contains("joined")),
            "membership lines stay out of the page: {rows:?}"
        );
    }

    /// Typing on a page reaches the subject as the user, through the same
    /// delivery the `@name` grammar uses — and a `/` line is a message, not a
    /// command (the zoom's rule, which is CC's).
    #[tokio::test]
    async fn typing_on_a_page_reaches_the_agent_as_the_user() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        chat.set_input("/help is not a command here".to_string());
        chat.submit();
        let pending = chat.session.agents.pending_of("scout");
        assert_eq!(pending.len(), 1, "{pending:?}");
        assert_eq!(pending[0].0, "user");
        assert!(pending[0].1.contains("/help is not a command here"));
        assert!(
            chat.slash_info_lines.is_empty() && chat.slash_lines.is_empty(),
            "the console's parser never saw the draft"
        );
    }

    /// A message resumes a stopped instance (D105a's one door), and the page
    /// is where the user most naturally opens it.
    #[tokio::test]
    async fn a_message_to_a_stopped_agent_resumes_it() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        let _ = chat.session.agents.stop("scout");
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        chat.set_input("wake up".to_string());
        chat.submit();
        assert!(
            chat.session
                .agents
                .list()
                .iter()
                .any(|s| s.name == "scout" && s.state == AgentState::Running),
            "delivery revived and the flush claimed it"
        );
    }

    /// Posting into a room the user is not in joins them first — the same
    /// auto-join the `#room` grammar performs.
    #[tokio::test]
    async fn a_room_page_joins_before_it_speaks() {
        let mut chat = test_chat();
        seed(&chat, "zoe");
        seed_room(&chat, "crew", &["zoe"]);
        chat.switch_to(Some(ZoomTarget::Room("crew".into())));
        chat.set_input("hello room".to_string());
        chat.submit();
        assert!(
            chat.session.channels.is_member("crew", "user"),
            "the post seated the user first"
        );
        let log = chat.session.channels.log_of("crew");
        assert!(
            log.iter()
                .any(|m| m.from == "user" && m.text == "hello room"),
            "{log:?}"
        );
    }

    /// Esc's ladder on a page: the running turn first (the subject's, never
    /// main's), then the page. One press, one level.
    #[test]
    fn esc_stops_the_run_first_and_comes_home_second() {
        let mut chat = test_chat();
        seed(&chat, "scout"); // Running
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        chat.conv.busy = true; // main's own turn, out of Esc's reach while away

        chat.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(
            chat.session
                .agents
                .list()
                .iter()
                .any(|s| s.name == "scout" && s.state == AgentState::Stopped),
            "the first press stops the subject's run"
        );
        assert!(chat.away.is_some(), "and stays on the page");
        assert!(chat.conv.busy, "main's turn was never touched");

        chat.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(chat.away.is_none(), "the second press comes home");
        assert!(chat.conv.busy, "still not touched");
    }

    /// `shift+tab` cycles the **viewed agent's** permission mode and leaves
    /// the console's alone (CC's rule, kept from the zoom).
    #[test]
    fn shift_tab_cycles_the_viewed_agents_mode_and_not_mains() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        let before_main = chat.permission_mode;
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        chat.on_key(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(chat.permission_mode, before_main, "main's mode untouched");
        assert_ne!(
            chat.session.agents.permission_mode_of("scout"),
            Some(PermissionMode::Default),
            "the subject's mode moved"
        );
    }

    /// Entering reads: the accounting's active conversation follows the page,
    /// the badge clears, the sender's mail dot clears — and leaving points it
    /// home again.
    #[test]
    fn entering_reads_the_conversation_and_leaving_gives_it_back() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.agent_mail.insert("scout".to_string(), 2);
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        assert_eq!(
            chat.buffers.active(),
            &BufferId::Dm("scout".to_string()),
            "the accounting follows the reader"
        );
        assert!(
            !chat.agent_mail.contains_key("scout"),
            "the mail dot is cleared by the looking"
        );
        chat.switch_to(None);
        assert_eq!(chat.buffers.active(), &BufferId::Hub);
    }

    /// The page closes itself when its subject leaves the domain — and stays
    /// open when the subject merely finishes (CC's ruling, kept).
    #[test]
    fn the_page_closes_when_its_subject_is_gone_and_stays_when_done() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        chat.sync_away();
        assert!(chat.away.is_some(), "a finished agent's page stays open");
        let _ = chat.session.agents.remove("scout");
        chat.sync_away();
        assert!(chat.away.is_none(), "a deleted agent's page cannot stay");
    }

    /// Every switch owes the terminal a page turn, and coming home reprints a
    /// recent tail (the flush cursor parks near the end rather than at zero).
    #[test]
    fn a_switch_owes_a_page_turn_and_home_reprints_the_tail() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        for i in 0..40 {
            chat.conv.messages.push(crate::tui::chat::UiMessage {
                role: crate::tui::chat::Role::User,
                text: format!("line {i}"),
                at: 0,
                speaker: None,
                activities: Vec::new(),
                insert_points: Vec::new(),
                groups: Vec::new(),
                group_of: Vec::new(),
            });
        }
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        assert!(
            std::mem::take(&mut chat.page_turn),
            "leaving turns the page"
        );
        assert_eq!(chat.flushed_segments, 0, "the page starts from its top");
        chat.switch_to(None);
        assert!(
            std::mem::take(&mut chat.page_turn),
            "coming home turns it again"
        );
        assert!(
            chat.flushed_segments > 0,
            "home owes a recent tail, not the whole record: {}",
            chat.flushed_segments
        );
    }

    /// The roster's enter opens pages (v6): an agent row switches to it and
    /// the main row comes home — the CC footer's `openSelected`, one flat
    /// list instead of a tree.
    #[test]
    fn enter_on_a_roster_row_switches_and_main_comes_home() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.refresh_conversations();
        assert!(chat.roster_enter_selection(), "↓ fell into the rows");
        chat.roster_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(chat.roster_selection(), Some(1), "on the agent's row");
        assert!(chat.roster_key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            chat.zoom,
            Some(ZoomTarget::Agent("scout".to_string())),
            "enter switched to the page"
        );
        assert!(chat.away.is_some());
        assert!(
            chat.roster_selection().is_none(),
            "the cursor is spent by the switch"
        );
        // Main's row comes home.
        assert!(chat.roster_enter_selection());
        assert!(chat.roster_key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(chat.away.is_none(), "the main row comes home");
    }
}

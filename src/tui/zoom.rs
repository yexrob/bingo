//! The conversation targets (v6): which page the screen can become, and the
//! semantics a page shares with the console — entering reads, the composer
//! addresses the subject, shift+tab cycles the subject's permission mode.
//!
//! This module used to be the alt-screen zoom (D105): a second renderer over
//! flat post rows, gone the moment it closed. v6 retired the modal — an away
//! page is drawn by the transcript's own pipeline and banks into the
//! terminal's own scrollback ([`crate::tui::conv`]) — and what remains here is
//! the part that was always about *meaning* rather than machinery: the target
//! vocabulary, the accounting hand-off, and the tree's enter.
//!
//! **The composer left with D135.** A page's draft used to be submitted here,
//! by a second `submit` that read the whole line as prose. There is one now
//! ([`Chat::submit`]): the console's `/`, `!` and `@name` work on every page,
//! and where the *prose* goes is the only thing that reads which page it is.

use crate::agents::AgentState;
use crate::tui::buffer::BufferId;
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

    /// Which store the console keeps for this page.
    pub fn conv_key(&self) -> crate::ui::ConvKey {
        match self {
            Self::Agent(name) => crate::ui::ConvKey::Agent(name.clone()),
            Self::Room(name) => crate::ui::ConvKey::Room(name.clone()),
        }
    }

    /// Which conversation the accounting store is being pointed at.
    pub fn buffer(&self) -> BufferId {
        match self {
            Self::Agent(name) => BufferId::Dm(name.clone()),
            Self::Room(name) => BufferId::Channel(name.clone()),
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
            .now()
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
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let _ = chat
            .session
            .channels
            .post("zoe", "crew", "tests are green")
            .now();
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
    /// delivery the `@name` grammar uses.
    #[tokio::test]
    async fn typing_on_a_page_reaches_the_agent_as_the_user() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        chat.set_input("look at the lexer".to_string());
        chat.submit();
        let pending = chat.session.agents.pending_of("scout");
        assert_eq!(pending.len(), 1, "{pending:?}");
        assert_eq!(pending[0].0, "user");
        assert!(pending[0].1.contains("look at the lexer"));
    }

    /// **Reversed by D135**, and it is the whole point of the record: a `/`
    /// line used to be prose here, on the argument that the page's composer
    /// addresses the page. It addresses the *console* — the command is the
    /// console's, its answer is drawn on whatever page is up, and the agent
    /// hears nothing.
    #[tokio::test]
    async fn a_slash_line_on_a_page_is_the_consoles_command() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        chat.set_input("/help".to_string());
        chat.submit();
        assert!(
            chat.session.agents.pending_of("scout").is_empty(),
            "scout heard nothing"
        );
        assert!(!chat.slash_info_lines.is_empty(), "the console answered");
        let rows = page_rows(&mut chat);
        assert!(
            rows.iter().any(|r| r.contains("/compact")),
            "and the answer is on the page the user is looking at: {rows:?}"
        );
    }

    /// The ruling `/compact` is the exception to (D135): most commands are
    /// console settings and keep acting on the console, but compaction acts on
    /// the context in front of you, because it is the one command whose
    /// wrong target loses real work.
    #[tokio::test]
    async fn compact_on_a_page_targets_that_agents_context() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout"); // finished: an empty stored history
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        chat.set_input("/compact".to_string());
        chat.submit();
        assert!(
            chat.slash_lines.iter().any(|l| l.contains("@scout")),
            "the answer names the instance, not the console: {:?}",
            chat.slash_lines
        );
        assert!(
            chat.pinned_panels.is_empty(),
            "and the console's own compaction never started"
        );
    }

    /// A running instance owns the history a compaction would rewrite, so the
    /// command refuses instead of racing the turn that will overwrite it.
    #[tokio::test]
    async fn compact_refuses_while_the_agent_is_mid_turn() {
        let mut chat = test_chat();
        seed(&chat, "scout"); // spawned = Running
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        chat.set_input("/compact".to_string());
        chat.submit();
        assert!(
            chat.slash_error_lines
                .iter()
                .any(|l| l.contains("@scout") && l.contains("mid-turn")),
            "{:?}",
            chat.slash_error_lines
        );
    }

    /// A room has no context behind it, and compacting the console's instead
    /// would be exactly the wrong-target loss the ruling exists to prevent.
    #[tokio::test]
    async fn compact_on_a_room_page_says_there_is_nothing_to_compact() {
        let mut chat = test_chat();
        seed_room(&chat, "crew", &["user"]);
        chat.switch_to(Some(ZoomTarget::Room("crew".into())));
        chat.set_input("/compact".to_string());
        chat.submit();
        assert!(
            chat.slash_info_lines.iter().any(|l| l.contains("#crew")),
            "{:?}",
            chat.slash_info_lines
        );
        assert!(chat.pinned_panels.is_empty());
    }

    /// And home it is the console's own context, as it always was.
    #[tokio::test]
    async fn compact_at_home_is_still_the_consoles() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout");
        chat.set_input("/compact".to_string());
        chat.submit();
        assert!(
            chat.pinned_panels
                .iter()
                .any(|(id, lines)| id == "compact" && lines.iter().all(|l| !l.contains('@'))),
            "{:?}",
            chat.pinned_panels
        );
    }

    /// `!` is the console's shell on every page too, and it opens the console's
    /// own turn rather than sending the command to the agent.
    #[tokio::test]
    async fn bash_mode_on_a_page_runs_the_consoles_shell() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        chat.on_key(KeyCode::Char('!'), KeyModifiers::NONE);
        assert!(chat.bash_mode, "`!` on an empty composer arms shell mode");
        chat.set_input("echo hi".to_string());
        chat.submit();
        assert!(
            chat.session.agents.pending_of("scout").is_empty(),
            "the command went to the shell, not to scout"
        );
        assert!(chat.main_conv().busy, "the console's own turn opened");
    }

    /// Shell mode is the console's, so it survives a page turn — and Esc's
    /// ladder leaves the mode before it leaves the page.
    #[test]
    fn shell_mode_outlives_a_page_turn() {
        let mut chat = test_chat();
        seed_idle(&chat, "scout"); // no run to stop, so Esc reaches the mode
        chat.on_key(KeyCode::Char('!'), KeyModifiers::NONE);
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        assert!(chat.bash_mode, "the mode came with the reader");
        chat.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!chat.bash_mode, "esc leaves the mode first");
        assert!(!chat.active.is_main(), "and the page is still up");
        chat.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(chat.active.is_main(), "the next press comes home");
    }

    /// The `@name` grammar reaches somebody who is not the page's subject —
    /// dead on a page until D135, because the whole line was prose.
    #[tokio::test]
    async fn a_direct_send_from_a_page_reaches_a_third_party() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        seed(&chat, "dev");
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        chat.set_input("@dev take the parser".to_string());
        chat.submit();
        assert!(
            chat.session.agents.pending_of("scout").is_empty(),
            "the page's subject is not the addressee"
        );
        let pending = chat.session.agents.pending_of("dev");
        assert_eq!(pending.len(), 1, "{pending:?}");
        assert!(pending[0].1.contains("take the parser"));
    }

    /// A console command typed while main's turn runs waits behind that turn,
    /// wherever the screen is — and says so, because the queue rows are main's
    /// page and this is not main's page.
    #[test]
    fn a_command_on_a_page_waits_behind_mains_turn_and_says_so() {
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.conv.busy = true; // main's turn, before the page turn parks it
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        chat.set_input("/clear".to_string());
        chat.submit();
        assert!(
            chat.session.agents.pending_of("scout").is_empty(),
            "and it never reached the agent"
        );
        assert_eq!(
            chat.main_conv().queued.len(),
            1,
            "it is in the console's queue"
        );
        assert!(
            chat.slash_info_lines.iter().any(|l| l.contains("queued")),
            "{:?}",
            chat.slash_info_lines
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
        chat.conv.busy = true; // main's own turn, out of Esc's reach while away
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));

        chat.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(
            chat.session
                .agents
                .list()
                .iter()
                .any(|s| s.name == "scout" && s.state == AgentState::Stopped),
            "the first press stops the subject's run"
        );
        assert!(!chat.active.is_main(), "and stays on the page");
        assert!(chat.main_conv().busy, "main's turn was never touched");

        chat.on_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(chat.active.is_main(), "the second press comes home");
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
        assert!(!chat.active.is_main(), "a finished agent's page stays open");
        let _ = chat.session.agents.remove("scout");
        chat.sync_away();
        assert!(chat.active.is_main(), "a deleted agent's page cannot stay");
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
        assert!(!chat.active.is_main());
        assert!(
            chat.roster_selection().is_none(),
            "the cursor is spent by the switch"
        );
        // Main's row comes home.
        assert!(chat.roster_enter_selection());
        assert!(chat.roster_key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(chat.active.is_main(), "the main row comes home");
    }

    /// D137: a colleague's direct message lands on the receiver's page, named.
    /// No surface was added for it — `deliver` is the door every sender already
    /// used, so the page that showed main's mail shows a peer's.
    #[test]
    fn a_peers_message_lands_on_the_receivers_page() {
        let mut chat = test_chat();
        seed(&chat, "dev");
        seed(&chat, "qa");
        chat.session
            .agents
            .deliver("qa", "dev", "does the parser handle EOF?", Vec::new(), None)
            .unwrap_or_else(|e| panic!("{e}"));
        chat.switch_to(Some(ZoomTarget::Agent("qa".into())));
        let rows = page_rows(&mut chat);
        assert!(
            rows.iter().any(|r| r.contains("@dev")),
            "the page says who wrote: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|r| r.contains("does the parser handle EOF?")),
            "{rows:?}"
        );
    }

    /// And one already in the record is filed the same way by the cold-start
    /// walk: the marker is the only thing either path reads, so the live half
    /// and the committed half cannot disagree about who spoke.
    #[test]
    fn a_peers_message_in_the_record_is_filed_to_the_peer() {
        let mut chat = test_chat();
        seed_with_history(
            &chat,
            "qa",
            vec![
                ApiMessage {
                    role: ApiRole::User,
                    content: vec![ContentBlock::Text {
                        text: crate::channels::format_agent_message(
                            "dev",
                            "does the parser handle EOF?",
                        ),
                    }],
                },
                assistant("it does"),
            ],
        );
        chat.switch_to(Some(ZoomTarget::Agent("qa".into())));
        let rows = page_rows(&mut chat);
        assert!(
            rows.iter().any(|r| r.contains("@dev")),
            "the walk names the colleague: {rows:?}"
        );
        assert!(
            rows.iter()
                .all(|r| !r.contains(crate::channels::AGENT_MESSAGE_PREFIX)),
            "and drops the scaffolding rather than rendering it as prose: {rows:?}"
        );
    }
}

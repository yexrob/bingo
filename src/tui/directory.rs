//! The team directory (D95): the organization, seen whole.
//!
//! The team is not a conversation. You cannot say anything to it, nothing is
//! ever addressed to it, and the thing you actually want from it is an answer
//! to "who is here, what rooms exist, what just happened" — three questions a
//! `#team` buffer answered by pretending to be a chat you could open, badge and
//! all. So it is a directory instead: a read-only view on the second stop of
//! the `ctrl+t` cycle, built from live sources every time it is drawn.
//!
//! **Navigation and information only.** Enter opens a member's conversation (a
//! DM, or the console for main) or a room; `o` opens a member's observation
//! page; `j` joins the room under the cursor. Stopping an agent, restarting one,
//! reading its stats — all of that stays in the `ctrl+b` manager, which already
//! owns those verbs and their warnings. A second surface that could stop an
//! agent would be a second place for "stop" to mean something slightly
//! different, and the switcher's `ctrl+x` already pays that tax by routing
//! through the manager's own path.
//!
//! Three sources, no copies: the agent registry is the roster, the channel
//! registry is the room list, and [`crate::tui::buffer::Buffers::team_log`] —
//! the bounded lifecycle log that used to back the `#team` board — is the feed.
//! The board's storage was never the problem; where it was rendered was.
//!
//! **Main is on the roster (D100)**, first and always present. It is a
//! participant like the rest — it has a conversation, rooms and a record — and
//! a roster that listed everyone it dispatches but not itself was a roster of
//! the team minus its first member. What stays special is the host machinery,
//! not the row: main's presence is the host turn rather than a registry state,
//! and it is not stoppable — which costs nothing here, because this surface has
//! no verb that stops anybody.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::channels::{HUB_NAME, USER_NAME};
use crate::tui::buffer::{BufferId, team_line};
use crate::tui::chat::{Chat, Row, one_line};
use crate::tui::line::{Line, SegStyle};

/// How many lifecycle entries the feed shows. The log keeps two hundred; a
/// directory answering "what just happened" wants the last handful, and the
/// rest is the transcript's business.
const FEED_SHOWN: usize = 10;

/// How many rooms are named beside a member before the list gives up and
/// counts. Three fits the row; a member in eight rooms is telling you they are
/// in eight rooms, not which ones.
const ROOMS_NAMED: usize = 3;

/// The open directory. One cursor, and it indexes the *selectable* rows only —
/// headings are not destinations, and an arrow key that lands on one would be a
/// press that appeared to do nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Directory {
    pub selected: usize,
}

/// What the cursor can be on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirTarget {
    /// A teammate: Enter opens their DM.
    Member(String),
    /// A room: Enter opens it — as a conversation if you are in it, as
    /// something you are watching if you are not.
    Room(String),
}

/// One line of the directory, and whether the cursor can land on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirRow {
    pub text: String,
    /// `None` for a heading or a note; `Some` for anything Enter can open.
    pub target: Option<DirTarget>,
    /// Headings carry the section name; the renderer emphasizes them.
    pub heading: bool,
}

impl DirRow {
    fn heading(text: &str) -> Self {
        Self {
            text: text.to_string(),
            target: None,
            heading: true,
        }
    }
    fn note(text: String) -> Self {
        Self {
            text,
            target: None,
            heading: false,
        }
    }
    fn at(text: String, target: DirTarget) -> Self {
        Self {
            text,
            target: Some(target),
            heading: false,
        }
    }
}

/// `#a #b #c` — or `#a #b +4` once naming them all would cost more than it says.
fn rooms_label(rooms: &[String]) -> String {
    if rooms.is_empty() {
        return String::new();
    }
    let named: Vec<String> = rooms
        .iter()
        .take(ROOMS_NAMED)
        .map(|room| format!("#{room}"))
        .collect();
    let extra = rooms.len().saturating_sub(ROOMS_NAMED);
    if extra == 0 {
        named.join(" ")
    } else {
        format!("{} +{extra}", named.join(" "))
    }
}

impl Chat {
    /// Open the directory. Inert behind a permission dialog, for the switcher's
    /// reason (D81): a surface that competes for Enter must not open over a
    /// question that is holding up a turn.
    pub(crate) fn open_directory(&mut self) {
        if self.pending_ask.is_some() {
            return;
        }
        self.directory = Some(Directory::default());
        self.dirty = true;
    }

    /// The whole directory, rebuilt from the domain. Nothing is cached: the
    /// roster is a dozen rows, it is drawn only while the panel is open, and a
    /// cached roster is a roster that can be wrong.
    pub(crate) fn directory_rows(&self) -> Vec<DirRow> {
        let mut rows = vec![DirRow::heading("Team")];
        // Main is a participant, so it is on the roster — first, because it is
        // the one member of the team that is always there (D100). Its presence
        // is the host turn: `busy` is the console's own running state, where a
        // subagent's comes from the registry. It is not stoppable and not
        // manageable, and the directory has no verb that could try.
        let presence = if self.busy { "●" } else { "○" };
        let state = if self.busy { "running" } else { "idle" };
        let rooms = rooms_label(&self.session.channels.rooms_of(HUB_NAME));
        let tail = if rooms.is_empty() {
            String::new()
        } else {
            format!(" · {rooms}")
        };
        rows.push(DirRow::at(
            format!("{presence} {HUB_NAME} · console · {state}{tail}"),
            DirTarget::Member(HUB_NAME.to_string()),
        ));
        let agents = self.session.agents.list();
        if agents.is_empty() {
            rows.push(DirRow::note("no teammates yet".to_string()));
        }
        for status in &agents {
            let presence = match status.state {
                crate::agents::AgentState::Running => "●",
                crate::agents::AgentState::Idle => "○",
                crate::agents::AgentState::Stopped => "·",
            };
            let rooms = rooms_label(&self.session.channels.rooms_of(&status.name));
            let tail = if rooms.is_empty() {
                String::new()
            } else {
                format!(" · {rooms}")
            };
            rows.push(DirRow::at(
                format!(
                    "{presence} {} · {} · {}{tail}",
                    status.name,
                    status.kind.label(),
                    status.state.label()
                ),
                DirTarget::Member(status.name.clone()),
            ));
        }

        rows.push(DirRow::heading("Rooms"));
        let rooms = self.session.channels.list();
        if rooms.is_empty() {
            rows.push(DirRow::note("no rooms yet".to_string()));
        }
        for room in &rooms {
            // The mark is on the rooms you are *not* in, because those are the
            // ones where opening means something different. A mark on every
            // room you are in would be a column of ticks saying nothing.
            let mine = room.members.iter().any(|member| member == USER_NAME);
            let outside = if mine { "" } else { " · you're not in" };
            rows.push(DirRow::at(
                format!(
                    "#{} · {} msgs · {}{outside}",
                    room.name,
                    room.seq,
                    room.members.join(", ")
                ),
                DirTarget::Room(room.name.clone()),
            ));
        }

        rows.push(DirRow::heading("Recent"));
        let feed = self.buffers.team_log();
        if feed.is_empty() {
            rows.push(DirRow::note("nothing yet".to_string()));
        }
        // Newest first: a feed read top-down should open on what just happened,
        // where the log is written oldest-first because it is a log.
        for event in feed.iter().rev().take(FEED_SHOWN) {
            rows.push(DirRow::note(format!(
                "{} · {}",
                event.label,
                team_line(event)
            )));
        }
        rows
    }

    /// The rows the cursor can land on, in the order they are drawn.
    fn directory_targets(&self) -> Vec<DirTarget> {
        self.directory_rows()
            .into_iter()
            .filter_map(|row| row.target)
            .collect()
    }

    /// Keys, while the directory is open. Modal: an open directory swallows what
    /// it does not understand rather than letting it edit the draft underneath,
    /// which is the switcher's rule and for the same reason.
    pub(crate) fn directory_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.pending_ask.is_some() {
            return false;
        }
        let Some(mut state) = self.directory.take() else {
            return false;
        };
        // Chords belong to the application, not to the panel: `ctrl+t` is the
        // cycle that opened this and must be able to close it, `ctrl+c` always
        // means out (D80), and `ctrl+k` reaches the switcher from anywhere. The
        // switcher swallows chords because it is filtering text; this list is
        // not, so it has nothing to protect a draft from.
        if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            self.directory = Some(state);
            return false;
        }
        let targets = self.directory_targets();
        state.selected = state.selected.min(targets.len().saturating_sub(1));
        self.dirty = true;
        match code {
            KeyCode::Esc => return true,
            KeyCode::Down => {
                state.selected = (state.selected + 1).min(targets.len().saturating_sub(1));
            }
            KeyCode::Up => state.selected = state.selected.saturating_sub(1),
            KeyCode::Enter => {
                match targets.get(state.selected) {
                    // A room you are not in opens the same way a room you are in
                    // does. What differs is what the flow says about it and what
                    // the composer will do — both derived from membership, so
                    // there is no second door to keep in step with this one.
                    Some(DirTarget::Room(name)) => self.switch_to(BufferId::Channel(name.clone())),
                    // Main's conversation is the console, not a DM with an
                    // instance: `BufferId::Hub` *is* the pair view of the user
                    // and main, so Enter on its row goes home.
                    Some(DirTarget::Member(name)) if name == HUB_NAME => {
                        self.switch_to(BufferId::Hub)
                    }
                    Some(DirTarget::Member(name)) => self.switch_to(BufferId::Dm(name.clone())),
                    None => {}
                }
                return true;
            }
            // `o` opens the member's observation page (D100). It is the same
            // record `tab` reaches from inside a conversation and the ctrl+b
            // detail reaches from the manager — one page, three doors — and
            // like Enter it hands the screen over, so the panel closes behind
            // it. On a room it does nothing: a room has no protagonist.
            KeyCode::Char('o') => {
                if let Some(DirTarget::Member(name)) = targets.get(state.selected) {
                    self.open_perspective = Some(name.clone());
                    self.dirty = true;
                    return true;
                }
            }
            // Join from the directory. The panel stays open on purpose: the mark
            // flips under the cursor, which is the confirmation, and the reader
            // was in the middle of looking around.
            KeyCode::Char('j') => {
                if let Some(DirTarget::Room(name)) = targets.get(state.selected) {
                    let name = name.clone();
                    if let Err(why) = self.session.channels.invite(&name, USER_NAME) {
                        self.push_slash_info(why);
                    } else {
                        self.refresh_conversations();
                    }
                }
            }
            _ => {}
        }
        self.directory = Some(state);
        true
    }

    /// Rows for the overlay, in the ctrl+b manager's frame — the third surface
    /// to use it, because a reader should not have to relearn where a panel's
    /// edges are.
    pub fn directory_view_rows(&self, width: usize) -> Vec<Row> {
        let Some(state) = &self.directory else {
            return Vec::new();
        };
        let theme = &self.theme;
        let rows = self.directory_rows();
        let selectable = rows.iter().filter(|row| row.target.is_some()).count();
        let selected = state.selected.min(selectable.saturating_sub(1));
        let mut out = vec![Row::new(Line::styled(
            "Team directory",
            SegStyle::fg(theme.text).bold(),
        ))];
        let mut index = 0usize;
        for row in &rows {
            if row.heading {
                out.push(Row::new(Line::styled(
                    one_line(&row.text, width.saturating_sub(2)),
                    SegStyle::fg(theme.text_secondary).bold(),
                )));
                continue;
            }
            let Some(_) = &row.target else {
                out.push(Row::new(Line::styled(
                    one_line(&format!("    {}", row.text), width.saturating_sub(2)),
                    SegStyle::fg(theme.text_secondary),
                )));
                continue;
            };
            let here = index == selected;
            index += 1;
            out.push(Row::new(Line::styled(
                one_line(
                    &format!("{} {}", if here { "  ❯" } else { "   " }, row.text),
                    width.saturating_sub(2),
                ),
                SegStyle::fg(if here { theme.permission } else { theme.text }),
            )));
        }
        out.push(Row::new(Line::styled(
            "↑/↓ select · Enter open · o record · j join room · ctrl+t / Esc close",
            SegStyle::fg(theme.text_secondary),
        )));
        crate::tui::chat::manager_box(out, width, theme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;
    use crate::channels::ChannelMode;
    use crate::tui::test_util::chat_at;

    fn test_chat() -> Chat {
        chat_at(100, 40)
    }

    fn seed_agent(chat: &Chat, name: &str, kind: AgentKind) {
        chat.session.agents.insert(
            name,
            kind,
            None,
            "test instance".to_string(),
            chat.session.clone(),
        );
    }

    fn texts(chat: &Chat) -> Vec<String> {
        chat.directory_rows()
            .into_iter()
            .map(|row| row.text)
            .collect()
    }

    /// The three questions, in one view, from three live sources.
    #[test]
    fn the_directory_shows_the_roster_the_rooms_and_what_just_happened() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", AgentKind::Crew);
        seed_agent(&chat, "zoe", AgentKind::Hire);
        chat.session
            .channels
            .create(
                "build",
                vec![
                    "main".to_string(),
                    USER_NAME.to_string(),
                    "scout".to_string(),
                ],
                ChannelMode::Free,
            )
            .expect("room created");
        chat.session
            .channels
            .create(
                "parser",
                vec!["scout".to_string(), "zoe".to_string()],
                ChannelMode::Free,
            )
            .expect("room created");
        chat.buffers.note_watch_event(
            "scout #1 · fix the parser",
            crate::watch::WatchKind::Agent,
            crate::watch::WatchState::Done,
            Some("fixed it"),
            1,
        );
        chat.open_directory();

        let rows = texts(&chat);
        let all = rows.join("\n");
        assert!(rows.contains(&"Team".to_string()), "{all}");
        assert!(rows.contains(&"Rooms".to_string()), "{all}");
        assert!(rows.contains(&"Recent".to_string()), "{all}");
        // A member carries presence and the rooms they are in.
        assert!(
            rows.iter().any(|row| row.contains("scout")
                && row.contains("#build")
                && row.contains("#parser")),
            "a member lists their rooms: {all}"
        );
        // A room the user is not in is marked as such, and one they are in is not.
        assert!(
            rows.iter()
                .any(|row| row.starts_with("#parser") && row.contains("you're not in")),
            "{all}"
        );
        assert!(
            rows.iter()
                .any(|row| row.starts_with("#build") && !row.contains("you're not in")),
            "{all}"
        );
        // And the feed is the lifecycle log the retired board used to replay.
        assert!(
            rows.iter()
                .any(|row| row.contains("scout #1") && row.contains("done · fixed it")),
            "{all}"
        );
    }

    /// The feed reads newest first: a directory answers "what just happened",
    /// and the log is written oldest-first because it is a log.
    #[test]
    fn the_feed_opens_on_the_most_recent_thing() {
        let mut chat = test_chat();
        for i in 0..FEED_SHOWN + 5 {
            chat.buffers.note_watch_event(
                &format!("run #{i}"),
                crate::watch::WatchKind::Agent,
                crate::watch::WatchState::Running,
                None,
                1,
            );
        }
        chat.open_directory();
        let rows = texts(&chat);
        let feed: Vec<&String> = rows.iter().filter(|row| row.starts_with("run #")).collect();
        assert_eq!(feed.len(), FEED_SHOWN, "the feed is bounded: {feed:?}");
        assert!(
            feed[0].starts_with(&format!("run #{}", FEED_SHOWN + 4)),
            "{feed:?}"
        );
        assert!(feed[FEED_SHOWN - 1].starts_with("run #5"), "{feed:?}");
    }

    /// The cursor walks destinations and skips headings, and Enter opens what
    /// it is on — a DM for a member, the room itself for a room.
    #[test]
    fn enter_opens_a_member_dm_and_a_room() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", AgentKind::Crew);
        chat.session
            .channels
            .create("parser", vec!["scout".to_string()], ChannelMode::Free)
            .expect("room created");
        chat.refresh_conversations();

        chat.open_directory();
        assert_eq!(
            chat.directory_targets(),
            vec![
                DirTarget::Member(HUB_NAME.to_string()),
                DirTarget::Member("scout".to_string()),
                DirTarget::Room("parser".to_string())
            ],
            "headings are not destinations, and main leads the roster (D100)"
        );
        chat.directory_key(KeyCode::Down, KeyModifiers::NONE);
        chat.directory_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(*chat.buffers.active(), BufferId::Dm("scout".to_string()));
        assert!(chat.directory.is_none(), "opening closes the directory");

        chat.open_directory();
        chat.directory_key(KeyCode::Down, KeyModifiers::NONE);
        chat.directory_key(KeyCode::Down, KeyModifiers::NONE);
        chat.directory_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            *chat.buffers.active(),
            BufferId::Channel("parser".to_string()),
            "a room the user is not in opens all the same"
        );
    }

    /// Main is a participant, so it is on the roster — first, present, with the
    /// rooms it is in — and Enter on it goes to the console rather than to a DM
    /// with an instance that does not exist.
    #[test]
    fn main_leads_the_roster_and_enter_on_it_opens_the_console() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", AgentKind::Crew);
        chat.session
            .channels
            .create(
                "build",
                vec![HUB_NAME.to_string(), "scout".to_string()],
                ChannelMode::Free,
            )
            .expect("room created");
        chat.refresh_conversations();
        chat.open_directory();

        let rows = texts(&chat);
        assert_eq!(rows[0], "Team", "{rows:?}");
        assert_eq!(
            rows[1], "○ main · console · idle · #build",
            "main is the first row, with its presence, its label and its rooms"
        );
        assert!(
            rows.iter().any(|row| row.contains("scout")),
            "and the teammates follow it: {rows:?}"
        );

        // Presence is the host turn, not a registry state.
        chat.busy = true;
        assert_eq!(texts(&chat)[1], "● main · console · running · #build");

        chat.switch_to(BufferId::Dm("scout".to_string()));
        chat.open_directory();
        chat.directory_key(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            *chat.buffers.active(),
            BufferId::Hub,
            "main's conversation is the console, not a DM with an instance"
        );
    }

    /// `o` opens the member's observation page — the same record `tab` reaches
    /// from a conversation — and it works on main's row too. On a room it does
    /// nothing: a room has no protagonist.
    #[test]
    fn o_opens_the_record_of_the_member_under_the_cursor() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", AgentKind::Crew);
        chat.session
            .channels
            .create("parser", vec!["scout".to_string()], ChannelMode::Free)
            .expect("room created");
        chat.refresh_conversations();

        chat.open_directory();
        assert!(chat.directory_key(KeyCode::Char('o'), KeyModifiers::NONE));
        assert_eq!(chat.open_perspective.as_deref(), Some(HUB_NAME));
        assert!(chat.directory.is_none(), "the page takes the screen");

        chat.open_perspective = None;
        chat.open_directory();
        chat.directory_key(KeyCode::Down, KeyModifiers::NONE);
        chat.directory_key(KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(chat.open_perspective.as_deref(), Some("scout"));

        chat.open_perspective = None;
        chat.open_directory();
        chat.directory_key(KeyCode::Down, KeyModifiers::NONE);
        chat.directory_key(KeyCode::Down, KeyModifiers::NONE);
        chat.directory_key(KeyCode::Char('o'), KeyModifiers::NONE);
        assert_eq!(chat.open_perspective, None, "a room has no protagonist");
        assert!(chat.directory.is_some(), "and the panel stays open");
    }

    /// The footer names the keys the panel actually has, `o` included.
    #[test]
    fn the_footer_names_the_record_door() {
        let mut chat = test_chat();
        chat.open_directory();
        let text = chat
            .directory_view_rows(100)
            .iter()
            .map(|row| row.line.plain_text())
            .collect::<Vec<String>>()
            .join("\n");
        assert!(text.contains("o record"), "{text}");
        assert!(text.contains("Enter open"), "{text}");
    }

    /// `j` joins the room under the cursor: the roster changes, the room becomes
    /// one of the user's conversations, and the panel stays open so the mark
    /// flipping is the confirmation.
    #[test]
    fn j_joins_the_room_under_the_cursor() {
        let mut chat = test_chat();
        chat.session
            .channels
            .create("parser", vec!["scout".to_string()], ChannelMode::Free)
            .expect("room created");
        chat.refresh_conversations();
        assert!(
            chat.buffers
                .get(&BufferId::Channel("parser".to_string()))
                .is_none(),
            "a room the user is not in is not one of their conversations"
        );

        chat.open_directory();
        // Past main's row, which now leads the roster (D100).
        chat.directory_key(KeyCode::Down, KeyModifiers::NONE);
        chat.directory_key(KeyCode::Char('j'), KeyModifiers::NONE);

        assert!(
            chat.session.channels.is_member("parser", USER_NAME),
            "the user is on the roster"
        );
        assert!(chat.directory.is_some(), "the panel stays open");
        assert!(
            texts(&chat)
                .iter()
                .any(|row| row.starts_with("#parser") && !row.contains("you're not in")),
            "and the mark flipped"
        );
        assert!(
            chat.buffers
                .get(&BufferId::Channel("parser".to_string()))
                .is_some(),
            "the room is in the bar"
        );
    }

    /// Agent operations stay in the ctrl+b manager (recorded scope): the
    /// directory navigates and informs, and `x` here is not a kill.
    #[test]
    fn the_directory_does_not_stop_agents() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout", AgentKind::Crew);
        chat.open_directory();
        chat.directory_key(KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(
            chat.session
                .agents
                .list()
                .iter()
                .any(|status| status.name == "scout"),
            "the agent is untouched"
        );
        assert!(chat.directory.is_some(), "and the panel is still open");
    }

    /// An empty session still renders three sections rather than an empty box:
    /// "nothing yet" is an answer, a blank panel is a bug report.
    #[test]
    fn an_empty_directory_still_says_what_it_would_have_shown() {
        let mut chat = test_chat();
        chat.open_directory();
        let rows = texts(&chat);
        assert!(rows.iter().any(|row| row == "no teammates yet"), "{rows:?}");
        assert!(rows.iter().any(|row| row == "no rooms yet"), "{rows:?}");
        assert!(rows.iter().any(|row| row == "nothing yet"), "{rows:?}");
        assert!(!chat.directory_view_rows(80).is_empty());
    }

    #[test]
    fn a_member_in_many_rooms_counts_the_rest() {
        assert_eq!(rooms_label(&[]), "");
        assert_eq!(rooms_label(&["a".to_string()]), "#a");
        let many: Vec<String> = (0..6).map(|i| format!("r{i}")).collect();
        assert_eq!(rooms_label(&many), "#r0 #r1 #r2 +3");
    }
}

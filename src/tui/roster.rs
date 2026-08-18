//! The roster (v6): every conversation as a row under the composer, the way
//! the user's own screenshot of Claude Code has it — `● main` first, then the
//! agents, then the rooms, at most three rows with the selection scrolling
//! the window.
//!
//! This is the status layer's third body. D104 built the tree (shift+↑/↓, a
//! panel above the composer), D115 hung the badges on it; the user's v6
//! ruling replaces the panel with a **constant** presence: the rows are
//! always there once a conversation exists, and the one gesture is `↓` from
//! the composer — the CC fallthrough (cursor → history → rows), no chord, no
//! new key. `Enter` switches to the page under the cursor, `↑` off the top
//! returns to the composer, `Esc` drops the selection, `k` stops a running
//! agent (the tree's own key, kept), and any printable character gives the
//! keyboard back to the draft (CC's type-to-exit).
//!
//! The rows read the same stores the tree read — the registry for state and
//! cost, [`crate::tui::tree::status_label`] for the wording, `badge_of` for
//! the two badge tiers — so a row says what its tree row said, one line at a
//! time, flat.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::Color;

use crate::channels::{MAIN_NAME, USER_NAME};
use crate::tui::buffer::BufferId;
use crate::tui::chat::{Chat, Row};
use crate::tui::line::{Line, SegStyle, text_width};
use crate::tui::tree::{duration_label, status_label};
use crate::tui::zoom::ZoomTarget;

/// Rows on screen at once. The selection scrolls the window; edge rows carry
/// a dim `N more` marker for what is folded past them.
pub(crate) const ROSTER_WINDOW: usize = 3;

/// One row of the roster, resolved from the stores each frame.
struct Entry {
    /// Where `Enter` goes: `None` is home.
    target: Option<ZoomTarget>,
    /// `@main`, `@scout`, `#crew`.
    label: String,
    /// `●` running, `○` idle, `·` stopped.
    dot: char,
    dot_color: Color,
    /// The status text after the name — the tree's own wording.
    detail: String,
    /// The detail in the accent (waiting on you).
    urgent: bool,
    /// (unread, mention) for the badge, D115's grammar.
    badge: (u64, bool),
    /// A running agent `k` can stop.
    stoppable: bool,
}

/// A debt leads the row and the state follows it: the debt is what the user is
/// looking for, and the state is what explains it. Either half alone is the
/// whole detail.
fn join_detail(owed: Option<String>, state: String) -> String {
    match (owed, state.is_empty()) {
        (Some(owed), true) => owed,
        (Some(owed), false) => format!("{owed} · {state}"),
        (None, _) => state,
    }
}

impl Chat {
    fn roster_entries(&self) -> Vec<Entry> {
        let now = std::time::Instant::now();
        let asking = self.asking_instance();
        let mut out = Vec::new();
        let palette = crate::tui::avatar::Palette::new(&self.theme);
        // Main first, always — the row the leader gets in the screenshot.
        out.push(Entry {
            target: None,
            label: format!("@{MAIN_NAME}"),
            dot: if self.conv.busy { '●' } else { '○' },
            dot_color: if self.conv.busy {
                palette.presence_on
            } else {
                palette.presence_off
            },
            detail: if self.conv.busy {
                format!("{}…", self.conv.turn_verb)
            } else {
                String::new()
            },
            urgent: false,
            badge: self.badge_of(&BufferId::Hub),
            stoppable: false,
        });
        for status in self.tree_instances() {
            let stopped = status.state == crate::agents::AgentState::Stopped;
            let running = status.state == crate::agents::AgentState::Running;
            let waiting = asking.as_deref() == Some(status.name.as_str());
            let owed = self.owed_by(&status.name);
            out.push(Entry {
                target: Some(ZoomTarget::Agent(status.name.clone())),
                label: format!("@{}", status.name),
                dot: if running {
                    '●'
                } else if stopped {
                    '·'
                } else {
                    '○'
                },
                dot_color: if running {
                    palette.presence_on
                } else {
                    palette.presence_off
                },
                detail: if waiting {
                    "waiting on you (permission)".to_string()
                } else {
                    join_detail(owed, status_label(&status, now))
                },
                urgent: waiting,
                badge: self.badge_of(&BufferId::Dm(status.name.clone())),
                stoppable: running,
            });
        }
        for room in self.tree_rooms() {
            let members = room.members.len();
            let owed = self.session.channels.owed_in(&room.name);
            let oldest = owed.first();
            out.push(Entry {
                target: Some(ZoomTarget::Room(room.name.clone())),
                label: format!("#{}", room.name),
                dot: '○',
                dot_color: palette.presence_off,
                detail: match oldest {
                    Some(mention) => format!(
                        "waiting on {} · {}",
                        Self::owed_target(&mention.to),
                        duration_label(std::time::Duration::from_secs(
                            crate::tui::buffer::now().saturating_sub(mention.at)
                        ))
                    ),
                    None => format!("{members} member{}", if members == 1 { "" } else { "s" }),
                },
                // The accent means *you* are the holdup, and via R7 the user's
                // half of a room is main. A room waiting on a member is news,
                // not a prompt, so it says so without the colour.
                urgent: oldest.is_some_and(|m| m.to == MAIN_NAME || m.to == USER_NAME),
                badge: self.badge_of(&BufferId::Channel(room.name.clone())),
                stoppable: false,
            });
        }
        out
    }

    /// The oldest `@` this member has not answered, as its row says it (v7
    /// batch 3): which room, which message, and — the part that separates a
    /// member that has not looked yet from one that looked and said nothing —
    /// whether the line is even in its context.
    fn owed_by(&self, name: &str) -> Option<String> {
        let standing = self
            .session
            .channels
            .standing_of(name)
            .into_iter()
            .find(|s| s.owes.is_some())?;
        let mention = standing.owes.as_ref()?;
        let unread = standing.read_to < mention.seq;
        Some(format!(
            "owes #{} #{}{}",
            standing.room,
            mention.seq,
            if unread { " · unread" } else { "" }
        ))
    }

    /// `@dev`, or the room itself for an `@all` — which is owed one covered
    /// answer rather than one answer each (R4).
    fn owed_target(to: &str) -> String {
        if to == crate::channels::ALL_NAME {
            "the room".to_string()
        } else {
            format!("@{to}")
        }
    }

    /// How many rows the roster has — zero keeps it (and its keys) out of the
    /// way entirely: a session with no agents is the plain console.
    pub(crate) fn roster_len(&self) -> usize {
        if self.tree_instances().is_empty() && self.tree_rooms().is_empty() {
            return 0;
        }
        1 + self.tree_instances().len() + self.tree_rooms().len()
    }

    /// The row the cursor is on, clamped against a roster that shrank.
    pub(crate) fn roster_selection(&self) -> Option<usize> {
        let len = self.roster_len();
        self.roster_sel.map(|sel| sel.min(len.saturating_sub(1)))
    }

    /// `↓` at the bottom of history: fall into the rows (CC's fallthrough).
    pub(crate) fn roster_enter_selection(&mut self) -> bool {
        if self.roster_len() == 0 {
            return false;
        }
        self.roster_sel = Some(0);
        self.dirty = true;
        true
    }

    /// Keys while a row is selected. Everything unhandled falls back to the
    /// caller — and any printable character is the composer's (type-to-exit).
    pub(crate) fn roster_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(sel) = self.roster_selection() else {
            return false;
        };
        if modifiers.contains(KeyModifiers::CONTROL) || modifiers.contains(KeyModifiers::ALT) {
            return false;
        }
        match code {
            KeyCode::Down => {
                let len = self.roster_len();
                self.roster_sel = Some((sel + 1).min(len.saturating_sub(1)));
                self.dirty = true;
                true
            }
            KeyCode::Up => {
                // Off the top is back to the composer — CC's `footer:up`.
                if sel == 0 {
                    self.roster_sel = None;
                } else {
                    self.roster_sel = Some(sel - 1);
                }
                self.dirty = true;
                true
            }
            KeyCode::Enter => {
                let target = self
                    .roster_entries()
                    .get(sel)
                    .map(|entry| entry.target.clone());
                self.roster_sel = None;
                match target {
                    Some(Some(target)) => self.switch_to(Some(target)),
                    Some(None) if !self.active.is_main() => self.switch_to(None),
                    _ => {}
                }
                self.dirty = true;
                true
            }
            KeyCode::Esc => {
                self.roster_sel = None;
                self.dirty = true;
                true
            }
            KeyCode::Char('k') => {
                let entry = self.roster_entries().into_iter().nth(sel);
                if let Some(Entry {
                    stoppable: true,
                    target: Some(ZoomTarget::Agent(name)),
                    ..
                }) = entry
                {
                    self.stop_agent(&name);
                }
                true
            }
            // Type-to-exit: the draft takes the keyboard back (CC
            // `PromptInput.tsx:1898-1902`); the caller re-dispatches.
            KeyCode::Char(_) | KeyCode::Backspace => {
                self.roster_sel = None;
                self.dirty = true;
                false
            }
            _ => false,
        }
    }

    /// The rows, windowed to [`ROSTER_WINDOW`] around the selection.
    pub(crate) fn roster_rows(&self, width: usize) -> Vec<Row> {
        let entries = self.roster_entries();
        if self.roster_len() == 0 {
            return Vec::new();
        }
        let sel = self.roster_selection();
        let len = entries.len();
        let window = ROSTER_WINDOW.min(len);
        // The window follows the cursor with one row of context, and rests at
        // the top — main visible — when nothing is selected.
        let start = sel
            .map(|s| s.saturating_sub(1).min(len - window))
            .unwrap_or(0);
        let theme = &self.theme;
        let mut rows = Vec::new();
        for (i, entry) in entries.iter().enumerate().skip(start).take(window) {
            let selected = sel == Some(i);
            let active = match &entry.target {
                None => self.active.is_main(),
                Some(target) => self.zoom.as_ref() == Some(target),
            };
            let mut line = Line::styled(
                if selected { "❯ " } else { "  " }.to_string(),
                SegStyle::fg(theme.claude).bold(),
            );
            line.push_styled(format!("{} ", entry.dot), SegStyle::fg(entry.dot_color));
            let name_color = self.identity_color(entry.label.trim_start_matches(['@', '#']));
            let name = SegStyle::fg(name_color);
            line.push_styled(
                entry.label.clone(),
                if active || selected {
                    name.bold()
                } else {
                    name
                },
            );
            let mut badge_budget = width;
            crate::tui::tree::push_badge(&mut line, &mut badge_budget, entry.badge, theme);
            if !entry.detail.is_empty() {
                let detail = crate::tui::chat::one_line(
                    &entry.detail,
                    width.saturating_sub(text_width(&line.plain_text()) + 2),
                );
                let style = if entry.urgent {
                    SegStyle::fg(theme.claude).bold()
                } else {
                    SegStyle::fg(theme.text_secondary)
                };
                line.push_styled(format!(": {detail}"), style);
            }
            // Edge rows say what the window folded past them.
            let folded = if i == start && start > 0 {
                Some(format!("↑ {start} more"))
            } else if i + 1 == start + window && start + window < len {
                Some(format!("↓ {} more", len - start - window))
            } else {
                None
            };
            if let Some(marker) = folded {
                crate::tui::line::push_right(&mut line, &marker, theme.muted(), width, 2);
            }
            rows.push(Row::new(line));
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;
    use crate::tui::test_util::chat_at;

    fn test_chat() -> Chat {
        chat_at(100, 40)
    }

    fn seed(chat: &Chat, name: &str) {
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
    }

    fn texts(chat: &Chat) -> Vec<String> {
        chat.roster_rows(100)
            .iter()
            .map(|r| r.line.plain_text().trim_end().to_string())
            .collect()
    }

    /// The screenshot's shape: main first, the agents after it, each row a
    /// presence dot, the name, and the status copy — and no rows at all
    /// before a conversation exists.
    #[test]
    fn the_rows_lead_with_main_and_wear_the_status_copy() {
        let mut chat = test_chat();
        assert!(texts(&chat).is_empty(), "no conversations, no furniture");
        seed(&chat, "scout");
        chat.session
            .channels
            .create(
                "crew",
                vec![crate::channels::USER_NAME.into(), "scout".into()],
                crate::channels::ChannelMode::Free,
            )
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        chat.refresh_conversations();
        let rows = texts(&chat);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(rows[0].contains("@main"), "{rows:?}");
        assert!(
            rows[1].contains("● @scout"),
            "a running agent wears the filled dot: {rows:?}"
        );
        assert!(
            rows[2].contains("#crew") && rows[2].contains("2 members"),
            "{rows:?}"
        );
        // The page's row is the roster's bold cursor for "you are here";
        // switching marks it.
        chat.switch_to(Some(ZoomTarget::Agent("scout".into())));
        assert!(!chat.active.is_main());
        let _ = chat.page_turn;
    }

    /// The window is three rows, scrolled by the selection, with the folded
    /// count on the edge rows.
    #[test]
    fn the_window_is_three_rows_and_follows_the_cursor() {
        let mut chat = test_chat();
        for name in ["a", "b", "c", "d"] {
            seed(&chat, name);
        }
        chat.refresh_conversations();
        assert_eq!(chat.roster_len(), 5);
        let rows = texts(&chat);
        assert_eq!(rows.len(), ROSTER_WINDOW, "{rows:?}");
        assert!(rows[0].contains("@main"), "resting window starts at main");
        assert!(
            rows[2].contains("more"),
            "the edge row counts what is folded: {rows:?}"
        );

        chat.roster_sel = Some(4);
        let rows = texts(&chat);
        assert!(
            rows.iter().any(|r| r.contains("❯") && r.contains("@d")),
            "the window followed the cursor to the last row: {rows:?}"
        );
        assert!(
            rows[0].contains("more"),
            "and the fold marker moved to the top edge: {rows:?}"
        );
    }

    /// The fallthrough: `↓` with history at its end lands on the rows; `↑`
    /// off the top comes back; a printable character is the composer's again.
    #[test]
    fn down_falls_in_up_comes_back_and_typing_leaves() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.refresh_conversations();
        assert!(chat.roster_selection().is_none());
        chat.on_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(
            chat.roster_selection(),
            Some(0),
            "↓ at the bottom of history falls into the rows"
        );
        chat.on_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(chat.roster_selection(), Some(1));
        chat.on_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(chat.roster_selection(), Some(1), "the last row holds");
        chat.on_key(KeyCode::Up, KeyModifiers::NONE);
        chat.on_key(KeyCode::Up, KeyModifiers::NONE);
        assert!(
            chat.roster_selection().is_none(),
            "↑ off the top returns to the composer"
        );

        assert!(chat.roster_enter_selection());
        chat.on_key(KeyCode::Char('h'), KeyModifiers::NONE);
        assert!(
            chat.roster_selection().is_none(),
            "a printable character gives the keyboard back"
        );
        assert_eq!(chat.input, "h", "and lands in the draft");
    }

    /// `k` stops the running agent under the cursor and nothing else.
    #[test]
    fn k_stops_only_the_running_row_under_the_cursor() {
        use crossterm::event::{KeyCode, KeyModifiers};
        let mut chat = test_chat();
        seed(&chat, "scout");
        chat.refresh_conversations();
        assert!(chat.roster_enter_selection());
        // On main's row, `k` is nothing.
        chat.on_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert!(
            chat.session
                .agents
                .list()
                .iter()
                .all(|s| s.state == crate::agents::AgentState::Running),
            "main's row has nothing to stop"
        );
        chat.on_key(KeyCode::Down, KeyModifiers::NONE);
        chat.on_key(KeyCode::Char('k'), KeyModifiers::NONE);
        assert!(
            chat.session
                .agents
                .list()
                .iter()
                .any(|s| s.name == "scout" && s.state == crate::agents::AgentState::Stopped),
            "the row under the cursor stopped"
        );
    }

    // -----------------------------------------------------------------------
    // The `@` ledger on the rows (v7 batch 3)
    // -----------------------------------------------------------------------

    /// The four situations a silent member could be in used to look identical
    /// (v7's table): not read yet, read and working, read and not answering, and
    /// dead. The row now separates them — the debt says an answer is owed, the
    /// cursor says whether the line has even reached the member, and the state
    /// that was always there explains the rest.
    #[test]
    fn a_row_says_what_a_member_owes_and_whether_it_has_looked() {
        let mut chat = test_chat();
        seed(&chat, "dev");
        chat.session
            .channels
            .create(
                "build",
                vec!["dev".to_string(), "qa".to_string()],
                crate::channels::ChannelMode::Free,
            )
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        chat.session
            .channels
            .post("qa", "build", "@dev is the lexer done?")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        chat.refresh_conversations();

        let unread = texts(&chat);
        assert!(
            unread.iter().any(|r| r.contains("@dev")
                && r.contains("owes #build #1")
                && r.contains("unread")),
            "owed and not yet in its context: {unread:?}"
        );

        chat.session.channels.mark_seen("dev", "build", 1);
        chat.session.channels.settle_now();
        let read = texts(&chat);
        assert!(
            read.iter().any(|r| r.contains("@dev")
                && r.contains("owes #build #1")
                && !r.contains("unread")),
            "read it and still owes it — the row that used to be invisible: {read:?}"
        );

        chat.session
            .channels
            .post("dev", "build", "not yet, two cases left")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let answered = texts(&chat);
        assert!(
            !answered.iter().any(|r| r.contains("owes #build")),
            "answering clears it: {answered:?}"
        );
    }

    /// The room's own row says what it is waiting on, and for how long — the
    /// slot a messenger fills with "delivered / read" and bingo had nothing in.
    #[test]
    fn a_rooms_row_says_who_it_is_waiting_on() {
        let mut chat = test_chat();
        seed(&chat, "dev");
        chat.session
            .channels
            .create(
                "build",
                vec![
                    "dev".to_string(),
                    "qa".to_string(),
                    crate::channels::USER_NAME.to_string(),
                ],
                crate::channels::ChannelMode::Free,
            )
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        chat.refresh_conversations();
        assert!(
            texts(&chat)
                .iter()
                .any(|r| r.contains("#build") && r.contains("member")),
            "a quiet room reports its size"
        );

        chat.session
            .channels
            .post("qa", "build", "@dev status?")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let waiting = texts(&chat);
        assert!(
            waiting
                .iter()
                .any(|r| r.contains("#build") && r.contains("waiting on @dev")),
            "the room names who it is blocked on: {waiting:?}"
        );

        chat.session
            .channels
            .post("qa", "build", "@all anyone?")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        chat.session
            .channels
            .post("dev", "build", "here")
            .now()
            .unwrap_or_else(|e| panic!("{e}"));
        let quiet = texts(&chat);
        assert!(
            !quiet.iter().any(|r| r.contains("waiting on")),
            "both debts settled by the one answer (R4): {quiet:?}"
        );
    }
}

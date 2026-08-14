//! The rewind selector (D91): two stages over the hub's own history.
//!
//! esc-esc on an empty composer opens it — the one press Esc had no answer for
//! since D80 reserved the slot. Stage one lists the turns the user opened and
//! can still go back to; stage two asks what "back" means for the chosen one.
//! Claude Code's five options, in Claude Code's words, because the question is
//! the same question and a second vocabulary for it would help nobody.
//!
//! The overlay owns no history of its own: it reads the session's projection
//! when it opens and re-reads it before it acts, so a list built a minute ago
//! can never truncate something else than what it named.

use crossterm::event::{KeyCode, KeyModifiers};

use super::*;
use crate::rewind::Checkpoint;
use crate::tui::line::{Line, SegStyle};

/// Turns offered. Deep history is what `/resume` and the transcript are for;
/// a rewind list is a list of recent regrets.
pub const REWIND_MAX: usize = 50;

/// The five answers, in Claude Code's wording.
pub const ACTIONS: [&str; 5] = [
    "Restore code and conversation",
    "Restore conversation",
    "Restore code",
    "Summarize from here",
    "Never mind",
];

/// State lines the rewind flow leaves behind, marked so they render as a state
/// and not as something the user said.
pub(crate) const REWIND_PREFIX: &str = "⏪ ";

pub(crate) fn is_rewind_line(text: &str) -> bool {
    text.starts_with(REWIND_PREFIX)
}

/// Which half of an action is available for the chosen checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offered {
    /// Files were snapshotted at or after this point.
    pub code: bool,
    /// The message is still in history verbatim.
    pub conversation: bool,
    /// There is a message before this turn for a summary to hang off.
    pub summarize: bool,
}

impl Offered {
    /// Whether the option at `index` can be chosen at all.
    pub fn allows(&self, index: usize) -> bool {
        match index {
            0 => self.code && self.conversation,
            1 => self.conversation,
            2 => self.code,
            3 => self.summarize && self.conversation,
            _ => true,
        }
    }

    /// Why it cannot, in the parenthetical the dimmed row carries.
    fn refusal(&self, index: usize) -> &'static str {
        match index {
            0 | 2 if !self.code => "no files recorded for this turn",
            3 if !self.summarize => "nothing before this turn to summarize into",
            _ => "no longer in this conversation",
        }
    }
}

/// The open selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rewind {
    /// The turns on offer, newest first.
    pub points: Vec<Checkpoint>,
    /// Index into `points`.
    pub selected: usize,
    /// `None` while picking a turn; `Some(action)` while picking what to do.
    pub action: Option<usize>,
}

impl Rewind {
    fn point(&self) -> Option<&Checkpoint> {
        self.points.get(self.selected)
    }
}

impl super::Chat {
    /// What the chosen checkpoint can offer right now.
    pub(crate) fn rewind_offered(&self, point: &Checkpoint) -> Offered {
        Offered {
            code: point.coverage.files > 0,
            conversation: self.rewind_still_present(point),
            summarize: self.rewind_cut_before(point).is_some(),
        }
    }

    /// The session's projection, or nothing when there is no transcript.
    fn rewind_entries(&self) -> Vec<crate::transcript::Entry> {
        self.session
            .runtime
            .transcript
            .borrow()
            .clone()
            .and_then(|transcript| transcript.load_projection().ok())
            .unwrap_or_default()
    }

    /// This session's snapshot store.
    fn rewind_dir(&self) -> Option<std::path::PathBuf> {
        let transcript = self.session.runtime.transcript.borrow().clone()?;
        Some(crate::rewind::session_dir(
            &self.session.home,
            &transcript.name(),
        ))
    }

    /// Whether the checkpoint is still a turn-opening message in history — the
    /// one fact the list cannot cache, because a compaction between opening the
    /// selector and confirming it would have folded the message into a summary.
    fn rewind_still_present(&self, point: &Checkpoint) -> bool {
        self.rewind_entries()
            .iter()
            .any(|entry| entry.line == Some(point.line) && entry.opens_turn.is_some())
    }

    /// The line a summary of this turn onwards would be appended after: the
    /// message before the turn opened. `None` when the turn is the oldest thing
    /// left in history, where there is no "from here" to summarize into.
    fn rewind_cut_before(&self, point: &Checkpoint) -> Option<usize> {
        let entries = self.rewind_entries();
        let index = entries
            .iter()
            .position(|entry| entry.line == Some(point.line))?;
        entries.get(index.checked_sub(1)?)?.line
    }

    /// Open the selector. Inert behind a dialog, and refused outright while a
    /// turn is running — rewinding under a turn that is still writing files
    /// would race the snapshots it is still taking.
    pub(crate) fn open_rewind(&mut self) {
        if self.pending_ask.is_some() {
            return;
        }
        if self.busy {
            self.push_slash_info("finish or interrupt the turn first".to_string());
            return;
        }
        let Some(dir) = self.rewind_dir() else {
            self.push_slash_info("this session has no transcript; nothing to rewind".to_string());
            return;
        };
        let points = crate::rewind::checkpoints_of(&self.rewind_entries(), &dir, REWIND_MAX);
        if points.is_empty() {
            self.push_slash_info("no turns to rewind to yet".to_string());
            return;
        }
        self.close_menus();
        self.rewind = Some(Rewind {
            points,
            selected: 0,
            action: None,
        });
        self.clear_slash_suggestions();
        self.dirty = true;
    }

    /// Keys, while the selector is open. Modal, like the switcher: an open
    /// chooser swallows what it does not understand rather than letting it edit
    /// the draft underneath.
    pub(crate) fn rewind_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.pending_ask.is_some() {
            return false;
        }
        let Some(mut state) = self.rewind.take() else {
            return false;
        };
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        self.dirty = true;
        match code {
            // Ctrl+C skips every layer and always means out (D80).
            KeyCode::Char('c') if ctrl => {
                self.rewind = Some(state);
                return false;
            }
            // One press, one level: the action list returns to the turn list
            // before the selector closes, the way `/model` peels its two.
            KeyCode::Esc => {
                if state.action.is_some() {
                    state.action = None;
                    self.rewind = Some(state);
                }
                return true;
            }
            KeyCode::Down => match &mut state.action {
                Some(action) => *action = (*action + 1).min(ACTIONS.len() - 1),
                None => state.selected = (state.selected + 1).min(state.points.len() - 1),
            },
            KeyCode::Up => match &mut state.action {
                Some(action) => *action = action.saturating_sub(1),
                None => state.selected = state.selected.saturating_sub(1),
            },
            KeyCode::Char(c) if c.is_ascii_digit() && !ctrl && state.action.is_some() => {
                if let Some(n) = c.to_digit(10)
                    && (1..=ACTIONS.len()).contains(&(n as usize))
                {
                    state.action = Some(n as usize - 1);
                }
            }
            KeyCode::Enter => match state.action {
                None => state.action = Some(0),
                Some(action) => {
                    let Some(point) = state.point().cloned() else {
                        return true;
                    };
                    if !self.rewind_offered(&point).allows(action) {
                        // A dimmed row is inert: the press is swallowed so it
                        // cannot fall through and edit the draft underneath.
                        self.rewind = Some(state);
                        return true;
                    }
                    self.rewind_apply(&point, action);
                    return true;
                }
            },
            _ => {}
        }
        self.rewind = Some(state);
        true
    }

    /// Carry out the chosen action. Everything is re-checked here: the list was
    /// built when the overlay opened, and only what is true now may be acted on.
    fn rewind_apply(&mut self, point: &Checkpoint, action: usize) {
        self.rewind = None;
        self.dirty = true;
        if action == 4 {
            return;
        }
        if self.busy {
            self.push_slash_info("finish or interrupt the turn first".to_string());
            return;
        }
        if self.pinned_panels.iter().any(|(id, _)| id == "compact") {
            self.push_slash_info("wait for compaction to finish first".to_string());
            return;
        }
        let (Some(transcript), Some(dir)) = (
            self.session.runtime.transcript.borrow().clone(),
            self.rewind_dir(),
        ) else {
            self.push_slash_info("this session has no transcript; nothing to rewind".to_string());
            return;
        };
        if action == 3 {
            self.rewind_summarize(point);
            return;
        }

        // Code first: a conversation restored over files that failed to come
        // back would describe a state the disk is not in.
        let mut lines: Vec<String> = Vec::new();
        if action == 0 || action == 2 {
            match crate::rewind::restore(&dir, point.line) {
                Ok(restored) => {
                    lines.push(format!(
                        "{REWIND_PREFIX}restored {} file{}",
                        restored.len(),
                        if restored.len() == 1 { "" } else { "s" }
                    ));
                    lines.extend(restored.iter().take(RESTORE_LIST_MAX).map(|file| {
                        format!(
                            "   {} {}",
                            if file.removed { "removed" } else { "reverted" },
                            file.path.display()
                        )
                    }));
                    if restored.len() > RESTORE_LIST_MAX {
                        lines.push(format!("   … {} more", restored.len() - RESTORE_LIST_MAX));
                    }
                }
                Err(error) => {
                    self.push_slash_error(format!(
                        "[error] rewind could not restore files: {error}"
                    ));
                    return;
                }
            }
        }
        if action == 0 || action == 1 {
            if !self.rewind_still_present(point) {
                self.push_slash_info(
                    "that turn is no longer in this conversation; nothing was changed".to_string(),
                );
                return;
            }
            if let Err(error) = transcript.truncate_at_line(point.line) {
                self.push_slash_error(format!(
                    "[error] rewind could not rewrite the session: {error}"
                ));
                return;
            }
            // The message goes back into the composer, where the user left it —
            // rewinding to a turn is almost always about asking it differently.
            self.set_input(point.text.clone());
            self.queued.clear();
            let stamp = crate::tui::buffer::stamp(point.at);
            lines.push(match action {
                0 => format!("{REWIND_PREFIX}code and conversation restored to {stamp}"),
                _ => format!("{REWIND_PREFIX}conversation restored to {stamp}"),
            });
        }
        // The turns those snapshots belong to are gone from the conversation,
        // so the pre-images they hold address nothing any more.
        crate::rewind::drop_from(&dir, point.line);
        self.refresh_context_usage_from_transcript();
        self.push_user_line(lines.join("\n"));
    }

    /// Summarize this turn and everything after it, in place of them. The cut
    /// lands *before* the turn opened and the summary is appended after it, so
    /// the compaction marker's own format is not borrowed for something it does
    /// not mean — its wording is a contract about a prefix, and this is a tail.
    fn rewind_summarize(&mut self, point: &Checkpoint) {
        let Some(cut) = self.rewind_cut_before(point) else {
            self.push_slash_info("nothing before this turn to summarize into".to_string());
            return;
        };
        let Some(transcript) = self.session.runtime.transcript.borrow().clone() else {
            return;
        };
        let Some(dir) = self.rewind_dir() else {
            return;
        };
        let session = self.session.clone();
        let events = self.events.clone();
        let line = point.line;
        let stamp = crate::tui::buffer::stamp(point.at);
        self.pin_panel(
            "rewind",
            vec!["⏳ summarizing the turns from here…".to_string()],
        );
        tokio::spawn(async move {
            let unpin = || {
                let _ = events.send(crate::ui::UiEvent::Unpin {
                    id: "rewind".to_string(),
                });
            };
            let entries = transcript.load_projection().unwrap_or_default();
            let tail: Vec<crate::api::types::Message> = entries
                .iter()
                .skip_while(|entry| entry.line != Some(line))
                .map(|entry| entry.message.clone())
                .collect();
            if tail.is_empty() {
                unpin();
                let _ = events.send(crate::ui::UiEvent::SlashInfo(
                    "that turn is no longer in this conversation; nothing was changed".to_string(),
                ));
                return;
            }
            let Some(summary) = crate::compact::summarize_slice(&session, &tail).await else {
                unpin();
                let _ = events.send(crate::ui::UiEvent::SlashError(
                    "[error] rewind could not summarize (model call failed).".to_string(),
                ));
                return;
            };
            if let Err(error) = crate::rewind::write_summary(&transcript, cut, &summary) {
                unpin();
                let _ = events.send(crate::ui::UiEvent::SlashError(format!(
                    "[error] rewind could not rewrite the session: {error}"
                )));
                return;
            }
            crate::rewind::drop_from(&dir, line);
            unpin();
            let _ = events.send(crate::ui::UiEvent::RewindDone(format!(
                "{REWIND_PREFIX}turns from {stamp} replaced by a summary"
            )));
        });
    }
}

/// Files named individually in the restore report before it says "and N more".
const RESTORE_LIST_MAX: usize = 8;

impl super::Chat {
    /// The overlay, in the frame the switcher and the agent manager share.
    pub(crate) fn rewind_rows(&self, width: usize) -> Vec<Row> {
        let Some(state) = &self.rewind else {
            return Vec::new();
        };
        let theme = &self.theme;
        let mut rows: Vec<Row> = Vec::new();
        match state.action {
            None => {
                rows.push(Row::new(Line::styled(
                    "Rewind to an earlier turn",
                    SegStyle::fg(theme.text).bold(),
                )));
                for (index, point) in state.points.iter().enumerate() {
                    let selected = index == state.selected;
                    let files = match point.coverage.files {
                        0 => "no files".to_string(),
                        1 => "1 file".to_string(),
                        n => format!("{n} files"),
                    };
                    let missed = match point.coverage.missed {
                        0 => String::new(),
                        n => format!(" (+{n} unsnapshotted)"),
                    };
                    let stamp = crate::tui::buffer::stamp(point.at);
                    rows.push(Row::new(Line::styled(
                        format!(
                            "{}{stamp} · {files}{missed} · {}",
                            if selected { "❯ " } else { "  " },
                            point.label
                        ),
                        SegStyle::fg(if selected {
                            theme.permission
                        } else {
                            theme.text
                        }),
                    )));
                }
                rows.push(Row::new(Line::styled(
                    "↑/↓ select · Enter choose · Esc close",
                    SegStyle::fg(theme.text_secondary),
                )));
            }
            Some(chosen) => {
                let offered = state
                    .point()
                    .map(|point| self.rewind_offered(point))
                    .unwrap_or(Offered {
                        code: false,
                        conversation: false,
                        summarize: false,
                    });
                rows.push(Row::new(Line::styled(
                    format!(
                        "Rewind to {}",
                        state.point().map(|p| p.label.as_str()).unwrap_or_default()
                    ),
                    SegStyle::fg(theme.text).bold(),
                )));
                for (index, action) in ACTIONS.iter().enumerate() {
                    let selected = index == chosen;
                    let allowed = offered.allows(index);
                    let why = match allowed {
                        true => String::new(),
                        false => format!(" ({})", offered.refusal(index)),
                    };
                    let color = match (allowed, selected) {
                        (false, _) => theme.text_secondary,
                        (true, true) => theme.permission,
                        (true, false) => theme.text,
                    };
                    rows.push(Row::new(Line::styled(
                        format!(
                            "{}{}. {action}{why}",
                            if selected { "❯ " } else { "  " },
                            index + 1
                        ),
                        SegStyle::fg(color),
                    )));
                }
                rows.push(Row::new(Line::styled(
                    "↑/↓ select · 1-5 jump · Enter confirm · Esc back",
                    SegStyle::fg(theme.text_secondary),
                )));
            }
        }
        crate::tui::chat::manager_box(rows, width, theme)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{ContentBlock, Message, Role};
    use crate::transcript::Transcript;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn home_for(tag: &str) -> PathBuf {
        let home =
            std::env::temp_dir().join(format!("bingo-rewind-ui-{tag}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&home);
        home
    }

    /// A hub chat whose session home — and so whose snapshot store — is this
    /// test's alone.
    fn chat_at_home(home: &Path) -> Chat {
        let mut session = (*crate::tui::test_util::test_session()).clone();
        session.home = home.to_path_buf();
        let (events, events_rx) = tokio::sync::mpsc::unbounded_channel();
        let (asks, asks_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut chat = Chat::new(
            Arc::new(session),
            events,
            events_rx,
            asks,
            asks_rx,
            crate::tui::theme::Theme::dark(),
            crate::tui::theme::ThemeSetting::Auto,
            None,
        );
        chat.width = 100;
        chat.height = 40;
        chat
    }

    fn attach(chat: &Chat, home: &Path, tag: &str) -> Transcript {
        let cwd = home.join(tag);
        let _ = std::fs::create_dir_all(&cwd);
        let transcript = crate::transcript::create(home, &cwd).unwrap();
        let _ = chat
            .session
            .runtime
            .transcript_tx
            .send(Some(transcript.clone()));
        transcript
    }

    fn turn(transcript: &Transcript, text: &str, id: &str) {
        transcript.append_turn(1_700_000_000).unwrap();
        transcript.append(&Message::user_text(text)).unwrap();
        transcript
            .append(&Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: id.to_string(),
                    name: "Edit".to_string(),
                    input: serde_json::json!({}),
                }],
            })
            .unwrap();
        transcript
            .append(&Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: id.to_string(),
                    content: serde_json::json!("ok"),
                    is_error: false,
                }],
            })
            .unwrap();
        transcript
            .append(&Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: format!("did {text}"),
                }],
            })
            .unwrap();
    }

    fn rows(chat: &Chat) -> Vec<String> {
        chat.rewind_rows(100)
            .into_iter()
            .map(|row| row.line.plain_text())
            .collect()
    }

    fn press(chat: &mut Chat, code: KeyCode) -> bool {
        chat.rewind_key(code, KeyModifiers::NONE)
    }

    fn state_lines(chat: &Chat) -> Vec<String> {
        chat.messages
            .iter()
            .filter(|m| is_rewind_line(&m.text))
            .map(|m| m.text.clone())
            .collect()
    }

    #[test]
    fn esc_esc_on_an_empty_composer_opens_the_selector() {
        let home = home_for("open");
        let mut chat = chat_at_home(&home);
        let transcript = attach(&chat, &home, "open");
        turn(&transcript, "first question", "t1");

        let t0 = std::time::Instant::now();
        assert!(chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0));
        assert!(chat.rewind.is_none(), "the first press only arms it");
        assert_eq!(chat.notice, Some("Press esc again to rewind"));
        assert!(chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0));
        assert!(chat.rewind.is_some(), "the second press opens it");
        assert_eq!(chat.notice, None);
    }

    #[test]
    fn a_second_esc_outside_the_window_only_rearms() {
        let home = home_for("window");
        let mut chat = chat_at_home(&home);
        let transcript = attach(&chat, &home, "window");
        turn(&transcript, "first question", "t1");

        let t0 = std::time::Instant::now();
        chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
        let late = t0 + crate::tui::chat::ESC_WINDOW + std::time::Duration::from_millis(1);
        chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), late);
        assert!(chat.rewind.is_none(), "the window had closed");
        assert_eq!(chat.notice, Some("Press esc again to rewind"));
    }

    #[test]
    fn a_non_empty_composer_still_clears_instead_of_rewinding() {
        let home = home_for("draft");
        let mut chat = chat_at_home(&home);
        let transcript = attach(&chat, &home, "draft");
        turn(&transcript, "first question", "t1");

        chat.set_input("half a thought");
        let t0 = std::time::Instant::now();
        chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
        assert_eq!(chat.notice, Some("Press esc again to clear"));
        chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
        assert_eq!(chat.input, "", "esc-esc still clears a draft");
        assert!(chat.rewind.is_none(), "and does not open the selector");
    }

    #[test]
    fn a_running_turn_refuses_the_selector() {
        let home = home_for("busy");
        let mut chat = chat_at_home(&home);
        let transcript = attach(&chat, &home, "busy");
        turn(&transcript, "first question", "t1");

        chat.busy = true;
        chat.open_rewind();
        assert!(chat.rewind.is_none());
        assert!(
            chat.slash_info_lines
                .iter()
                .any(|line| line.contains("finish or interrupt the turn first")),
            "{:?}",
            chat.slash_info_lines
        );
    }

    #[test]
    fn a_session_with_no_turns_says_so_rather_than_opening_empty() {
        let home = home_for("empty");
        let mut chat = chat_at_home(&home);
        attach(&chat, &home, "empty");
        chat.open_rewind();
        assert!(chat.rewind.is_none());
        assert!(
            chat.slash_info_lines
                .iter()
                .any(|line| line.contains("no turns to rewind to yet"))
        );
    }

    #[test]
    fn the_list_shows_turn_openers_newest_first_and_enter_asks_what_to_do() {
        let home = home_for("list");
        let mut chat = chat_at_home(&home);
        let transcript = attach(&chat, &home, "list");
        turn(&transcript, "first question", "t1");
        turn(&transcript, "second question", "t2");

        chat.open_rewind();
        let listed = rows(&chat);
        let first = listed
            .iter()
            .position(|row| row.contains("second question"))
            .unwrap();
        let second = listed
            .iter()
            .position(|row| row.contains("first question"))
            .unwrap();
        assert!(first < second, "newest first: {listed:?}");
        assert!(listed.iter().any(|row| row.contains("Esc close")));

        assert!(press(&mut chat, KeyCode::Enter));
        let actions = rows(&chat);
        for action in ACTIONS {
            assert!(
                actions.iter().any(|row| row.contains(action)),
                "{action} missing from {actions:?}"
            );
        }
        // One press, one level: back to the turns, then closed.
        assert!(press(&mut chat, KeyCode::Esc));
        assert!(chat.rewind.as_ref().is_some_and(|r| r.action.is_none()));
        assert!(press(&mut chat, KeyCode::Esc));
        assert!(chat.rewind.is_none());
    }

    #[test]
    fn the_code_options_are_dimmed_when_no_files_were_recorded() {
        let home = home_for("dim");
        let mut chat = chat_at_home(&home);
        let transcript = attach(&chat, &home, "dim");
        turn(&transcript, "first question", "t1");

        chat.open_rewind();
        press(&mut chat, KeyCode::Enter);
        let actions = rows(&chat);
        assert!(
            actions
                .iter()
                .any(|row| row.contains("Restore code (no files recorded for this turn)")),
            "{actions:?}"
        );
        let point = chat.rewind.as_ref().unwrap().points[0].clone();
        let offered = chat.rewind_offered(&point);
        assert!(!offered.allows(0) && !offered.allows(2), "code halves off");
        assert!(offered.allows(1), "the conversation half stands alone");

        // Confirming a dimmed row is inert — swallowed, not fallen through.
        chat.rewind.as_mut().unwrap().action = Some(2);
        assert!(press(&mut chat, KeyCode::Enter));
        assert!(chat.rewind.is_some(), "the selector stayed open");
        assert!(state_lines(&chat).is_empty(), "and nothing happened");
    }

    #[test]
    fn the_oldest_turn_cannot_be_summarized_into_nothing() {
        let home = home_for("summarize-dim");
        let mut chat = chat_at_home(&home);
        let transcript = attach(&chat, &home, "summarize-dim");
        turn(&transcript, "the very first question", "t1");
        turn(&transcript, "a later question", "t2");

        chat.open_rewind();
        let points = chat.rewind.as_ref().unwrap().points.clone();
        let oldest = points.last().unwrap();
        let newest = points.first().unwrap();
        assert!(
            !chat.rewind_offered(oldest).summarize,
            "nothing precedes the first turn"
        );
        assert!(chat.rewind_offered(newest).summarize);
    }

    #[test]
    fn restoring_the_conversation_truncates_history_and_fills_the_composer() {
        let home = home_for("conversation");
        let mut chat = chat_at_home(&home);
        let transcript = attach(&chat, &home, "conversation");
        turn(&transcript, "first question", "t1");
        turn(&transcript, "second question", "t2");
        let before = transcript.load_messages().unwrap().len();

        chat.open_rewind();
        // The list is newest first, so index 1 is the first question.
        chat.rewind.as_mut().unwrap().selected = 1;
        press(&mut chat, KeyCode::Enter);
        chat.rewind.as_mut().unwrap().action = Some(1);
        press(&mut chat, KeyCode::Enter);

        assert!(chat.rewind.is_none(), "the selector closed");
        let after = transcript.load_messages().unwrap();
        assert!(after.len() < before);
        assert_eq!(
            after.last().unwrap().content,
            vec![ContentBlock::Text {
                text: "first question".to_string()
            }],
            "history ends at the chosen turn"
        );
        assert_eq!(chat.input, "first question", "the composer gets it back");
        let lines = state_lines(&chat);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("conversation restored to"), "{lines:?}");
        assert!(
            crate::tui::chat::is_state_line(&lines[0]),
            "it renders as a state, not as something the user said"
        );
    }

    #[test]
    fn restoring_code_puts_the_files_back_and_leaves_the_conversation() {
        let home = home_for("code");
        let mut chat = chat_at_home(&home);
        let transcript = attach(&chat, &home, "code");
        turn(&transcript, "first question", "t1");
        let entries = transcript.load_projection().unwrap();
        let line = entries
            .iter()
            .find_map(|e| e.opens_turn.and(e.line))
            .unwrap();

        let dir = crate::rewind::session_dir(&home, &transcript.name());
        let edited = home.join("code-edited.txt");
        let created = home.join("code-created.txt");
        std::fs::write(&edited, "original").unwrap();
        crate::rewind::snapshot(&dir, line, &edited).unwrap();
        crate::rewind::snapshot(&dir, line, &created).unwrap();
        std::fs::write(&edited, "changed").unwrap();
        std::fs::write(&created, "new").unwrap();
        let before = transcript.load_messages().unwrap().len();

        chat.open_rewind();
        press(&mut chat, KeyCode::Enter);
        chat.rewind.as_mut().unwrap().action = Some(2);
        press(&mut chat, KeyCode::Enter);

        assert_eq!(std::fs::read_to_string(&edited).unwrap(), "original");
        assert!(!created.exists(), "a created file is removed");
        assert_eq!(
            transcript.load_messages().unwrap().len(),
            before,
            "the conversation is untouched"
        );
        assert_eq!(chat.input, "", "and the composer is not filled");
        let lines = state_lines(&chat);
        assert!(lines[0].starts_with("⏪ restored 2 files"), "{lines:?}");
        assert!(lines[0].contains("reverted") && lines[0].contains("removed"));
    }

    #[test]
    fn restoring_both_reports_one_line_and_drops_the_snapshots() {
        let home = home_for("both");
        let mut chat = chat_at_home(&home);
        let transcript = attach(&chat, &home, "both");
        turn(&transcript, "first question", "t1");
        let entries = transcript.load_projection().unwrap();
        let line = entries
            .iter()
            .find_map(|e| e.opens_turn.and(e.line))
            .unwrap();

        let dir = crate::rewind::session_dir(&home, &transcript.name());
        let file = home.join("both-edited.txt");
        std::fs::write(&file, "original").unwrap();
        crate::rewind::snapshot(&dir, line, &file).unwrap();
        std::fs::write(&file, "changed").unwrap();

        chat.open_rewind();
        press(&mut chat, KeyCode::Enter);
        chat.rewind.as_mut().unwrap().action = Some(0);
        press(&mut chat, KeyCode::Enter);

        assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");
        assert_eq!(
            transcript.load_messages().unwrap().last().unwrap().content,
            vec![ContentBlock::Text {
                text: "first question".to_string()
            }]
        );
        let lines = state_lines(&chat);
        assert_eq!(lines.len(), 1, "one combined state line: {lines:?}");
        assert!(
            lines[0].contains("restored 1 file")
                && lines[0].contains("code and conversation restored to"),
            "{lines:?}"
        );
        assert_eq!(
            crate::rewind::changed_files(&dir, line),
            crate::rewind::Coverage::default(),
            "the snapshots of the turns that are gone went with them"
        );
    }

    #[test]
    fn never_mind_closes_and_changes_nothing() {
        let home = home_for("never");
        let mut chat = chat_at_home(&home);
        let transcript = attach(&chat, &home, "never");
        turn(&transcript, "first question", "t1");
        let before = transcript.load_messages().unwrap();

        chat.open_rewind();
        press(&mut chat, KeyCode::Enter);
        // `5` jumps straight to it, the way the numbered rows read.
        press(&mut chat, KeyCode::Char('5'));
        assert_eq!(chat.rewind.as_ref().unwrap().action, Some(4));
        press(&mut chat, KeyCode::Enter);

        assert!(chat.rewind.is_none());
        assert_eq!(transcript.load_messages().unwrap(), before);
        assert!(state_lines(&chat).is_empty());
        assert_eq!(chat.input, "");
    }

    #[test]
    fn a_compaction_in_flight_refuses_the_restore() {
        let home = home_for("mid-compact");
        let mut chat = chat_at_home(&home);
        let transcript = attach(&chat, &home, "mid-compact");
        turn(&transcript, "first question", "t1");
        let before = transcript.load_messages().unwrap();

        chat.open_rewind();
        press(&mut chat, KeyCode::Enter);
        chat.pin_panel("compact", vec!["⏳ compacting the context…".to_string()]);
        chat.rewind.as_mut().unwrap().action = Some(1);
        press(&mut chat, KeyCode::Enter);

        assert_eq!(transcript.load_messages().unwrap(), before);
        assert!(
            chat.slash_info_lines
                .iter()
                .any(|line| line.contains("wait for compaction to finish first")),
            "{:?}",
            chat.slash_info_lines
        );
    }
}

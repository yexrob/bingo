//! The background dialog (D107): one modal over everything working in the
//! background — agents, shells and rooms.
//!
//! CC's answer to "what else is going on" is a single on-demand dialog
//! (`components/tasks/BackgroundTasksDialog.tsx`) with one section per kind of
//! background work, one cursor walking all of them, and four verbs on the
//! bottom row. D107 rebuilds bingo's `ctrl+b` manager into exactly that shape
//! and lets it absorb the D95 team directory, which answered two of the same
//! questions — who is here, what rooms exist — from a panel that had lost its
//! door.
//!
//! **Three sections, two of them CC's.** `Agents` and `Shells` are CC's own
//! (`:429`, `:439`); `Rooms` is bingo's one extension, expressed in the same
//! grammar rather than beside it. Nothing here is cached: the registries are
//! read fresh on every draw, because a dozen rows drawn only while a modal is
//! open cost nothing and a cached roster is a roster that can be wrong.
//!
//! **This is the surface the accounting store was kept for.** D103 kept
//! `Buffers`' readers alive through a batch that had no use for them and D104
//! declined them again — CC puts no badge on a pill or a tree row — with the
//! note that "three unread from @scout" is a question *this* dialog asks. It
//! does: every row carries its conversation's unread count, and the sections
//! are ordered by what moved most recently.
//!
//! **What the cursor is on is a thing, not a position.** CC keeps a selected
//! id and re-finds it every render (`:184-192` sorts, `Item` compares ids);
//! bingo keeps the target itself for the same reason and one more — the rows
//! re-sort as work moves, and an index would let `x` stop whatever slid under
//! the cursor between two frames.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyModifiers};

use crate::agents::{AgentState, AgentStatus};
use crate::channels::{ChannelStatus, USER_NAME};
use crate::tui::buffer::BufferId;
use crate::tui::chat::{Chat, Row, one_line};
use crate::tui::line::{Line, SegStyle};
use crate::tui::theme::Theme;
use crate::tui::tree::{duration_label, stats_body, status_label};
use crate::tui::zoom::ZoomTarget;
use crate::watch::{WatchKind, WatchSnapshot, WatchState};

/// How many rows one section shows before it counts the rest. The whole dialog
/// is an overlay above the composer, so the three sections together must still
/// leave a window to type in.
const SECTION_ROWS_MAX: usize = 8;

/// How many recent messages a room's detail shows.
const ROOM_LOG_SHOWN: usize = 6;

/// How many output lines a shell's detail shows — the tail, because the end of
/// a build is what a reader opened it for.
const SHELL_OUTPUT_ROWS: usize = 10;

/// Longest prompt a detail prints, and the rows it may spend on it.
const PROMPT_CHARS_MAX: usize = 300;
const PROMPT_ROWS_MAX: usize = 6;

/// What the cursor can be on. A shell is keyed by its watch id: two `cargo
/// build`s are two shells, and a label is not an identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DialogTarget {
    Agent(String),
    Shell(u64),
    Room(String),
}

impl DialogTarget {
    /// Where `f` points the screen. A shell has no conversation to zoom into —
    /// it is a command, not somebody to talk to.
    fn zoom(&self) -> Option<ZoomTarget> {
        match self {
            Self::Agent(name) => Some(ZoomTarget::Agent(name.clone())),
            Self::Room(name) => Some(ZoomTarget::Room(name.clone())),
            Self::Shell(_) => None,
        }
    }
}

/// The open dialog: what the cursor is on, and whether a detail has the box.
///
/// `selected: None` means "the first row there is", resolved at draw time —
/// so opening the dialog needs to know nothing about the roster, and a row
/// that leaves takes the cursor back to the top rather than off the end.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BackgroundDialog {
    pub selected: Option<DialogTarget>,
    pub detail: Option<DialogTarget>,
}

/// How a row's status chip reads. CC colours the chip by status and leaves a
/// running one plain (`ShellProgress.tsx:21`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Done,
    Failed,
    Stopped,
    /// A badge the accounting store put there: unread, and whether it named you.
    Unread {
        mention: bool,
    },
}

/// One line of the list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogRow {
    /// The identity column: `@scout`, `$ cargo build`, `#build`.
    pub name: String,
    /// What it is doing, dim — CC's `: <activity>` (`BackgroundTask.tsx:177`).
    pub body: String,
    /// CC's parenthesised status chip, without its parens. Empty for none.
    pub chip: String,
    pub tone: Tone,
    /// `None` for a heading or a note; `Some` for a row the cursor can act on.
    pub target: Option<DialogTarget>,
    pub heading: bool,
}

impl DialogRow {
    fn heading(text: String) -> Self {
        Self {
            name: text,
            body: String::new(),
            chip: String::new(),
            tone: Tone::Plain,
            target: None,
            heading: true,
        }
    }

    /// A furniture row: the blank between two sections.
    pub fn is_empty(&self) -> bool {
        !self.heading && self.target.is_none() && self.text().is_empty()
    }

    /// The row as one string — what the tests read, and what the width
    /// arithmetic measures.
    pub fn text(&self) -> String {
        let mut out = format!("{}{}", self.name, self.body);
        if !self.chip.is_empty() {
            out.push_str(&format!(" ({})", self.chip));
        }
        out
    }
}

/// One conversation's accounting, as the dialog reads it.
#[derive(Debug, Clone, Copy, Default)]
struct Badge {
    unread: u64,
    mention: bool,
    /// The host's frame counter at the last observed change. **An order, not a
    /// duration**: the tick only advances while something is happening
    /// (`Chat::needs_tick`), so it can say which conversation moved last and
    /// cannot say how long ago. Durations come from the sources that carry a
    /// wall clock — an instance's `last_active`, a watch's elapsed.
    last_activity: u64,
}

impl Badge {
    /// The chip text, empty where there is nothing to report.
    fn chip(&self) -> String {
        if self.unread == 0 {
            String::new()
        } else {
            format!("{} unread", self.unread)
        }
    }
}

impl Chat {
    /// Open the dialog. Inert behind a permission dialog, for the reason every
    /// competing surface is (D81): a modal that owns Enter must not open over a
    /// question that is holding up a turn.
    pub(crate) fn open_background_dialog(&mut self) {
        if self.pending_ask.is_some() {
            return;
        }
        self.dialog = Some(BackgroundDialog::default());
        self.dirty = true;
    }

    /// Every conversation's accounting, keyed by conversation.
    ///
    /// This is the one read of [`crate::tui::buffer::Buffers`] the dialog
    /// makes, and it is the whole of what D103 and D104 kept the store's
    /// readers alive for.
    fn dialog_badges(&self) -> HashMap<BufferId, Badge> {
        self.buffers
            .iter()
            .map(|buffer| {
                (
                    buffer.id().clone(),
                    Badge {
                        unread: buffer.unread(),
                        mention: buffer.mention(),
                        last_activity: buffer.last_activity(),
                    },
                )
            })
            .collect()
    }

    /// The background shells: the watch registry's own command entries, which
    /// is what a `Bash` call with `background: true` and a `ctrl+b` promotion
    /// both become (D84).
    fn dialog_shells(&self) -> Vec<WatchSnapshot> {
        let mut shells: Vec<WatchSnapshot> = self
            .session
            .watch
            .snapshot()
            .into_iter()
            .filter(|snapshot| snapshot.kind == WatchKind::Command)
            .collect();
        // CC's order: running first, then youngest first (`:184-192`, over
        // `startTime`). Ids are handed out in sequence, so a higher id is a
        // younger command and the second key needs no clock.
        shells.sort_by(|a, b| {
            let running = |s: &WatchSnapshot| !s.state.is_terminal();
            running(b)
                .cmp(&running(a))
                .then_with(|| b.id.0.cmp(&a.id.0))
        });
        shells
    }

    /// The roster, in the dialog's order: running first, then whichever
    /// conversation moved most recently, then by name so the order is total.
    fn dialog_agents(&self, badges: &HashMap<BufferId, Badge>) -> Vec<AgentStatus> {
        let mut agents = self.session.agents.list();
        agents.sort_by(|a, b| {
            let running = |s: &AgentStatus| s.state == AgentState::Running;
            let moved = |s: &AgentStatus| {
                badges
                    .get(&BufferId::Dm(s.name.clone()))
                    .map(|badge| badge.last_activity)
                    .unwrap_or(0)
            };
            running(b)
                .cmp(&running(a))
                .then_with(|| moved(b).cmp(&moved(a)))
                .then_with(|| a.name.cmp(&b.name))
        });
        agents
    }

    /// The rooms, most recently moved first. A room the user is not in has no
    /// accounting at all, so it sinks — which is the right place for somebody
    /// else's conversation.
    fn dialog_rooms(&self, badges: &HashMap<BufferId, Badge>) -> Vec<ChannelStatus> {
        let mut rooms = self.session.channels.list();
        rooms.sort_by(|a, b| {
            let moved = |s: &ChannelStatus| {
                badges
                    .get(&BufferId::Channel(s.name.clone()))
                    .map(|badge| badge.last_activity)
                    .unwrap_or(0)
            };
            moved(b).cmp(&moved(a)).then_with(|| a.name.cmp(&b.name))
        });
        rooms
    }

    /// The whole list, rebuilt from the domain.
    ///
    /// **A heading only appears where there is something to tell it apart
    /// from** — CC renders the `Agents` and `Shells` headers only when another
    /// kind is present (`:428`, `:438`), because a label over the only list on
    /// screen is noise. An empty section renders nothing at all, and a dialog
    /// with nothing in it says so in one line (`:426`).
    pub(crate) fn dialog_rows(&self) -> Vec<DialogRow> {
        let badges = self.dialog_badges();
        let agents = self.dialog_agents(&badges);
        let shells = self.dialog_shells();
        let rooms = self.dialog_rooms(&badges);
        let kinds = [!agents.is_empty(), !shells.is_empty(), !rooms.is_empty()]
            .iter()
            .filter(|present| **present)
            .count();
        let headings = kinds > 1;
        let now = std::time::Instant::now();
        let mut rows: Vec<DialogRow> = Vec::new();

        if !agents.is_empty() {
            if headings {
                rows.push(DialogRow::heading(format!("  Agents ({})", agents.len())));
            }
            for status in agents.iter().take(SECTION_ROWS_MAX) {
                let badge = badges
                    .get(&BufferId::Dm(status.name.clone()))
                    .copied()
                    .unwrap_or_default();
                rows.push(DialogRow {
                    name: format!("@{}", status.name),
                    body: format!(": {}", status_label(status, now)),
                    chip: badge.chip(),
                    tone: Tone::Unread {
                        mention: badge.mention,
                    },
                    target: Some(DialogTarget::Agent(status.name.clone())),
                    heading: false,
                });
            }
            push_overflow(&mut rows, agents.len(), "agents");
        }

        if !shells.is_empty() {
            push_gap(&mut rows);
            if headings {
                rows.push(DialogRow::heading(format!("  Shells ({})", shells.len())));
            }
            for shell in shells.iter().take(SECTION_ROWS_MAX) {
                let (chip, tone) = shell_chip(shell.state);
                rows.push(DialogRow {
                    name: shell.label.clone(),
                    body: String::new(),
                    chip: chip.to_string(),
                    tone,
                    target: Some(DialogTarget::Shell(shell.id.0)),
                    heading: false,
                });
            }
            push_overflow(&mut rows, shells.len(), "shells");
        }

        if !rooms.is_empty() {
            push_gap(&mut rows);
            if headings {
                rows.push(DialogRow::heading(format!("  Rooms ({})", rooms.len())));
            }
            for room in rooms.iter().take(SECTION_ROWS_MAX) {
                let mine = room.members.iter().any(|member| member == USER_NAME);
                let badge = badges
                    .get(&BufferId::Channel(room.name.clone()))
                    .copied()
                    .unwrap_or_default();
                // The mark is on the rooms you are *not* in (D95): those are
                // the ones where speaking means something different, and a
                // mark on every room you are in would be a column of ticks
                // saying nothing.
                let (chip, tone) = if mine {
                    (
                        badge.chip(),
                        Tone::Unread {
                            mention: badge.mention,
                        },
                    )
                } else {
                    ("you're not in".to_string(), Tone::Plain)
                };
                rows.push(DialogRow {
                    name: format!("#{}", room.name),
                    body: format!(
                        ": {} {} · {}",
                        room.seq,
                        if room.seq == 1 { "message" } else { "messages" },
                        room.members.join(", ")
                    ),
                    chip,
                    tone,
                    target: Some(DialogTarget::Room(room.name.clone())),
                    heading: false,
                });
            }
            push_overflow(&mut rows, rooms.len(), "rooms");
        }
        rows
    }

    /// The rows the cursor can land on, in the order they are drawn.
    pub(crate) fn dialog_targets(&self) -> Vec<DialogTarget> {
        self.dialog_rows()
            .into_iter()
            .filter_map(|row| row.target)
            .collect()
    }

    /// What the cursor is on right now: the target it was put on if that row is
    /// still there, otherwise the first row of the list.
    pub(crate) fn dialog_selection(&self) -> Option<DialogTarget> {
        let dialog = self.dialog.as_ref()?;
        let targets = self.dialog_targets();
        match &dialog.selected {
            Some(target) if targets.contains(target) => Some(target.clone()),
            _ => targets.first().cloned(),
        }
    }

    /// The dialog's subtitle — CC's running counts, ` · ` joined (`:404-413`).
    /// A room has no running state, so it is not counted: the line says what is
    /// *working*, and a room is a place rather than a task.
    fn dialog_subtitle(&self) -> String {
        let agents = self
            .session
            .agents
            .list()
            .iter()
            .filter(|status| status.state == AgentState::Running)
            .count();
        let shells = self
            .dialog_shells()
            .iter()
            .filter(|shell| !shell.state.is_terminal())
            .count();
        let mut parts: Vec<String> = Vec::new();
        if agents > 0 {
            parts.push(format!(
                "{agents} {}",
                if agents == 1 { "agent" } else { "agents" }
            ));
        }
        if shells > 0 {
            parts.push(format!(
                "{shells} active {}",
                if shells == 1 { "shell" } else { "shells" }
            ));
        }
        parts.join(" · ")
    }

    /// The bottom row of the list, CC's `Byline` of `<key> to <action>` hints
    /// (`:414`, `design-system/KeyboardShortcutHint.tsx:16`).
    ///
    /// `f` and `x` are conditional in CC and conditional here, on what the
    /// selected row can actually be asked to do.
    fn dialog_hint(&self) -> String {
        let selection = self.dialog_selection();
        let mut parts = vec!["↑/↓ to select".to_string(), "Enter to view".to_string()];
        if selection.as_ref().is_some_and(|t| t.zoom().is_some()) {
            parts.push("f to foreground".to_string());
        }
        if self.dialog_stoppable(selection.as_ref()) {
            parts.push("x to stop".to_string());
        }
        parts.push("←/Esc to close".to_string());
        parts.join(" · ")
    }

    /// Whether `x` means anything on this row. Only a running instance can be
    /// stopped: a background command has no kill path in bingo at all, and a
    /// room is not a process.
    fn dialog_stoppable(&self, target: Option<&DialogTarget>) -> bool {
        let Some(DialogTarget::Agent(name)) = target else {
            return false;
        };
        self.session
            .agents
            .list()
            .iter()
            .any(|status| &status.name == name && status.state == AgentState::Running)
    }

    /// Move the cursor by one row, wrapping — the list is a ring, as CC's is
    /// (`useBackgroundTaskNavigation.ts:26-58`).
    fn dialog_step(&mut self, delta: isize) {
        let targets = self.dialog_targets();
        if targets.is_empty() {
            return;
        }
        let here = self
            .dialog_selection()
            .and_then(|target| targets.iter().position(|t| *t == target))
            .unwrap_or(0) as isize;
        let next = (here + delta).rem_euclid(targets.len() as isize) as usize;
        if let Some(dialog) = self.dialog.as_mut() {
            dialog.selected = Some(targets[next].clone());
        }
    }

    /// `f`: point the screen at the selected conversation (CC's `foreground`,
    /// `:414`). The dialog closes behind it — the zoom takes the whole screen,
    /// and a modal waiting underneath it would be a surprise on the way back.
    ///
    /// A shell is not a conversation and has nothing to foreground.
    fn dialog_foreground(&mut self) -> bool {
        let Some(target) = self.dialog_selection().and_then(|t| t.zoom()) else {
            return false;
        };
        self.close_menus();
        self.switch_to(Some(target));
        true
    }

    /// `x`: stop the selected instance, through the one stop path every
    /// surface uses — one warning, one watch transition, and no confirmation,
    /// which is CC's ruling (`useBackgroundTaskNavigation.ts:228-241`).
    ///
    /// A running **shell** says why it cannot be stopped instead of doing
    /// nothing: bingo hands a promoted command to the watch registry without
    /// keeping a handle on the child (`tool/bash.rs`), so there is nothing here
    /// to kill. An honest refusal beats a key that appears dead.
    fn dialog_stop(&mut self) {
        match self.dialog_selection() {
            Some(DialogTarget::Agent(name)) => {
                if self.dialog_stoppable(Some(&DialogTarget::Agent(name.clone()))) {
                    self.stop_agent(&name);
                }
            }
            Some(DialogTarget::Shell(id)) => {
                let running = self
                    .dialog_shells()
                    .iter()
                    .any(|shell| shell.id.0 == id && !shell.state.is_terminal());
                if running {
                    self.push_warning(
                        "a background command cannot be stopped from here; it reports when it exits"
                            .to_string(),
                    );
                }
            }
            _ => {}
        }
    }

    /// Keys, while the dialog is open.
    ///
    /// Modal for what it uses and transparent to the chords it does not: an
    /// open dialog swallows a bare key rather than letting it edit the draft
    /// underneath, and `ctrl+c` still means out (D80).
    pub fn background_dialog_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        // ctrl+b reads the situation (D84): a shell command running in the
        // foreground is what the key is about right now, and only when there is
        // none does it open the dialog.
        if self.dialog.is_none() && code == KeyCode::Char('b') && ctrl && self.live.promote() {
            // The tail's rows go with the row they hung under; the command
            // reappears as the background task it now is.
            self.bash_tail = None;
            self.dirty = true;
            return true;
        }
        if self.dialog.is_none() && code == KeyCode::Char('b') && ctrl {
            self.open_background_dialog();
            return self.dialog.is_some();
        }
        if self.dialog.is_none() || self.pending_ask.is_some() {
            return false;
        }
        // The key that opened it closes it — the ctrl+t panels' rule, and the
        // alternative is a dead chord, because nothing else is bound to it.
        if code == KeyCode::Char('b') && ctrl {
            self.dialog = None;
            self.dirty = true;
            return true;
        }
        // Every other chord belongs to the application: `ctrl+c` means out
        // (D80), and an editing chord still reaches the draft underneath.
        if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
            return false;
        }
        self.dirty = true;
        let in_detail = self.dialog.as_ref().is_some_and(|d| d.detail.is_some());
        match code {
            // CC's detail *replaces* the list rather than sitting on top of it
            // (`:396`, `:398`), so `←` is the only way back and Esc closes the
            // dialog from either mode.
            KeyCode::Esc => self.dialog = None,
            KeyCode::Left => {
                if in_detail {
                    if let Some(dialog) = self.dialog.as_mut() {
                        dialog.detail = None;
                    }
                } else {
                    self.dialog = None;
                }
            }
            KeyCode::Enter | KeyCode::Char(' ') if in_detail => self.dialog = None,
            KeyCode::Enter => {
                if let Some(target) = self.dialog_selection()
                    && let Some(dialog) = self.dialog.as_mut()
                {
                    dialog.detail = Some(target);
                }
            }
            KeyCode::Up => self.dialog_step(-1),
            KeyCode::Down => self.dialog_step(1),
            KeyCode::Char('f') => {
                if self.dialog_foreground() {
                    self.dialog = None;
                }
            }
            KeyCode::Char('x') => self.dialog_stop(),
            _ => {}
        }
        true
    }

    /// The overlay: the list, or the detail that replaced it.
    pub fn dialog_view_rows(&self, width: usize) -> Vec<Row> {
        let Some(dialog) = &self.dialog else {
            return Vec::new();
        };
        let theme = &self.theme;
        let rows = match &dialog.detail {
            Some(DialogTarget::Agent(name)) => self.agent_detail_rows(name, width),
            Some(DialogTarget::Shell(id)) => self.shell_detail_rows(*id, width),
            Some(DialogTarget::Room(name)) => self.room_detail_rows(name, width),
            None => self.dialog_list_rows(width),
        };
        crate::tui::chat::manager_box(rows, width, theme)
    }

    /// The list itself.
    fn dialog_list_rows(&self, width: usize) -> Vec<Row> {
        let theme = &self.theme;
        let budget = width.saturating_sub(4);
        let mut out = vec![Row::new(Line::styled(
            "Background tasks",
            SegStyle::fg(theme.text).bold(),
        ))];
        let subtitle = self.dialog_subtitle();
        if !subtitle.is_empty() {
            out.push(Row::new(Line::styled(
                subtitle,
                SegStyle::fg(theme.text_secondary),
            )));
        }
        let rows = self.dialog_rows();
        // The header is its own block, like each section and like the byline.
        out.push(Row::new(Line::empty()));
        if rows.is_empty() {
            out.push(Row::new(Line::styled(
                "No tasks currently running",
                SegStyle::fg(theme.text_secondary),
            )));
        }
        let selected = self.dialog_selection();
        for row in &rows {
            if row.heading {
                out.push(Row::new(Line::styled(
                    one_line(&row.text(), budget),
                    SegStyle::fg(theme.text_secondary).bold(),
                )));
                continue;
            }
            let Some(target) = &row.target else {
                out.push(Row::new(if row.is_empty() {
                    Line::empty()
                } else {
                    Line::styled(
                        one_line(&row.text(), budget),
                        SegStyle::fg(theme.text_secondary),
                    )
                }));
                continue;
            };
            let here = selected.as_ref() == Some(target);
            let mut line = Line::styled(
                // CC's pointer, in the cell before the row (`:571`).
                if here { "❯ " } else { "  " }.to_string(),
                SegStyle::fg(theme.permission),
            );
            let name_style = match target {
                DialogTarget::Agent(name) => SegStyle::fg(self.identity_color(name)),
                _ if here => SegStyle::fg(theme.permission),
                _ => SegStyle::fg(theme.text),
            };
            let mut left = budget.saturating_sub(2);
            let name = one_line(&row.name, left);
            left = left.saturating_sub(crate::tui::line::text_width(&name));
            line.push_styled(name, name_style);
            if !row.body.is_empty() {
                let body = one_line(&row.body, left);
                left = left.saturating_sub(crate::tui::line::text_width(&body));
                line.push_styled(body, SegStyle::fg(theme.text_secondary));
            }
            if !row.chip.is_empty() {
                line.push_styled(
                    one_line(&format!(" ({})", row.chip), left),
                    chip_style(row.tone, theme),
                );
            }
            out.push(Row::new(line));
        }
        // The key row is the dialog's byline, not its last item: one blank
        // keeps it off the content the way the box's border keeps it off the
        // composer.
        out.push(Row::new(Line::empty()));
        out.push(Row::new(Line::styled(
            one_line(&self.dialog_hint(), budget),
            SegStyle::fg(theme.text_secondary),
        )));
        out
    }

    /// One instance's detail — CC's `InProcessTeammateDetailDialog`: the name
    /// and what it is doing as the title (`:126-150`), the run's cost as the
    /// subtitle (`:160-183`), then `Progress` and `Prompt` (`:209`, `:218`).
    fn agent_detail_rows(&self, name: &str, width: usize) -> Vec<Row> {
        let theme = &self.theme;
        let budget = width.saturating_sub(4);
        let statuses = self.session.agents.list();
        let status = statuses.iter().find(|status| status.name == name);
        let now = std::time::Instant::now();
        let mut title = Line::styled(
            format!("@{name}"),
            SegStyle::fg(self.identity_color(name)).bold(),
        );
        if let Some(status) = status {
            title.push_styled(
                format!(" ({})", status_label(status, now)),
                SegStyle::fg(theme.text_secondary),
            );
        }
        let mut rows = vec![Row::new(title)];
        let Some(status) = status else {
            rows.push(Row::new(Line::styled(
                "Agent is no longer available",
                SegStyle::fg(theme.text_secondary),
            )));
            rows.push(Row::new(Line::empty()));
            rows.push(Row::new(Line::styled(
                one_line(&self.dialog_hint_detail(None), budget),
                SegStyle::fg(theme.text_secondary),
            )));
            return rows;
        };
        let mut subtitle = Line::empty();
        // CC leads the subtitle with the state where it is not running
        // (`:154`), and prints the cost after it.
        if status.state != AgentState::Running {
            subtitle.push_styled(
                format!("{} · ", state_word(status.state)),
                SegStyle::fg(state_color(status.state, theme)),
            );
        }
        // Both halves are gated on there being something to say — the tree's
        // rule for the same numbers (D104): bingo's progress is the *current
        // run*, so an instance between runs has no elapsed to print and no
        // tools to count, and `0s · 0 tool uses · 0 tokens` would be three
        // measurements of nothing.
        let mut cost: Vec<String> = Vec::new();
        if let Some(elapsed) = status.elapsed {
            cost.push(duration_label(elapsed));
        }
        if status.tool_uses > 0 || status.output_tokens > 0 {
            cost.push(stats_body(status.tool_uses, status.output_tokens));
        }
        if !cost.is_empty() {
            subtitle.push_styled(cost.join(" · "), SegStyle::fg(theme.text_secondary));
        }
        if !subtitle.plain_text().is_empty() {
            rows.push(Row::new(subtitle));
        }
        rows.push(Row::new(Line::empty()));

        rows.push(Row::new(Line::styled(
            "Progress",
            SegStyle::fg(theme.text_secondary).bold(),
        )));
        if status.recent_activity.is_empty() {
            rows.push(Row::new(Line::styled(
                "› initializing…",
                SegStyle::fg(theme.text_secondary),
            )));
        } else {
            for (index, activity) in status.recent_activity.iter().enumerate() {
                // CC marks the newest with `›` and dims the rest (`:209`).
                let latest = index + 1 == status.recent_activity.len();
                rows.push(Row::new(Line::styled(
                    one_line(
                        &format!("{}{activity}", if latest { "› " } else { "  " }),
                        budget,
                    ),
                    SegStyle::fg(if latest {
                        theme.text
                    } else {
                        theme.text_secondary
                    }),
                )));
            }
        }
        rows.push(Row::new(Line::empty()));
        rows.push(Row::new(Line::styled(
            "Prompt",
            SegStyle::fg(theme.text_secondary).bold(),
        )));
        // The task it was dispatched with, in the fullest form the registry
        // has: the spawn prompt, or the one-line description a crew member
        // carries instead, or an admission that neither was recorded.
        let prompt = if !status.prompt.is_empty() {
            truncate_chars(&status.prompt, PROMPT_CHARS_MAX)
        } else if !status.description.is_empty() {
            truncate_chars(&status.description, PROMPT_CHARS_MAX)
        } else {
            "(prompt unavailable)".to_string()
        };
        let wrapped = crate::tui::line::wrap_words(&prompt, budget.max(1));
        for line in wrapped.iter().take(PROMPT_ROWS_MAX) {
            rows.push(Row::new(Line::plain(line.clone())));
        }
        if wrapped.len() > PROMPT_ROWS_MAX {
            rows.push(Row::new(Line::styled(
                format!("… +{} prompt lines", wrapped.len() - PROMPT_ROWS_MAX),
                SegStyle::fg(theme.text_secondary),
            )));
        }
        rows.push(Row::new(Line::empty()));
        rows.push(Row::new(Line::styled(
            one_line(&self.dialog_hint_detail(Some(status)), budget),
            SegStyle::fg(theme.text_secondary),
        )));
        rows
    }

    /// The detail's bottom row — CC's
    /// (`InProcessTeammateDetailDialog.tsx:198`), conditional on what the row
    /// can actually do.
    fn dialog_hint_detail(&self, status: Option<&AgentStatus>) -> String {
        let mut parts = vec![
            "← to go back".to_string(),
            "Esc/Enter/Space to close".to_string(),
        ];
        if status.is_some_and(|s| s.state == AgentState::Running) {
            parts.push("x to stop".to_string());
        }
        if status.is_some() {
            parts.push("f to foreground".to_string());
        }
        parts.join(" · ")
    }

    /// One shell's detail — CC's `ShellDetailDialog`: the labelled facts
    /// (`:177`, `:193`, `:223`, `:253`) and the tail of what it printed.
    fn shell_detail_rows(&self, id: u64, width: usize) -> Vec<Row> {
        let theme = &self.theme;
        let budget = width.saturating_sub(4);
        let shells = self.dialog_shells();
        let shell = shells.iter().find(|shell| shell.id.0 == id);
        let mut rows = vec![Row::new(Line::styled(
            "Shell details",
            SegStyle::fg(theme.text).bold(),
        ))];
        let Some(shell) = shell else {
            rows.push(Row::new(Line::styled(
                "Shell is no longer available",
                SegStyle::fg(theme.text_secondary),
            )));
            rows.push(Row::new(Line::styled(
                "← to go back · Esc/Enter/Space to close",
                SegStyle::fg(theme.text_secondary),
            )));
            return rows;
        };
        let (chip, tone) = shell_chip(shell.state);
        let mut status_line = Line::styled("Status: ", SegStyle::fg(theme.text).bold());
        status_line.push_styled(chip.to_string(), chip_style(tone, theme));
        if let Some(detail) = &shell.detail {
            status_line.push_styled(
                one_line(&format!(" · {detail}"), budget),
                SegStyle::fg(theme.text_secondary),
            );
        }
        rows.push(Row::new(status_line));
        let mut runtime = Line::styled("Runtime: ", SegStyle::fg(theme.text).bold());
        runtime.push_styled(
            duration_label(std::time::Duration::from_millis(shell.elapsed_ms)),
            SegStyle::fg(theme.text_secondary),
        );
        rows.push(Row::new(runtime));
        rows.push(Row::new(Line::styled(
            "Command:",
            SegStyle::fg(theme.text).bold(),
        )));
        for line in crate::tui::line::wrap_words(&shell.label, budget.max(1))
            .iter()
            .take(PROMPT_ROWS_MAX)
        {
            rows.push(Row::new(Line::plain(line.clone())));
        }
        rows.push(Row::new(Line::empty()));
        rows.push(Row::new(Line::styled(
            "Output:",
            SegStyle::fg(theme.text).bold(),
        )));
        // A running command's output is not in the registry: the tail the user
        // watched belongs to the foreground, and the completion payload is
        // what the finished one carries.
        let output: Vec<&str> = shell
            .payload
            .as_ref()
            .and_then(|payload| payload.as_str())
            .map(|text| text.lines().filter(|l| !l.trim().is_empty()).collect())
            .unwrap_or_default();
        if output.is_empty() {
            rows.push(Row::new(Line::styled(
                "No output available",
                SegStyle::fg(theme.text_secondary),
            )));
        } else {
            let shown = output.len().min(SHELL_OUTPUT_ROWS);
            for line in output.iter().skip(output.len() - shown) {
                rows.push(Row::new(Line::styled(
                    one_line(line, budget),
                    SegStyle::fg(theme.text_secondary),
                )));
            }
            rows.push(Row::new(Line::styled(
                format!("Showing {shown} lines"),
                SegStyle::fg(theme.text_secondary),
            )));
        }
        rows.push(Row::new(Line::empty()));
        rows.push(Row::new(Line::styled(
            one_line(
                &if shell.state.is_terminal() {
                    "← to go back · Esc/Enter/Space to close".to_string()
                } else {
                    "← to go back · Esc/Enter/Space to close · x to stop".to_string()
                },
                budget,
            ),
            SegStyle::fg(theme.text_secondary),
        )));
        rows
    }

    /// One room's detail. bingo's own section, in CC's detail grammar: the
    /// labelled facts, then the tail of the log.
    fn room_detail_rows(&self, name: &str, width: usize) -> Vec<Row> {
        let theme = &self.theme;
        let budget = width.saturating_sub(4);
        let rooms = self.session.channels.list();
        let room = rooms.iter().find(|room| room.name == name);
        let mut rows = vec![Row::new(Line::styled(
            format!("#{name}"),
            SegStyle::fg(theme.text).bold(),
        ))];
        let Some(room) = room else {
            rows.push(Row::new(Line::styled(
                "Room is no longer available",
                SegStyle::fg(theme.text_secondary),
            )));
            rows.push(Row::new(Line::styled(
                "← to go back · Esc/Enter/Space to close",
                SegStyle::fg(theme.text_secondary),
            )));
            return rows;
        };
        let mut members = Line::styled("Members: ", SegStyle::fg(theme.text).bold());
        members.push_styled(
            one_line(&room.members.join(", "), budget),
            SegStyle::fg(theme.text_secondary),
        );
        rows.push(Row::new(members));
        let mut count = Line::styled("Messages: ", SegStyle::fg(theme.text).bold());
        count.push_styled(
            format!(
                "{}{}",
                room.seq,
                if room.members.iter().any(|m| m == USER_NAME) {
                    ""
                } else {
                    " · you're not in"
                }
            ),
            SegStyle::fg(theme.text_secondary),
        );
        rows.push(Row::new(count));
        rows.push(Row::new(Line::empty()));
        rows.push(Row::new(Line::styled(
            "Recent messages:",
            SegStyle::fg(theme.text).bold(),
        )));
        let log = self.session.channels.log_of(name);
        if log.is_empty() {
            rows.push(Row::new(Line::styled(
                "No messages yet",
                SegStyle::fg(theme.text_secondary),
            )));
        } else {
            let shown = log.len().min(ROOM_LOG_SHOWN);
            for message in log.iter().skip(log.len() - shown) {
                rows.push(Row::new(Line::styled(
                    one_line(
                        &format!("{}: {}", message.from, message.text.replace('\n', " ")),
                        budget,
                    ),
                    SegStyle::fg(theme.text_secondary),
                )));
            }
        }
        rows.push(Row::new(Line::empty()));
        rows.push(Row::new(Line::styled(
            one_line(
                "← to go back · Esc/Enter/Space to close · f to foreground",
                budget,
            ),
            SegStyle::fg(theme.text_secondary),
        )));
        rows
    }
}

/// The blank row CC puts above a section that follows another
/// (`BackgroundTasksDialog.tsx:438`, `marginTop={1}`). It is furniture: no
/// target, no heading, nothing to select.
fn push_gap(rows: &mut Vec<DialogRow>) {
    if !rows.is_empty() {
        rows.push(DialogRow {
            name: String::new(),
            body: String::new(),
            chip: String::new(),
            tone: Tone::Plain,
            target: None,
            heading: false,
        });
    }
}

/// `… N more agents` where a section runs past its window.
fn push_overflow(rows: &mut Vec<DialogRow>, total: usize, kind: &str) {
    if total > SECTION_ROWS_MAX {
        rows.push(DialogRow {
            name: format!("  … {} more {kind}", total - SECTION_ROWS_MAX),
            body: String::new(),
            chip: String::new(),
            tone: Tone::Plain,
            target: None,
            heading: false,
        });
    }
}

/// CC's status chip for a command (`ShellProgress.tsx:39-80`): the word, and
/// the tone that colours it.
fn shell_chip(state: WatchState) -> (&'static str, Tone) {
    match state {
        WatchState::Done => ("done", Tone::Done),
        WatchState::Failed => ("error", Tone::Failed),
        WatchState::Cancelled => ("stopped", Tone::Stopped),
        WatchState::Running | WatchState::Idle => ("running", Tone::Plain),
    }
}

/// How a chip is painted. CC colours a finished task green, a failed one red
/// and a killed one amber, and leaves a running one plain
/// (`ShellProgress.tsx:21`). The unread badge is bingo's own, and it keeps D90's
/// rule: a conversation that said your name is worth more than one that merely
/// moved, so the accent means "wants you" and nothing else does.
fn chip_style(tone: Tone, theme: &Theme) -> SegStyle {
    match tone {
        Tone::Plain => SegStyle::fg(theme.text_secondary),
        Tone::Done => SegStyle::fg(theme.success),
        Tone::Failed => SegStyle::fg(theme.error),
        Tone::Stopped => SegStyle::fg(theme.warning),
        Tone::Unread { mention: true } => SegStyle::fg(theme.claude).bold(),
        Tone::Unread { mention: false } => SegStyle::fg(theme.text),
    }
}

/// What a stopped or finished instance's detail leads with.
fn state_word(state: AgentState) -> &'static str {
    match state {
        AgentState::Running => "Running",
        AgentState::Idle => "Idle",
        AgentState::Stopped => "Stopped",
    }
}

fn state_color(state: AgentState, theme: &Theme) -> ratatui::style::Color {
    match state {
        AgentState::Stopped => theme.warning,
        _ => theme.text_secondary,
    }
}

/// Cut to a character budget, with an ellipsis where anything was cut.
fn truncate_chars(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let mut out: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentKind;
    use crate::channels::ChannelMode;
    use crate::tui::test_util::chat_at;
    use crate::watch::{WatchId, WatchPoll, Watchable};

    fn test_chat() -> Chat {
        chat_at(100, 40)
    }

    fn seed_agent(chat: &Chat, name: &str) {
        chat.session.agents.insert(
            name,
            AgentKind::Crew,
            None,
            format!("{name}'s task"),
            chat.session.clone(),
        );
    }

    fn seed_room(chat: &Chat, name: &str, members: &[&str]) {
        chat.session
            .channels
            .create(
                name,
                members.iter().map(|m| (*m).to_string()).collect(),
                ChannelMode::Free,
            )
            .expect("room created");
    }

    /// A background command, as the registry holds one: the `$ <command>` label
    /// `tool/bash.rs` writes, and the `Command` kind every shell watch carries.
    struct Shell(String);

    impl Watchable for Shell {
        fn label(&self) -> String {
            self.0.clone()
        }
        fn poll(&self) -> WatchPoll {
            WatchPoll {
                state: WatchState::Running,
                detail: None,
                payload: None,
                signal: None,
            }
        }
        fn check_interval(&self) -> Option<std::time::Duration> {
            None
        }
    }

    fn seed_shell(chat: &Chat, command: &str) -> WatchId {
        chat.session.watch.register_with_conditions(
            Box::new(Shell(format!("$ {command}"))),
            Vec::new(),
            None,
        )
    }

    fn texts(chat: &Chat) -> Vec<String> {
        chat.dialog_rows().iter().map(DialogRow::text).collect()
    }

    fn view(chat: &Chat) -> String {
        chat.dialog_view_rows(100)
            .iter()
            .map(|row| row.line.plain_text())
            .collect::<Vec<String>>()
            .join("\n")
    }

    /// The three sections, from three live sources, under CC's headings — and
    /// the two questions the retired directory answered are among them: who is
    /// here, and what rooms exist with who is in them.
    #[test]
    fn the_dialog_shows_agents_shells_and_rooms() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        seed_shell(&chat, "cargo build");
        seed_room(&chat, "build", &["main", USER_NAME, "scout"]);
        seed_room(&chat, "parser", &["scout", "zoe"]);
        chat.refresh_conversations();
        chat.open_background_dialog();

        let rows = texts(&chat);
        let all = rows.join("\n");
        assert!(rows.iter().any(|row| row == "  Agents (1)"), "{all}");
        assert!(rows.iter().any(|row| row == "  Shells (1)"), "{all}");
        assert!(rows.iter().any(|row| row == "  Rooms (2)"), "{all}");
        // CC separates sections with a blank row (`:438`, `marginTop={1}`),
        // and never opens the list with one.
        let blanks: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.is_empty())
            .map(|(i, _)| i)
            .collect();
        assert_eq!(blanks.len(), 2, "one above each following section: {all}");
        assert!(blanks.iter().all(|i| *i > 0), "{all}");
        assert!(
            rows.iter().any(|row| row.starts_with("@scout: ")),
            "an instance says what it is doing: {all}"
        );
        assert!(
            rows.iter()
                .any(|row| row.starts_with("$ cargo build") && row.contains("(running)")),
            "a shell wears CC's status chip: {all}"
        );
        assert!(
            rows.iter()
                .any(|row| row.starts_with("#build") && row.contains("scout")),
            "a room names its members: {all}"
        );
        // The mark is on the rooms you are *not* in (D95, kept).
        assert!(
            rows.iter()
                .any(|row| row.starts_with("#parser") && row.contains("you're not in")),
            "{all}"
        );
        assert!(
            !rows
                .iter()
                .any(|row| row.starts_with("#build") && row.contains("you're not in")),
            "{all}"
        );
    }

    /// A heading only appears where there is something to tell it apart from
    /// (CC `BackgroundTasksDialog.tsx:428`, `:438`), and a dialog with nothing
    /// in it says so in one line (`:426`).
    #[test]
    fn one_kind_needs_no_heading_and_an_empty_dialog_says_so() {
        let mut chat = test_chat();
        chat.open_background_dialog();
        assert!(chat.dialog_rows().is_empty(), "nothing to list");
        let empty = view(&chat);
        assert!(empty.contains("Background tasks"), "{empty}");
        assert!(empty.contains("No tasks currently running"), "{empty}");

        seed_agent(&chat, "scout");
        chat.refresh_conversations();
        let rows = texts(&chat);
        assert!(
            !rows.iter().any(|row| row.contains("Agents (")),
            "one kind on screen needs no label over it: {rows:?}"
        );
        seed_shell(&chat, "cargo build");
        let rows = texts(&chat);
        assert!(
            rows.iter().any(|row| row == "  Agents (1)"),
            "a second kind brings both headings back: {rows:?}"
        );
    }

    /// The subtitle counts what is *working*, in CC's words (`:404-413`). A
    /// room is a place rather than a task, so it is not among them.
    #[test]
    fn the_subtitle_counts_what_is_running() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        seed_agent(&chat, "zoe");
        seed_shell(&chat, "cargo build");
        seed_room(&chat, "build", &[USER_NAME]);
        chat.refresh_conversations();
        chat.open_background_dialog();
        let text = view(&chat);
        assert!(text.contains("2 agents · 1 active shell"), "{text}");

        chat.session.agents.stop("zoe").expect("stopped");
        chat.session.agents.stop("scout").expect("stopped");
        let text = view(&chat);
        assert!(
            text.contains("1 active shell") && !text.contains("agents"),
            "a stopped instance is not running: {text}"
        );
    }

    /// Running first, then whatever moved most recently — CC's order
    /// (`:184-192`), with the accounting store's clock standing in for the
    /// start time CC sorts by, because bingo's rows are conversations that keep
    /// moving rather than tasks that only begin.
    #[test]
    fn rows_lead_with_what_is_running_and_then_with_what_just_moved() {
        let mut chat = test_chat();
        seed_agent(&chat, "alpha");
        seed_agent(&chat, "zulu");
        chat.refresh_conversations();
        chat.session.agents.stop("alpha").expect("stopped");
        chat.open_background_dialog();
        let names: Vec<String> = chat
            .dialog_rows()
            .iter()
            .filter_map(|row| row.target.clone())
            .map(|target| match target {
                DialogTarget::Agent(name) => name,
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            names,
            vec!["zulu", "alpha"],
            "the running instance leads, whatever the alphabet says"
        );

        // With neither running, the one whose conversation moved last leads.
        chat.session.agents.stop("zulu").expect("stopped");
        chat.buffers
            .observe(BufferId::Dm("alpha".into()), 4, false, 9);
        let names: Vec<String> = chat
            .dialog_rows()
            .iter()
            .filter_map(|row| row.target.clone())
            .map(|target| match target {
                DialogTarget::Agent(name) => name,
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(names, vec!["alpha", "zulu"]);
    }

    /// One cursor over three sections, wrapping at both ends.
    #[test]
    fn the_cursor_walks_every_section_and_wraps() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        seed_shell(&chat, "cargo build");
        seed_room(&chat, "build", &[USER_NAME]);
        chat.refresh_conversations();
        chat.open_background_dialog();

        let targets = chat.dialog_targets();
        assert_eq!(targets.len(), 3, "{targets:?}");
        assert_eq!(
            chat.dialog_selection(),
            Some(targets[0].clone()),
            "an untouched cursor is on the first row"
        );
        chat.background_dialog_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(chat.dialog_selection(), Some(targets[1].clone()));
        chat.background_dialog_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(
            chat.dialog_selection(),
            Some(targets[2].clone()),
            "↓ crosses a section boundary without stopping on the heading"
        );
        chat.background_dialog_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(chat.dialog_selection(), Some(targets[0].clone()), "wraps");
        chat.background_dialog_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            chat.dialog_selection(),
            Some(targets[2].clone()),
            "and wraps the other way"
        );
    }

    /// The cursor is on a *thing*: when the rows re-sort under it, it stays on
    /// the row it was on rather than on the position that row used to hold.
    /// This is what makes `x` safe on a list that reorders itself.
    #[test]
    fn the_cursor_follows_its_row_when_the_order_changes() {
        let mut chat = test_chat();
        seed_agent(&chat, "alpha");
        seed_agent(&chat, "zulu");
        chat.refresh_conversations();
        chat.session.agents.stop("zulu").expect("stopped");
        chat.open_background_dialog();
        assert_eq!(
            chat.dialog_selection(),
            Some(DialogTarget::Agent("alpha".into())),
            "the running one leads, so the cursor opens on it"
        );
        chat.background_dialog_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(
            chat.dialog_selection(),
            Some(DialogTarget::Agent("zulu".into()))
        );

        // alpha stops: zulu is no longer second, and the cursor is still on it.
        chat.session.agents.stop("alpha").expect("stopped");
        chat.buffers
            .observe(BufferId::Dm("zulu".into()), 2, false, 7);
        assert_eq!(
            chat.dialog_rows()
                .iter()
                .filter_map(|row| row.target.clone())
                .next(),
            Some(DialogTarget::Agent("zulu".into())),
            "zulu moved to the top"
        );
        assert_eq!(
            chat.dialog_selection(),
            Some(DialogTarget::Agent("zulu".into())),
            "and the cursor went with it"
        );
    }

    /// `f` is the door to the zoomed view — for an agent, and for a **room**,
    /// which is the door D105 built its room zoom for and left unopened.
    #[test]
    fn f_foregrounds_an_agent_and_a_room() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        seed_room(&chat, "build", &[USER_NAME]);
        chat.refresh_conversations();

        chat.open_background_dialog();
        assert!(chat.background_dialog_key(KeyCode::Char('f'), KeyModifiers::NONE));
        assert_eq!(
            chat.zoom,
            Some(ZoomTarget::Agent("scout".into())),
            "the agent under the cursor"
        );
        assert!(chat.dialog.is_none(), "and the dialog closes behind it");

        chat.switch_to(None);
        chat.open_background_dialog();
        chat.background_dialog_key(KeyCode::Down, KeyModifiers::NONE);
        assert!(chat.background_dialog_key(KeyCode::Char('f'), KeyModifiers::NONE));
        assert_eq!(
            chat.zoom,
            Some(ZoomTarget::Room("build".into())),
            "and the room, which had no door at all until now"
        );
    }

    /// A shell is a command, not a conversation: `f` has nowhere to point, and
    /// the key row does not offer it.
    #[test]
    fn f_does_nothing_on_a_shell() {
        let mut chat = test_chat();
        seed_shell(&chat, "cargo build");
        chat.open_background_dialog();
        assert!(chat.background_dialog_key(KeyCode::Char('f'), KeyModifiers::NONE));
        assert!(chat.zoom.is_none());
        assert!(chat.dialog.is_some(), "and nothing was closed");
        let text = view(&chat);
        assert!(!text.contains("f to foreground"), "{text}");
    }

    /// `x` stops the instance under the cursor through the one stop path, with
    /// one warning and no confirmation — and it does nothing anywhere else.
    #[test]
    fn x_stops_the_agent_under_the_cursor_and_nothing_else() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        seed_room(&chat, "build", &[USER_NAME]);
        let shell = seed_shell(&chat, "cargo build");
        chat.refresh_conversations();
        chat.open_background_dialog();

        assert!(chat.background_dialog_key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(
            chat.session
                .agents
                .list()
                .iter()
                .find(|status| status.name == "scout")
                .map(|status| status.state),
            Some(AgentState::Stopped),
        );
        assert_eq!(
            chat.warnings.len(),
            1,
            "one stop, one warning: {:?}",
            chat.warnings
        );
        assert!(chat.dialog.is_some(), "the dialog stays open");

        // A stopped instance has nothing left to stop.
        chat.warnings.clear();
        assert!(chat.background_dialog_key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(chat.warnings.is_empty(), "{:?}", chat.warnings);

        // A running shell says why it cannot be stopped rather than doing
        // nothing: bingo keeps no handle on a promoted command's child.
        chat.dialog = Some(BackgroundDialog {
            selected: Some(DialogTarget::Shell(shell.0)),
            detail: None,
        });
        assert!(chat.background_dialog_key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert_eq!(chat.warnings.len(), 1, "{:?}", chat.warnings);
        assert!(
            chat.warnings[0].1.contains("cannot be stopped"),
            "{:?}",
            chat.warnings
        );

        // A room is not a process.
        chat.warnings.clear();
        chat.dialog = Some(BackgroundDialog {
            selected: Some(DialogTarget::Room("build".into())),
            detail: None,
        });
        assert!(chat.background_dialog_key(KeyCode::Char('x'), KeyModifiers::NONE));
        assert!(chat.warnings.is_empty(), "{:?}", chat.warnings);
    }

    /// The badges the accounting store has been keeping since D99 land here:
    /// a count per conversation, on agents and rooms alike, and the accent
    /// where the conversation said your name (D90's rule, kept).
    #[test]
    fn rows_carry_the_unread_the_store_counted() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        seed_room(&chat, "build", &[USER_NAME, "scout"]);
        chat.refresh_conversations();
        chat.buffers
            .observe(BufferId::Dm("scout".into()), 3, false, 4);
        chat.buffers
            .observe(BufferId::Channel("build".into()), 2, true, 5);
        chat.open_background_dialog();

        let rows = chat.dialog_rows();
        let agent = rows
            .iter()
            .find(|row| row.name == "@scout")
            .expect("the agent's row");
        assert_eq!(agent.chip, "3 unread", "{:?}", agent);
        assert_eq!(agent.tone, Tone::Unread { mention: false });
        let room = rows
            .iter()
            .find(|row| row.name == "#build")
            .expect("the room's row");
        assert_eq!(room.chip, "2 unread");
        assert_eq!(
            room.tone,
            Tone::Unread { mention: true },
            "a room that said your name is worth more than one that moved"
        );
        let theme = &chat.theme;
        assert_ne!(
            chip_style(agent.tone, theme),
            chip_style(room.tone, theme),
            "and the accent is what says so"
        );
        assert!(view(&chat).contains("(3 unread)"));
    }

    /// Entering a conversation reads it — through the same door the zoom
    /// already uses, so the badge cannot disagree with what is on screen.
    #[test]
    fn foregrounding_a_row_clears_its_unread() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        chat.refresh_conversations();
        chat.buffers
            .observe(BufferId::Dm("scout".into()), 3, true, 4);
        chat.open_background_dialog();
        assert_eq!(
            chat.dialog_rows()
                .iter()
                .find(|row| row.name == "@scout")
                .map(|row| row.chip.clone()),
            Some("3 unread".to_string())
        );

        chat.background_dialog_key(KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(chat.away.is_some(), "f entered the page directly");
        chat.switch_to(None);
        chat.open_background_dialog();
        assert_eq!(
            chat.dialog_rows()
                .iter()
                .find(|row| row.name == "@scout")
                .map(|row| row.chip.clone()),
            Some(String::new()),
            "the view read it"
        );
    }

    /// The key row is CC's, verb for verb (`:414`), and its conditional parts
    /// are conditional on what the selected row can be asked to do.
    #[test]
    fn the_key_row_is_ccs_and_says_only_what_is_true() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        chat.refresh_conversations();
        chat.open_background_dialog();
        let hint = chat.dialog_hint();
        assert_eq!(
            hint,
            "↑/↓ to select · Enter to view · f to foreground · x to stop · ←/Esc to close"
        );

        chat.session.agents.stop("scout").expect("stopped");
        let hint = chat.dialog_hint();
        assert_eq!(
            hint, "↑/↓ to select · Enter to view · f to foreground · ←/Esc to close",
            "a stopped instance cannot be stopped again — but its conversation is still readable"
        );
    }

    /// Enter opens the detail on the row the cursor is on; `←` is the way back
    /// and Esc closes the modal from either mode (CC's detail replaces the
    /// list, so it has no second level to peel).
    #[test]
    fn enter_opens_the_detail_and_the_ways_out_are_ccs() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        chat.refresh_conversations();
        chat.open_background_dialog();

        assert!(chat.background_dialog_key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            chat.dialog.as_ref().and_then(|d| d.detail.clone()),
            Some(DialogTarget::Agent("scout".into()))
        );
        assert!(chat.background_dialog_key(KeyCode::Left, KeyModifiers::NONE));
        assert!(chat.dialog.as_ref().is_some_and(|d| d.detail.is_none()));
        assert!(
            chat.background_dialog_key(KeyCode::Left, KeyModifiers::NONE),
            "and `←` closes the list, as CC's `←/Esc to close` says"
        );
        assert!(chat.dialog.is_none());

        chat.open_background_dialog();
        chat.background_dialog_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(chat.background_dialog_key(KeyCode::Char(' '), KeyModifiers::NONE));
        assert!(chat.dialog.is_none(), "Space closes from the detail");
    }

    /// One instance's detail, in CC's shape.
    #[test]
    fn the_agent_detail_says_what_it_is_doing_and_what_it_was_asked() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        chat.session
            .agents
            .set_prompt("scout", "Find the rendering seam".into());
        chat.session.agents.set_progress_snapshot(
            "scout",
            crate::agents::AgentProgress {
                started_at: Some(std::time::Instant::now()),
                output_tokens: 1234,
                tool_uses: 2,
                recent_activity: vec!["⏺ Read(src/lexer.rs)".into()],
            },
        );
        chat.refresh_conversations();
        chat.open_background_dialog();
        chat.background_dialog_key(KeyCode::Enter, KeyModifiers::NONE);

        let text = view(&chat);
        assert!(text.contains("@scout"), "{text}");
        assert!(text.contains("2 tool uses · 1.2k tokens"), "{text}");
        assert!(text.contains("Progress"), "{text}");
        assert!(text.contains("› ⏺ Read(src/lexer.rs)"), "{text}");
        assert!(text.contains("Prompt"), "{text}");
        assert!(text.contains("Find the rendering seam"), "{text}");
        assert!(
            text.contains("← to go back · Esc/Enter/Space to close · x to stop · f to foreground"),
            "CC's detail row, in CC's order: {text}"
        );
        assert!(
            !text.contains("tab"),
            "and the record page's door retired with the page (D108): {text}"
        );
    }

    /// A shell's detail: CC's labels, and the tail of what the command printed.
    #[test]
    fn the_shell_detail_labels_the_facts_and_shows_the_tail() {
        let mut chat = test_chat();
        let id = seed_shell(&chat, "cargo build");
        chat.open_background_dialog();
        chat.background_dialog_key(KeyCode::Enter, KeyModifiers::NONE);

        let text = view(&chat);
        assert!(text.contains("Shell details"), "{text}");
        assert!(text.contains("Status: running"), "{text}");
        assert!(text.contains("Runtime: "), "{text}");
        assert!(text.contains("Command:"), "{text}");
        assert!(text.contains("$ cargo build"), "{text}");
        assert!(
            text.contains("No output available"),
            "a running command's output is not the registry's to show: {text}"
        );
        assert!(
            text.contains("← to go back · Esc/Enter/Space to close · x to stop"),
            "{text}"
        );

        chat.session.watch.set_state(
            id,
            WatchState::Done,
            Some("exit code 0".into()),
            Some(serde_json::json!("compiling bingo\nFinished in 3.2s")),
        );
        let text = view(&chat);
        assert!(text.contains("Status: done · exit code 0"), "{text}");
        assert!(text.contains("Finished in 3.2s"), "{text}");
        assert!(text.contains("Showing 2 lines"), "{text}");
        assert!(
            !text.contains("x to stop"),
            "a finished command has nothing to stop: {text}"
        );
    }

    /// A room's detail: who is in it, how much has been said, and the last of
    /// it — the directory's room row, opened up.
    #[test]
    fn the_room_detail_names_its_members_and_its_last_words() {
        let mut chat = test_chat();
        seed_room(&chat, "build", &["main", "scout"]);
        chat.session
            .channels
            .post("scout", "build", "the parser is fixed")
            .expect("posted");
        chat.refresh_conversations();
        chat.open_background_dialog();
        chat.background_dialog_key(KeyCode::Enter, KeyModifiers::NONE);

        let text = view(&chat);
        assert!(text.contains("#build"), "{text}");
        assert!(text.contains("Members: main, scout"), "{text}");
        assert!(text.contains("you're not in"), "{text}");
        assert!(text.contains("Recent messages:"), "{text}");
        assert!(text.contains("scout: the parser is fixed"), "{text}");
        assert!(text.contains("f to foreground"), "{text}");
    }

    /// Every row is cut to the box, and a section past its window counts the
    /// rest instead of growing without bound.
    #[test]
    fn the_dialog_fits_its_box_and_bounds_its_sections() {
        let mut chat = test_chat();
        for i in 0..SECTION_ROWS_MAX + 3 {
            seed_agent(&chat, &format!("agent{i:02}"));
        }
        seed_room(
            &chat,
            "a-very-long-room-name-that-keeps-going",
            &["main", "scout", "zoe", USER_NAME],
        );
        chat.refresh_conversations();
        chat.open_background_dialog();

        let rows = texts(&chat);
        assert!(
            rows.iter().any(|row| row.contains("… 3 more agents")),
            "{rows:?}"
        );
        for width in [30usize, 50, 80, 120] {
            for row in chat.dialog_view_rows(width) {
                assert!(
                    crate::tui::line::text_width(&row.line.plain_text()) <= width,
                    "a row overran {width}: {:?}",
                    row.line.plain_text()
                );
            }
        }
    }

    /// The dialog is a modal over the composer and stays out of the way of a
    /// question that is holding up a turn (D81).
    #[test]
    fn a_pending_question_keeps_the_dialog_shut() {
        let mut chat = test_chat();
        seed_agent(&chat, "scout");
        chat.open_background_dialog();
        assert!(chat.dialog.is_some());
        chat.dialog = None;

        let (tx, _rx) = tokio::sync::oneshot::channel();
        chat.pending_ask = Some((
            crate::ui::PermissionRequest::new(
                "Allow Bash",
                "cargo test",
                vec![crate::ui::ASK_YES.into(), crate::ui::ASK_NO.into()],
            ),
            tx,
        ));
        chat.open_background_dialog();
        assert!(chat.dialog.is_none(), "the question keeps the screen");
    }
}

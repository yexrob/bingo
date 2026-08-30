//! Everything that is the surface's own and nothing that is the session's.
//! The transcript, the turn, the queue and the open interactions all live in
//! `SessionState`; what lives here is what a second client would not share —
//! where the caret is, what is scrolled, what was armed and when.
//!
//! Every time-dependent decision takes `now`, so a test never sleeps.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use bingo_sdk::{CommandSpec, Level, SessionSummary, View};

use crate::blocks::Blocks;
use crate::commands::{self, Suggestion};
use crate::composer::Composer;
use crate::dialog::Dialog;
use crate::frame::Regions;
use crate::history::PromptHistory;
use crate::scroll::Scroll;

/// How long a transient notice holds the status line's middle slot (§3).
pub const NOTICE: Duration = Duration::from_secs(4);
/// A second ctrl+c within this window leaves.
pub const EXIT_WINDOW: Duration = Duration::from_secs(2);

/// A message that is not transcript: an ack, a warning, a hint.
#[derive(Clone, Debug)]
pub struct Notice {
    pub level: Level,
    pub text: String,
    pub until: Instant,
}

/// The command dropdown's own state; its rows are derived from the composer.
#[derive(Clone, Copy, Debug, Default)]
pub struct Menu {
    pub selected: usize,
    /// Esc closed it; typing opens it again.
    pub dismissed: bool,
}

/// The `/resume` picker, filled by the host and answered by Enter.
#[derive(Clone, Debug, Default)]
pub struct Picker {
    pub sessions: Vec<SessionSummary>,
    pub selected: usize,
}

/// What the last frame put on the screen: the blocks it rendered, where it
/// cut the regions, how tall the transcript came out and which of its lines
/// was at the top. A key or a click is answered against this — nothing else
/// knows how many lines there are to scroll through.
///
/// It is a memo of the draw, not state of its own: every field is what the
/// reducer and the terminal's size already imply, which is why drawing may
/// fill it from behind a shared borrow.
#[derive(Debug, Default)]
pub struct Painted {
    pub blocks: Blocks,
    pub regions: Regions,
    /// The transcript's height in wrapped lines.
    pub height: usize,
    /// The first transcript line the frame showed.
    pub top: usize,
}

/// What the kernel told this surface it can offer: the commands a session
/// runs, and the ids each of their catalogued arguments may take. Read once
/// at start — the dropdown ranks them, it does not watch them.
#[derive(Clone, Debug, Default)]
pub struct Catalogs {
    pub commands: Vec<CommandSpec>,
    pub values: commands::Catalogues,
}

/// The `ctrl+g` switcher over the sessions in the tree. Its rows are derived
/// from the tree at render time; only the cursor is the surface's own.
#[derive(Clone, Copy, Debug, Default)]
pub struct Switcher {
    pub selected: usize,
}

#[derive(Debug)]
pub struct Ui {
    pub composer: Composer,
    pub history: PromptHistory,
    pub dialog: Dialog,
    pub scroll: Scroll,
    pub help: bool,
    /// The `ctrl+t` panel over the viewed session's plugin state. What it
    /// draws is the reducer's; open is all this surface remembers.
    pub panel: bool,
    pub menu: Menu,
    pub picker: Option<Picker>,
    pub switcher: Option<Switcher>,
    pub notices: Vec<Notice>,
    /// A command's `View`, shown until the next key.
    pub block: Option<View>,
    /// When ctrl+c was pressed on an empty composer.
    pub armed: Option<Instant>,
    /// What the kernel offers the dropdown, read once at start.
    pub catalogs: Catalogs,
    /// An `open` is in flight; the swap happens when it lands.
    pub opening: bool,
    /// When this surface started, which is what the spinner turns on.
    pub started: Instant,
    /// The frame as the last draw left it.
    pub painted: RefCell<Painted>,
}

impl Ui {
    pub fn new(history: Vec<String>, started: Instant) -> Self {
        Self {
            composer: Composer::default(),
            history: PromptHistory::new(history),
            dialog: Dialog::default(),
            scroll: Scroll::default(),
            help: false,
            panel: false,
            menu: Menu::default(),
            picker: None,
            switcher: None,
            notices: Vec::new(),
            block: None,
            armed: None,
            catalogs: Catalogs::default(),
            opening: false,
            started,
            painted: RefCell::default(),
        }
    }

    pub fn notify(&mut self, level: Level, text: impl Into<String>, now: Instant) {
        self.notices.push(Notice {
            level,
            text: text.into(),
            until: now + NOTICE,
        });
    }

    /// Drop the notices whose time is up. Drawing never mutates, so the loop
    /// calls this.
    pub fn expire(&mut self, now: Instant) {
        self.notices.retain(|n| n.until > now);
    }

    /// Every command the dropdown may offer: the surface's own and the
    /// kernel's, in that order.
    pub fn commands(&self) -> Vec<CommandSpec> {
        let mut all = commands::local_specs();
        all.extend(self.catalogs.commands.iter().cloned());
        all
    }

    /// The dropdown's rows for the line being typed. Empty means no dropdown.
    pub fn suggestions(&self) -> Vec<Suggestion> {
        if self.menu.dismissed {
            return Vec::new();
        }
        commands::suggestions(
            self.composer.text(),
            &self.commands(),
            &self.catalogs.values,
        )
    }

    pub fn selected_suggestion(&self) -> Option<Suggestion> {
        let rows = self.suggestions();
        rows.get(self.menu.selected.min(rows.len().saturating_sub(1)))
            .cloned()
    }

    /// The composer changed: the dropdown reopens and the history walk ends.
    pub fn edited(&mut self) {
        self.menu = Menu::default();
        self.history.reset();
    }

    /// Whether a second ctrl+c would leave.
    pub fn exit_armed(&self, now: Instant) -> bool {
        self.armed
            .is_some_and(|at| now.duration_since(at) < EXIT_WINDOW)
    }

    /// How tall the transcript came out last frame and how many rows it had
    /// to show it in — what a scroll is measured against.
    pub fn transcript(&self) -> (usize, usize) {
        let painted = self.painted.borrow();
        (painted.height, painted.regions.transcript.height as usize)
    }

    /// The rows a page key moves by: the screenful a person is looking at.
    pub fn page(&self) -> usize {
        self.transcript().1.max(1)
    }

    /// The spinner frame for this instant.
    pub fn spinner(&self, now: Instant) -> &'static str {
        crate::theme::spinner(now.duration_since(self.started))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ui() -> Ui {
        Ui::new(Vec::new(), Instant::now())
    }

    #[test]
    fn a_notice_lives_exactly_as_long_as_its_window() {
        let now = Instant::now();
        let mut ui = ui();
        ui.notify(Level::Warn, "careful", now);
        ui.expire(now + NOTICE - Duration::from_millis(1));
        assert_eq!(ui.notices.len(), 1);
        ui.expire(now + NOTICE);
        assert!(ui.notices.is_empty());
    }

    #[test]
    fn the_exit_arm_lapses() {
        let now = Instant::now();
        let mut ui = ui();
        ui.armed = Some(now);
        assert!(ui.exit_armed(now + Duration::from_millis(1999)));
        assert!(!ui.exit_armed(now + EXIT_WINDOW));
    }

    #[test]
    fn dismissing_the_dropdown_hides_it_until_the_next_edit() {
        let mut ui = ui();
        ui.composer.insert("/he");
        assert!(!ui.suggestions().is_empty());
        ui.menu.dismissed = true;
        assert!(ui.suggestions().is_empty());
        ui.edited();
        assert!(!ui.suggestions().is_empty());
    }
}

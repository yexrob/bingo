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
use crate::layers::{self, Reveal};
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
#[derive(Clone, Debug, Default, PartialEq)]
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
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Switcher {
    pub selected: usize,
}

/// What is over the frame. One at a time: focus moves into a layer and back
/// out, never sideways (§7).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Open {
    #[default]
    Nothing,
    /// The binding table and the commands, as a sheet.
    Help,
    /// What the plugins wrote into the session in view, as a sheet.
    Panel,
    /// The `/resume` list, as a sheet.
    Picker(Picker),
    /// The tree, as a card above the input box.
    Switcher(Switcher),
}

impl Open {
    /// Whether the keyboard belongs to it while it is open. The two lists do
    /// answer keys; the two panels are read while a person goes on typing.
    pub fn captures(&self) -> bool {
        matches!(self, Open::Picker(_) | Open::Switcher(_))
    }

    /// How many frames its arrival takes: a card comes down, a sheet rises.
    fn frames(&self) -> u16 {
        match self {
            Open::Switcher(_) => layers::CARD_FRAMES,
            _ => layers::SHEET_FRAMES,
        }
    }
}

/// What is open and how far in it is. Closing runs the arrival backwards, so
/// what is going stays on the screen until it has gone.
#[derive(Clone, Debug)]
pub struct Layer {
    pub open: Open,
    since: Instant,
    closing: bool,
}

impl Layer {
    fn shut(now: Instant) -> Self {
        Self {
            open: Open::Nothing,
            since: now,
            closing: false,
        }
    }

    /// How far in it is at this instant.
    pub fn reveal(&self, now: Instant) -> Reveal {
        Reveal::at(self.open.frames(), self.since, now, self.closing)
    }

    /// How far in it is, or nothing at all when there is nothing over the
    /// frame — including the moment after the last frame of a leaving.
    pub fn drawn(&self, now: Instant) -> Option<Reveal> {
        let reveal = self.reveal(now);
        (self.open != Open::Nothing && !reveal.gone()).then_some(reveal)
    }

    /// Open this one, from the first frame.
    pub fn show(&mut self, open: Open, now: Instant) {
        self.open = open;
        self.since = now;
        self.closing = false;
    }

    /// Start closing whatever is open; [`Ui::expire`] takes it away once the
    /// last frame of its leaving has been drawn.
    pub fn close(&mut self, now: Instant) {
        if self.open == Open::Nothing || self.closing {
            return;
        }
        self.since = now;
        self.closing = true;
    }

    /// Open this one, or close it when it already is: what a toggle chord does.
    pub fn toggle(&mut self, open: Open, now: Instant) {
        match self.open == open && !self.closing {
            true => self.close(now),
            false => self.show(open, now),
        }
    }

    #[cfg(test)]
    pub fn is(&self, open: &Open) -> bool {
        &self.open == open && !self.closing
    }

    pub fn showing(&self) -> bool {
        self.open != Open::Nothing && !self.closing
    }

    /// Whether the keyboard belongs to it. One on its way out has already
    /// given the keys back.
    pub fn captures(&self) -> bool {
        self.showing() && self.open.captures()
    }
}

#[derive(Debug)]
pub struct Ui {
    pub composer: Composer,
    pub history: PromptHistory,
    pub dialog: Dialog,
    pub scroll: Scroll,
    /// The one layer over the frame, and how far it has come in.
    pub layer: Layer,
    pub menu: Menu,
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
            layer: Layer::shut(started),
            menu: Menu::default(),
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

    /// Drop what has run out: a notice past its window, a layer that has
    /// finished leaving. Drawing never mutates, so the loop calls this.
    pub fn expire(&mut self, now: Instant) {
        self.notices.retain(|n| n.until > now);
        if self.layer.closing && self.layer.reveal(now).gone() {
            self.layer = Layer::shut(now);
        }
    }

    /// Whether the next frame would draw a layer differently.
    pub fn layer_moving(&self, now: Instant) -> bool {
        self.layer.open != Open::Nothing && self.layer.reveal(now).moving()
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

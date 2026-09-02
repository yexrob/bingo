//! Everything that is the surface's own and nothing that is the session's.
//! The transcript, the turn, the queue and the open interactions all live in
//! `SessionState`; what lives here is what a second client would not share —
//! where the caret is, what is scrolled, what was armed and when.
//!
//! Every time-dependent decision takes `now`, so a test never sleeps.

use std::cell::RefCell;
use std::time::{Duration, Instant};

use std::collections::BTreeSet;

use bingo_sdk::{Action, CommandSpec, Level, Seq, SessionState, SessionSummary, TurnId, View};

use crate::blocks::Blocks;
use crate::clock::{Anim, FRAME, Now};
use crate::commands::{self, Suggestion};
use crate::complete;
use crate::composer::Composer;
use crate::dialog::Dialog;
use crate::fold::Folds;
use crate::frame::Regions;
use crate::history::PromptHistory;
use crate::layers::{self, Reveal};
use crate::pager::Pager;
use crate::rail::{CardId, Pin};
use crate::rewind::Rewind;
use crate::scroll::Scroll;
use crate::search::Search;
use crate::select::Select;
use crate::views::Marks;

/// How long a transient notice holds the status line's middle slot (§3),
/// between the frames it fades in over and the ones it fades out over.
pub const NOTICE: Duration = Duration::from_secs(4);
/// A notice fades into the slot over two frames, and out of it over two (§6).
pub const NOTICE_FADE: Duration = Duration::from_millis(2 * FRAME.as_millis() as u64);
/// A second ctrl+c within this window leaves.
pub const EXIT_WINDOW: Duration = Duration::from_secs(2);
/// A transcript that has just been stepped into crossfades through dim over
/// this long: two frames (§6).
pub const SWITCH: Duration = Duration::from_millis(2 * FRAME.as_millis() as u64);

/// A message that is not transcript: an ack, a warning, a hint.
#[derive(Clone, Debug)]
pub struct Notice {
    pub level: Level,
    pub text: String,
    /// What it is about, said after it in dim: the line a refused intent
    /// carried, so a person sees which of theirs came back.
    pub about: Option<String>,
    /// When it took the slot — one notice is read at a time, and the next
    /// waits (§3). `None` until its turn comes.
    shown: Option<Instant>,
}

impl Notice {
    /// How strongly it is being said: 0 as it arrives, 1 while it is there to
    /// be read, 0 again as it goes — and nothing at all while it waits.
    pub fn strength(&self, now: Now) -> Option<f32> {
        let shown = self.shown?;
        if !now.motion {
            return Some(1.0);
        }
        let held = NOTICE_FADE + NOTICE;
        match now.since(shown) {
            age if age < NOTICE_FADE => Some(Anim::new(shown, NOTICE_FADE).progress(now.instant)),
            age if age < held => Some(1.0),
            _ => Some(1.0 - Anim::new(shown + held, NOTICE_FADE).progress(now.instant)),
        }
    }

    /// Whether it has said its piece and left.
    fn gone(&self, now: Instant) -> bool {
        self.shown
            .is_some_and(|shown| now.saturating_duration_since(shown) >= self.life())
    }

    fn life(&self) -> Duration {
        NOTICE_FADE + NOTICE + NOTICE_FADE
    }
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
    /// The card on the screen, when one is open.
    pub card: Option<Card>,
    /// Where each rail card landed, in the rail's own rows: what a click on
    /// the rail is answered against.
    pub rail: Vec<(CardId, std::ops::Range<usize>)>,
}

/// A card as it was drawn: where its box is, and which option each of its
/// rows belongs to — what a click on it needs to know.
#[derive(Clone, Debug, Default)]
pub struct Card {
    pub area: ratatui::layout::Rect,
    pub options: Vec<Option<usize>>,
}

/// What the kernel told this surface it can offer: the commands a session
/// runs, and the ids each of their catalogued arguments may take. Read once
/// at start — the dropdown ranks them, it does not watch them.
#[derive(Clone, Debug, Default)]
pub struct Catalogs {
    pub commands: Vec<CommandSpec>,
    pub values: commands::Catalogues,
}

/// The `ctrl+g` switcher over the sessions in the tree and the root's stored
/// descendants. Its rows are derived at render time; what is the surface's
/// own is the cursor and the one listing the host answered with when the card
/// opened — nothing here watches the store.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Switcher {
    pub selected: usize,
    /// Empty until the read the opening spawned lands.
    pub stored: Vec<SessionSummary>,
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
    /// One block, whole, as a sheet.
    Pager(Pager),
    /// The turns of this transcript, as a card above the input box.
    Rewind(Rewind),
    /// The tree, as a card above the input box.
    Switcher(Switcher),
}

/// An action a person fired, and where the session's stream was when they
/// did. The mark on the button stays until the stream moves, which is the
/// ack: nothing here is a second copy of what the reducer already knows.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub action: Action,
    pub seq: Seq,
}

impl Open {
    /// Whether the keyboard belongs to it while it is open. The lists and the
    /// panel answer keys; help is read while a person goes on typing.
    pub fn captures(&self) -> bool {
        matches!(
            self,
            Open::Picker(_) | Open::Pager(_) | Open::Switcher(_) | Open::Rewind(_) | Open::Panel
        )
    }

    /// How many frames its arrival takes: a card comes down, a sheet rises.
    fn frames(&self) -> u16 {
        match self {
            Open::Switcher(_) | Open::Rewind(_) => layers::CARD_FRAMES,
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

    /// How far in it is at this instant. Where nothing may move, a layer is
    /// whole the moment it opens and gone the moment it closes.
    pub fn reveal(&self, now: Now) -> Reveal {
        if !now.motion {
            return match self.closing {
                true => Reveal::none(self.open.frames()),
                false => Reveal::whole(self.open.frames()),
            };
        }
        Reveal::at(self.open.frames(), self.since, now.instant, self.closing)
    }

    /// How far in it is, or nothing at all when there is nothing over the
    /// frame — including the moment after the last frame of a leaving.
    pub fn drawn(&self, now: Now) -> Option<Reveal> {
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
    /// `ctrl+f`: the query in the status line's row, while it is there.
    pub search: Option<Search>,
    /// `↓` on an empty composer: the quick cycle's strip has the status line
    /// (§3). Which chip it marks is the session the tree is showing, so being
    /// open is the whole of what there is to remember.
    pub cycling: bool,
    /// What the transcript is holding: a focused block, a run of cells.
    pub select: Select,
    pub notices: Vec<Notice>,
    /// A command's `View`, shown until the next key.
    pub block: Option<View>,
    /// The panels a person pinned into the rail, per session (ADR-0013 §4:
    /// where a panel sits is the surface's answer, not the plugin's).
    pub pinned: BTreeSet<Pin>,
    /// The rail card the keyboard talks to; `tab` cycles it, a click takes it.
    pub focus: Option<CardId>,
    /// The panel sheet's cursor: the row `⏎` would pin.
    pub panel: usize,
    /// An action fired and not yet answered.
    pub pending: Option<Pending>,
    /// How much of each block a person opened or shut, with `ctrl+o` or a
    /// click (§7). A block that is not in it wears its kind's own start
    /// ([`crate::fold`]), so the default is never written down twice.
    pub folds: Folds,
    /// When ctrl+c was pressed on an empty composer.
    pub armed: Option<Instant>,
    /// The turn this surface has asked to stop, so the activity row answers
    /// the key on the very frame it was pressed (§7). The kernel decides what
    /// an interrupt does and its `TurnCompleted` is the end of the story; this
    /// is a fact about the keypress, not a copy of session state, and it is
    /// kept by turn id so it can never speak for the turn after this one.
    pub stop_asked: Option<TurnId>,
    /// An `esc` closed nothing, so the next one is the second of `esc esc`.
    /// It needs no clock: any other key clears it.
    pub esc_armed: bool,
    /// What the kernel offers the dropdown, read once at start.
    pub catalogs: Catalogs,
    /// An `open` is in flight; the swap happens when it lands.
    pub opening: bool,
    /// Whether the terminal window is being looked at. A window nobody is
    /// looking at is the one that may say something to the desktop (§6).
    pub focused: bool,
    /// When the session in view was last stepped into: what the transcript
    /// crossfades from.
    pub switched: Option<Instant>,
    /// The frame as the last draw left it.
    pub painted: RefCell<Painted>,
    /// The paths the `@` dropdown ranks, walked when the first one asks.
    files: RefCell<Files>,
}

/// One reading of a directory, kept until the session's own directory changes.
#[derive(Debug, Default)]
struct Files {
    cwd: String,
    paths: Vec<String>,
}

impl Ui {
    pub fn new(history: Vec<String>, now: Instant) -> Self {
        Self {
            composer: Composer::default(),
            history: PromptHistory::new(history),
            dialog: Dialog::default(),
            scroll: Scroll::default(),
            layer: Layer::shut(now),
            menu: Menu::default(),
            search: None,
            cycling: false,
            select: Select::default(),
            notices: Vec::new(),
            block: None,
            pinned: BTreeSet::new(),
            focus: None,
            panel: 0,
            pending: None,
            folds: Folds::new(),
            armed: None,
            stop_asked: None,
            esc_armed: false,
            catalogs: Catalogs::default(),
            opening: false,
            focused: true,
            switched: None,
            painted: RefCell::default(),
            files: RefCell::default(),
        }
    }

    /// What the frame marks on a view it is about to draw: the action a
    /// person fired, while the session's stream has not moved since. The next
    /// frame from the session — the ack, or what the action changed — is the
    /// answer, so nothing has to be remembered to put the mark away.
    pub fn marks(&self, state: &SessionState) -> Marks {
        Marks {
            pending: self
                .pending
                .as_ref()
                .filter(|pending| pending.seq == state.seq)
                .map(|pending| pending.action.clone()),
        }
    }

    pub fn notify(&mut self, level: Level, text: impl Into<String>, now: Instant) {
        self.say(level, text.into(), None, now);
    }

    /// A notice that names what it is about — the line an intent carried,
    /// said after it in dim.
    pub fn notify_about(&mut self, level: Level, text: String, about: String, now: Instant) {
        self.say(level, text, Some(about), now);
    }

    /// One notice is read at a time; the next takes the slot when this one has
    /// left it.
    fn say(&mut self, level: Level, text: String, about: Option<String>, now: Instant) {
        let free = self.notices.is_empty();
        self.notices.push(Notice {
            level,
            text,
            about,
            shown: free.then_some(now),
        });
    }

    /// The notice on the status line, while one is being said.
    pub fn notice(&self) -> Option<&Notice> {
        self.notices.first()
    }

    /// Drop what has run out and start what was waiting for it: a notice past
    /// its window, a layer that has finished leaving. Drawing never mutates,
    /// so the loop calls this.
    pub fn expire(&mut self, now: Now) {
        if self.notices.first().is_some_and(|n| n.gone(now.instant)) {
            self.notices.remove(0);
        }
        if let Some(next) = self.notices.first_mut()
            && next.shown.is_none()
        {
            next.shown = Some(now.instant);
        }
        if self.layer.closing && self.layer.reveal(now).gone() {
            self.layer = Layer::shut(now.instant);
        }
    }

    /// Whether the transcript is still crossfading into the session a person
    /// has just stepped into (§6).
    pub fn crossfading(&self, now: Now) -> bool {
        now.motion && self.switched.is_some_and(|at| now.since(at) < SWITCH)
    }

    /// Whether the next frame would draw a layer differently.
    pub fn layer_moving(&self, now: Now) -> bool {
        self.layer.open != Open::Nothing && self.layer.reveal(now).moving()
    }

    /// Every command the dropdown may offer: the surface's own and the
    /// kernel's, in that order.
    pub fn commands(&self) -> Vec<CommandSpec> {
        let mut all = commands::local_specs();
        all.extend(self.catalogs.commands.iter().cloned());
        all
    }

    /// The dropdown's rows for the line being typed: a `/` command, or an `@`
    /// path from the session's own directory. Empty means no dropdown.
    pub fn suggestions(&self, cwd: &str) -> Vec<Suggestion> {
        if self.menu.dismissed {
            return Vec::new();
        }
        let line = self.composer.text();
        match complete::mention(line) {
            Some(partial) => self.paths(cwd, partial, line),
            None => commands::suggestions(line, &self.commands(), &self.catalogs.values),
        }
    }

    /// The `@` rows. The walk is a memo of the directory, taken when the first
    /// mention asks for it and thrown away when the session's own directory
    /// changes — nothing here is a second copy of what is on disk.
    fn paths(&self, cwd: &str, partial: &str, line: &str) -> Vec<Suggestion> {
        let mut files = self.files.borrow_mut();
        if files.cwd != cwd {
            *files = Files {
                cwd: cwd.to_string(),
                paths: complete::walk(std::path::Path::new(cwd)),
            };
        }
        complete::rank(partial, &files.paths)
            .into_iter()
            .map(|path| Suggestion {
                value: complete::completed(line, &path),
                label: format!("@{path}"),
                hint: String::new(),
            })
            .collect()
    }

    pub fn selected_suggestion(&self, cwd: &str) -> Option<Suggestion> {
        let rows = self.suggestions(cwd);
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

    /// The whole transcript as the last frame rendered it, one string a line:
    /// what a search looks through.
    pub fn transcript_text(&self) -> Vec<String> {
        let painted = self.painted.borrow();
        painted
            .blocks
            .window(0, painted.height)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    /// The rows a page key moves by: the screenful a person is looking at.
    pub fn page(&self) -> usize {
        self.transcript().1.max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{later, scene};

    fn ui() -> Ui {
        Ui::new(Vec::new(), Instant::now())
    }

    #[test]
    fn a_notice_lives_its_window_and_the_frames_it_arrives_and_leaves_in() {
        let (mut ui, now) = scene();
        ui.notify(Level::Warn, "careful", now.instant);
        let life = (NOTICE_FADE + NOTICE + NOTICE_FADE).as_millis() as i64;
        ui.expire(later(now, life - 1));
        assert_eq!(ui.notices.len(), 1);
        ui.expire(later(now, life));
        assert!(ui.notices.is_empty());
    }

    #[test]
    fn a_notice_fades_in_over_two_frames_holds_and_fades_out() {
        let (mut ui, now) = scene();
        ui.notify(Level::Warn, "careful", now.instant);
        let at = |ms| {
            ui.notice()
                .and_then(|notice| notice.strength(later(now, ms)))
        };
        assert_eq!(at(0), Some(0.0), "it starts dim");
        assert_eq!(at(33), Some(0.5), "and arrives over two frames");
        assert_eq!(at(66), Some(1.0));
        assert_eq!(at(2_000), Some(1.0), "held while it is read");
        assert_eq!(at(4_066), Some(1.0), "to the end of its window");
        assert_eq!(at(4_099), Some(0.5), "then it leaves the same way");
        assert_eq!(at(4_132), Some(0.0));
    }

    #[test]
    fn a_still_surface_says_a_notice_at_full_strength() {
        let (mut ui, now) = scene();
        ui.notify(Level::Warn, "careful", now.instant);
        let still = crate::test_support::still(now);
        assert_eq!(ui.notice().and_then(|n| n.strength(still)), Some(1.0));
    }

    #[test]
    fn the_next_notice_waits_until_the_first_has_left() {
        let (mut ui, now) = scene();
        ui.notify(Level::Warn, "first", now.instant);
        ui.notify(Level::Info, "second", now.instant);
        assert_eq!(
            ui.notice().map(|n| n.text.clone()).as_deref(),
            Some("first")
        );
        assert_eq!(
            ui.notices[1].strength(now),
            None,
            "the one behind it is not being said at all"
        );

        let life = (NOTICE_FADE + NOTICE + NOTICE_FADE).as_millis() as i64;
        ui.expire(later(now, life));
        assert_eq!(
            ui.notice().map(|n| n.text.clone()).as_deref(),
            Some("second")
        );
        assert_eq!(
            ui.notices[0].strength(later(now, life)),
            Some(0.0),
            "and it arrives from the beginning of its own life"
        );
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
        assert!(!ui.suggestions("/tmp/project").is_empty());
        ui.menu.dismissed = true;
        assert!(ui.suggestions("/tmp/project").is_empty());
        ui.edited();
        assert!(!ui.suggestions("/tmp/project").is_empty());
    }
}

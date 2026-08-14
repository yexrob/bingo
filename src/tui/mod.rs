//! Terminal front end.
//!
//! Composition is declarative — Ink's shape without Ink's runtime (D38):
//!
//! - [`el`] is the element tree: components are plain functions returning
//!   elements, and one render walk yields rows, click ranges and the caret.
//! - [`statics`] is the Static region: the transcript as a block list with a
//!   frozen prefix (write-once scrollback, lazy freezing — Ink `<Static>`).
//! - [`chrome`] declares every section below the transcript.
//! - [`transcript`] is the alternate-screen pager over the whole session
//!   (`ctrl+o`), the compensation for write-once scrollback.
//! - [`composer`] owns the prompt's readline state (kill ring, the
//!   `ctrl+x ctrl+e` chord) and the `$EDITOR` round trip.
//! - [`buffer`] is the conversation engine (D88): every conversation — hub, DM,
//!   channel, team board — as one shape, holding read cursors and drafts while
//!   the transcripts stay in the domain stores they already live in.
//! - [`bufferview`] is its host side (D89): one terminal, one flow, one active
//!   conversation. Switching splices a divider and a replay into the flow and
//!   holds the conversations you are not in; the composer routes to whichever
//!   one is active.
//! - [`chat`] is the state machine and the transcript block builder
//!   (`build_rows`); [`slash`] owns slash command metadata and pure
//!   suggestion/help transformations; [`complete`] owns the fuzzy scorer, the
//!   `@` mention dropdown and the per-command argument sources.
//! - [`app`] is the event loop and the frame assembly.
//! - [`notify`] builds the attention channel's bytes (bell, notification OSC,
//!   terminal title); like [`gfx`], it builds and never writes.
//! - [`view`] converts document rows to ratatui text; [`term`] is the only
//!   module that writes to the terminal.
//!
//! The renderer-agnostic contract (`UiEvent`, the dialog types, `tui_hooks`)
//! lives in [`crate::ui`].

pub mod activities;
mod app;
pub mod avatar;
pub mod buffer;
pub mod bufferview;
pub mod chat;
mod chrome;
pub mod complete;
pub mod composer;
pub mod convbar;
pub mod directory;
pub mod el;
pub mod gfx;
pub mod highlight;
pub mod history;
pub mod images;
pub mod input;
pub mod keys;
pub mod line;
pub mod markdown;
pub mod math;
pub mod model_menu;
pub mod motion;
pub mod notify;
pub mod perspective;
pub mod perspective_ui;
pub mod picker;
pub mod slash;
pub mod statics;
pub mod switcher;
pub(crate) mod term;
#[cfg(test)]
pub(crate) mod test_util;
pub mod theme;
pub(crate) mod transcript;
mod view;

use std::io::{Write as IoWrite, stdout};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::query::Session;
use crate::tui::chat::Chat;
use crate::tui::theme::{Theme, ThemeSetting};

/// True between the moment raw mode is entered and the moment the terminal is
/// handed back, so a clean teardown leaves the panic hook with nothing to do.
static TUI_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Which teardown the active host owes (alternate screen or inline).
static TUI_FULLSCREEN: AtomicBool = AtomicBool::new(false);
/// Whether this session ever set a terminal title (D79). The teardown hands the
/// title back only when it took it: a session with `notifications: "off"` never
/// wrote one, and clearing it would throw away whatever the shell had put there.
static TUI_TITLE: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK: std::sync::Once = std::sync::Once::new();

std::thread_local! {
    /// Set on the thread that drives the host — the runtime's root future, which
    /// `block_on` polls on the calling thread and never migrates. A panic there
    /// unwinds out of `main` and the terminal goes with it, which is what the
    /// hook is for.
    ///
    /// Panics on any other thread are a spawned task's, and tokio contains
    /// those: the turn task's is already caught by `supervise_turn` and turned
    /// into the recoverable `TURN_LOST` state, so tearing the screen down for it
    /// would break a session that is still alive. `Cell<bool>` has no destructor,
    /// so reading it from a panic hook is safe at any point in a thread's life.
    static DRIVES_TERMINAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Hand the terminal back, best-effort at each step and in the reverse order of
/// setup: a failure halfway must not leave raw mode or the alternate screen
/// behind. Every step is a fixed escape sequence, so this is safe to run from a
/// panic hook — nothing here allocates a message or unwinds.
fn restore_terminal(fullscreen: bool) {
    let mut out = stdout();
    // The title is this session's only mark outside its own screen area, so it
    // goes back first, and only if it was ever taken (D79).
    if TUI_TITLE.swap(false, Ordering::SeqCst) {
        let _ = out.write_all(notify::RESET_TITLE);
        let _ = out.flush();
    }
    // Paired with the push in `setup_terminal`, and a no-op when nothing was
    // pushed — so every path out of the TUI, this one included, can call it
    // without first working out whether the terminal took the flags (D86).
    term::pop_keyboard_enhancement(&mut out);
    if fullscreen {
        let _ = execute!(
            out,
            DisableBracketedPaste,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    } else {
        let _ = execute!(out, DisableBracketedPaste);
    }
    let _ = disable_raw_mode();
    let _ = execute!(out, crossterm::cursor::Show);
}

/// Take the terminal: raw mode, the host's screen, and the keyboard
/// enhancement flags the terminal admitted to understanding. The claim goes in
/// between, so a panic during the screen setup still owes a teardown.
///
/// Both the initial setup and the resume after an `$EDITOR` suspend go through
/// here — the two used to be one inline block that only the startup path could
/// reach, and a second entry point would have been a second answer to "what
/// does this host have switched on".
fn setup_terminal(fullscreen: bool) -> std::io::Result<()> {
    enable_raw_mode()?;
    claim_terminal(fullscreen);
    let mut out = stdout();
    if fullscreen {
        execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
    } else {
        execute!(out, EnableBracketedPaste)?;
    }
    term::push_keyboard_enhancement(&mut out);
    Ok(())
}

/// The terminal, handed to a child process for the length of its run (the
/// `$EDITOR` compose, D86) and taken back afterwards.
///
/// The hand-over is the full teardown, not a partial one: an editor needs raw
/// mode off, the alternate screen gone and the cursor visible, and it needs
/// the panic hook to know the terminal is not ours while it runs. Taking it
/// back is [`setup_terminal`] again, so the host resumes with exactly what it
/// started with.
pub(crate) struct TerminalSuspend {
    fullscreen: bool,
    resumed: bool,
}

impl TerminalSuspend {
    /// Take the terminal back. Idempotent, and `Drop` runs it, so an early
    /// return or a panic between suspend and resume cannot leave the host
    /// without raw mode.
    pub(crate) fn resume(&mut self) {
        if self.resumed {
            return;
        }
        self.resumed = true;
        let _ = setup_terminal(self.fullscreen);
    }
}

impl Drop for TerminalSuspend {
    fn drop(&mut self) {
        self.resume();
    }
}

/// Hand the terminal back for a child process to use. When no host holds it
/// (tests, headless), the guard is inert in both directions rather than
/// enabling raw mode on a terminal nobody claimed.
pub(crate) fn suspend_terminal() -> TerminalSuspend {
    let fullscreen = TUI_FULLSCREEN.load(Ordering::SeqCst);
    let held = release_terminal(restore_terminal);
    TerminalSuspend {
        fullscreen,
        resumed: !held,
    }
}

/// While an alternate-screen modal is open over the *inline* host, the teardown
/// the panic hook owes changes shape: raw mode alone is no longer enough, the
/// alternate screen and the mouse capture have to come off too. The guard flips
/// [`TUI_FULLSCREEN`] for as long as the modal is up and puts the host's own
/// answer back on the way out — so a panic inside the pager restores the same
/// screen the pager would have restored.
pub(crate) struct AltScreenClaim(bool);

impl AltScreenClaim {
    pub(crate) fn enter() -> Self {
        Self(TUI_FULLSCREEN.swap(true, Ordering::SeqCst))
    }
}

impl Drop for AltScreenClaim {
    fn drop(&mut self) {
        TUI_FULLSCREEN.store(self.0, Ordering::SeqCst);
    }
}

/// Claim the terminal for the panic hook, on the thread that drives the host.
/// Paired with exactly one [`release_terminal`], whichever of the two paths gets
/// there first.
fn claim_terminal(fullscreen: bool) {
    TUI_FULLSCREEN.store(fullscreen, Ordering::SeqCst);
    DRIVES_TERMINAL.set(true);
    TUI_ACTIVE.store(true, Ordering::SeqCst);
}

/// Hand the terminal back if this thread's claim is still live, and report
/// whether this call was the one that did it. Idempotent by construction: the
/// flag is swapped, so the clean teardown and a panic during it cannot both
/// restore. `restore` is passed in so the flag protocol can be tested without a
/// terminal to write escape sequences at.
fn release_terminal(restore: impl FnOnce(bool)) -> bool {
    if !DRIVES_TERMINAL.get() || !TUI_ACTIVE.swap(false, Ordering::SeqCst) {
        return false;
    }
    DRIVES_TERMINAL.set(false);
    restore(TUI_FULLSCREEN.load(Ordering::SeqCst));
    true
}

/// Install the terminal safety net once per process. A panic on the host's own
/// thread used to leave the user in raw mode inside the alternate screen with an
/// invisible cursor, staring at the frame that was on screen when it died — the
/// message went to a terminal that could no longer render it.
fn install_panic_hook() {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            release_terminal(restore_terminal);
            previous(info);
        }));
    });
}

/// Start a TUI session. `fullscreen=true` (default): alternate-screen canvas
/// with in-app scrolling and mouse interaction; `fullscreen=false`: inline
/// mode, where finalized content enters terminal scrollback and the viewport
/// only paints the live tail.
pub async fn run_tui_session(
    session: Arc<Session>,
    expand_rx: tokio::sync::watch::Receiver<bool>,
    fullscreen: bool,
    startup_notes: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Query the terminal background colour once before raw mode, for the auto
    // theme resolution (the probe itself toggles raw mode temporarily and
    // reads /dev/tty directly).
    let detected_background = Theme::detect_system_theme().await;

    // Version-check warm-up: skip when the cache is fresh (24h), otherwise fetch the latest release in the background and write the cache
    // (the welcome-card data source; failures are silent and never block startup). The headless (--print) path never goes through here.
    crate::update::spawn_background_check(session.home.clone());

    // Probe the kitty graphics capability for both hosts: inline flushes real
    // image bytes into scrollback, fullscreen places them in the live viewport
    // through the placement layer. Must happen before raw mode: the probe uses
    // the same /dev/tty query path.
    // Kitty keyboard protocol (D86): a query/response round trip like the two
    // above, and for the same reason it belongs here — before raw mode, before
    // the event stream exists to compete for the answer.
    term::probe_keyboard_enhancement();

    let image_probe = gfx::detect_image_cap().await;
    let image_cap = image_probe.cap;
    if std::env::var_os("BINGO_DEBUG").is_some() {
        eprintln!(
            "[bingo] image_cap={image_cap:?} TERM={:?} TERM_PROGRAM={:?}",
            std::env::var("TERM").ok(),
            std::env::var("TERM_PROGRAM").ok(),
        );
    }

    let (events_tx, events_rx) = mpsc::unbounded_channel();
    let (asks_tx, asks_rx) = mpsc::unbounded_channel();
    // Subagents have no modal of their own; point them at this one so their permission
    // requests reach the user instead of being auto-denied.
    session
        .agents
        .attach_ask(crate::ui::modal_ask(asks_tx.clone()));
    let theme_setting = ThemeSetting::parse(session.settings.theme.as_deref());
    let mut chat = Chat::new(
        session.clone(),
        events_tx,
        events_rx,
        asks_tx,
        asks_rx,
        Theme::for_terminal(theme_setting, detected_background),
        theme_setting,
        detected_background,
    );
    chat.image_cap = image_cap;
    // Attention channel (D79). The environment is read here, once, and handed
    // down resolved: `notify` decides nothing about the terminal on its own, so
    // its tests do not depend on the shell that launched them.
    let notifier = notify::Notifier::new(
        notify::NotifyChannel::parse(session.settings.notifications.as_deref()),
        &notify::TerminalEnv::from_env(),
    );
    if notifier.enabled() {
        TUI_TITLE.store(true, Ordering::SeqCst);
    }
    chat.set_notifier(notifier);
    // Inside tmux, a failed passthrough probe (outer terminal lacking kitty
    // support, passthrough off, or an unfocused pane) yields a one-time hint
    // explaining why images stay `#[image]` placeholders.
    if let Some(warning) = image_probe.warning {
        chat.push_warning(warning);
    }
    // Startup notes (invalid provider fallback, transcript failures): stderr
    // is wiped by the alternate screen, so they land in the info tier —
    // visible until the first input.
    for note in startup_notes {
        chat.push_startup_note(note);
    }

    install_panic_hook();
    // Even on setup failure the terminal is handed back (it would otherwise be
    // left half-configured).
    if let Err(e) = setup_terminal(fullscreen) {
        release_terminal(restore_terminal);
        return Err(e.into());
    }

    // Even if the host fails to construct, teardown still runs (the
    // reverse-order teardown below applies to both paths).
    let result: Result<(), Box<dyn std::error::Error>> = if fullscreen {
        match Terminal::new(CrosstermBackend::new(stdout())) {
            Ok(terminal) => app::run_fullscreen(chat, expand_rx, terminal).await,
            Err(e) => Err(e.into()),
        }
    } else {
        // The cursor is parked on the shell prompt line right now: the driver
        // uses it as the viewport origin.
        match term::InlineTerm::stdout() {
            Ok(host) => app::run_inline(chat, expand_rx, host).await,
            Err(e) => Err(e.into()),
        }
    };

    release_terminal(restore_terminal);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// The safety net is a latch, not a counter: the first restore wins and
    /// every later one is a no-op, so a panic during teardown cannot re-emit the
    /// sequences over a terminal that already belongs to the shell. One test
    /// because the flags are process-global — two would race each other.
    ///
    /// It exercises what the hook does, not `install_panic_hook` itself: a hook
    /// installed by a test outlives that test and would sit under every later
    /// panic in the suite, including the one `a_lost_turn_reports_itself…`
    /// raises on purpose. `Once` is what makes the installation single, and the
    /// installed closure is one call to `release_terminal`.
    #[test]
    fn the_terminal_is_claimed_and_released_exactly_once() {
        let restored: Cell<Option<bool>> = Cell::new(None);
        let record = |fullscreen: bool| restored.set(Some(fullscreen));

        assert!(
            !TUI_ACTIVE.load(Ordering::SeqCst),
            "no host has claimed the terminal"
        );
        assert!(
            !release_terminal(record),
            "nothing to restore before a host claims the terminal"
        );
        assert_eq!(restored.get(), None);

        claim_terminal(false);
        assert!(TUI_ACTIVE.load(Ordering::SeqCst));
        assert!(release_terminal(record), "the claim is honoured once");
        assert_eq!(restored.get(), Some(false), "the inline teardown");
        assert!(!TUI_ACTIVE.load(Ordering::SeqCst));
        restored.set(None);
        assert!(!release_terminal(record), "and not a second time");
        assert_eq!(restored.get(), None);

        // The fullscreen host owes a longer teardown; the flag says which.
        claim_terminal(true);
        assert!(release_terminal(record));
        assert_eq!(restored.get(), Some(true));

        // A panic in a spawned task must not take the screen away from a session
        // that survives it: `supervise_turn` turns exactly that into the
        // recoverable `TURN_LOST` state, and the terminal has to still be there
        // when it does.
        restored.set(None);
        std::thread::spawn(|| claim_terminal(true))
            .join()
            .expect("claiming thread");
        assert!(TUI_ACTIVE.load(Ordering::SeqCst), "the session is live");
        assert!(
            !DRIVES_TERMINAL.get(),
            "this thread does not drive the host"
        );
        assert!(
            !release_terminal(record),
            "so its panic keeps the screen as it is"
        );
        assert_eq!(restored.get(), None);
        assert!(TUI_ACTIVE.load(Ordering::SeqCst), "and the claim stands");

        TUI_ACTIVE.store(false, Ordering::SeqCst);
    }
}

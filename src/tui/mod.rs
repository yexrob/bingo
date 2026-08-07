//! Terminal front end.
//!
//! - [`chat`] is the state machine and the document builder (`build_rows`).
//! - [`app`] is the event loop and the frame assembly.
//! - [`view`] converts document rows to ratatui text; [`term`] is the only
//!   module that writes to the terminal.
//!
//! The renderer-agnostic contract (`UiEvent`, the dialog types, `tui_hooks`)
//! lives in [`crate::ui`].

pub mod activities;
mod app;
pub mod chat;
mod entity;
pub mod gfx;
pub mod history;
pub mod input;
pub mod keys;
pub mod line;
pub mod markdown;
pub mod math;
pub(crate) mod term;
#[cfg(test)]
mod test_util;
pub mod theme;
mod view;

use std::io::stdout;
use std::sync::Arc;

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

/// Start a TUI session. `fullscreen=false` (default): inline mode — finalized
/// content goes into the terminal scrollback and the viewport only paints the
/// live tail; `fullscreen=true`: fullscreen canvas (in-app scrolling + mouse
/// interaction).
pub async fn run_tui_session(
    session: Arc<Session>,
    expand_rx: tokio::sync::watch::Receiver<bool>,
    fullscreen: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Query the terminal background colour once before raw mode, for the auto
    // theme resolution (the probe itself toggles raw mode temporarily and
    // reads /dev/tty directly).
    let detected_background = Theme::detect_system_theme().await;

    // Fullscreen's per-frame diff repaint cannot reliably carry kitty images,
    // so real image display is only enabled in inline mode (finalized rows
    // land in scrollback once). This must also happen before raw mode: the
    // probe uses the same /dev/tty query path.
    let image_probe = if fullscreen {
        gfx::ImageProbe::default()
    } else {
        gfx::detect_image_cap().await
    };
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
    let mut chat = Chat::new(
        session.clone(),
        events_tx,
        events_rx,
        asks_tx,
        asks_rx,
        Theme::for_terminal(
            ThemeSetting::parse(session.settings.theme.as_deref()),
            detected_background,
        ),
        detected_background,
    );
    chat.image_cap = image_cap;
    // Inside tmux, a failed passthrough probe (outer terminal lacking kitty
    // support, passthrough off, or an unfocused pane) yields a one-time hint
    // explaining why images stay `#[image]` placeholders.
    if let Some(warning) = image_probe.warning {
        chat.push_warning(warning);
    }

    enable_raw_mode()?;
    let mut out = stdout();
    let setup = if fullscreen {
        execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        )
    } else {
        execute!(out, EnableBracketedPaste)
    };
    // Even on setup failure, raw mode is reverted (the terminal would
    // otherwise be left half-configured).
    if let Err(e) = setup {
        let _ = disable_raw_mode();
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

    // Tear down in reverse order, best-effort at each step: a failure halfway
    // must not leave the terminal in raw mode.
    let mut out = stdout();
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
    result
}

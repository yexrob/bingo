//! The terminal this surface borrows and gives back.
//!
//! Everything it takes — raw mode, the alternate screen, bracketed paste, the
//! kitty disambiguation flag, the window title — is given back on every exit
//! path, including a panic: [`restore`] is a fixed sequence of escape codes
//! that allocates nothing, and the panic hook runs it before the default hook
//! prints the message onto a screen a person can read.
//!
//! Only `DISAMBIGUATE_ESCAPE_CODES` is pushed. It is what makes shift+enter
//! arrive as its own key instead of as a bare `\r`; the other flags would
//! change what every existing binding sees.

use std::io::{self, Stdout, Write};
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::clock::Now;
use crate::tree::Tree;
use crate::ui::Ui;
use crate::view;

/// Push the window title onto the terminal's stack, and pop it back at exit,
/// so the shell's own title survives the session.
const SAVE_TITLE: &[u8] = b"\x1b[22;2t";
const RESTORE_TITLE: &[u8] = b"\x1b[23;2t";

/// Whether the enhancement flags are pushed right now, so [`restore`] can be
/// called blind from every teardown path and pop exactly once.
static PUSHED: AtomicBool = AtomicBool::new(false);
/// Whether the mouse is ours. Capturing it takes the terminal's own selection
/// away, so `BINGO_MOUSE=off` gives that back and costs only the wheel.
static MOUSE: AtomicBool = AtomicBool::new(false);
/// Whether the terminal is ours to give back.
static ENTERED: AtomicBool = AtomicBool::new(false);
static HOOK: Once = Once::new();

/// What the loop needs from a screen, so a test can be the screen.
pub(crate) trait Screen: Send {
    fn draw(&mut self, tree: &Tree, ui: &Ui, now: Now) -> io::Result<()>;

    /// Out-of-band bytes: they paint no cell, so they go between frames.
    fn title(&mut self, text: &str) -> io::Result<()>;

    fn bell(&mut self) -> io::Result<()>;

    /// Hand the terminal a selection for its own clipboard.
    fn copy(&mut self, bytes: &[u8]) -> io::Result<()>;
}

pub struct Tui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// The last title written; an unchanged one costs no bytes.
    title: Option<String>,
}

impl Tui {
    pub fn enter() -> io::Result<Self> {
        let mut out = io::stdout();
        out.write_all(SAVE_TITLE)?;
        enable_raw_mode()?;
        ENTERED.store(true, Ordering::SeqCst);
        install_hook();
        crossterm::execute!(out, EnterAlternateScreen, EnableBracketedPaste)?;
        take_mouse(&mut out);
        push_enhancement(&mut out);
        let terminal = Terminal::with_options(
            CrosstermBackend::new(io::stdout()),
            TerminalOptions {
                viewport: Viewport::Fullscreen,
            },
        )?;
        Ok(Self {
            terminal,
            title: None,
        })
    }

    pub fn leave(&mut self) -> io::Result<()> {
        restore();
        Ok(())
    }
}

impl Screen for Tui {
    fn draw(&mut self, tree: &Tree, ui: &Ui, now: Now) -> io::Result<()> {
        self.terminal
            .draw(|frame| view::draw(tree, ui, frame, now))?;
        Ok(())
    }

    fn title(&mut self, text: &str) -> io::Result<()> {
        if self.title.as_deref() == Some(text) {
            return Ok(());
        }
        self.title = Some(text.to_string());
        out_of_band(&osc_title(text))
    }

    fn bell(&mut self) -> io::Result<()> {
        out_of_band(crate::theme::BELL)
    }

    fn copy(&mut self, bytes: &[u8]) -> io::Result<()> {
        out_of_band(bytes)
    }
}

/// `OSC 2 ; text BEL`. A stray control byte in a path would end the sequence
/// early and spill the rest onto the screen as text.
fn osc_title(text: &str) -> Vec<u8> {
    let clean: String = text.chars().filter(|c| !c.is_control()).collect();
    let mut out = b"\x1b]2;".to_vec();
    out.extend_from_slice(clean.as_bytes());
    out.push(0x07);
    out
}

fn out_of_band(bytes: &[u8]) -> io::Result<()> {
    let mut out = io::stdout();
    out.write_all(bytes)?;
    out.flush()
}

fn install_hook() {
    HOOK.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore();
            default(info);
        }));
    });
}

/// The wheel, the drag and the click, unless a person asked for the
/// terminal's own selection back.
fn take_mouse(out: &mut Stdout) {
    if !wanted() {
        return;
    }
    if crossterm::execute!(out, EnableMouseCapture).is_ok() {
        MOUSE.store(true, Ordering::SeqCst);
    }
}

/// Whether this run takes the mouse at all.
pub fn wanted() -> bool {
    std::env::var("BINGO_MOUSE")
        .map(|v| v != "off")
        .unwrap_or(true)
}

fn push_enhancement(out: &mut Stdout) {
    if !supports_keyboard_enhancement().unwrap_or(false) {
        return;
    }
    if crossterm::execute!(
        out,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )
    .is_ok()
    {
        PUSHED.store(true, Ordering::SeqCst);
    }
}

/// Give the terminal back. Safe to call twice and from the panic hook.
pub fn restore() {
    if !ENTERED.swap(false, Ordering::SeqCst) {
        return;
    }
    let mut out = io::stdout();
    if PUSHED.swap(false, Ordering::SeqCst) {
        let _ = crossterm::execute!(out, PopKeyboardEnhancementFlags);
    }
    if MOUSE.swap(false, Ordering::SeqCst) {
        let _ = crossterm::execute!(out, DisableMouseCapture);
    }
    let _ = crossterm::execute!(
        out,
        DisableBracketedPaste,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = disable_raw_mode();
    let _ = out.write_all(RESTORE_TITLE);
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_title_is_one_osc_two_sequence_with_the_control_bytes_stripped() {
        assert_eq!(osc_title("bingo — p"), b"\x1b]2;bingo \xe2\x80\x94 p\x07");
        assert_eq!(
            osc_title("a\x1b]0;evil\x07b"),
            b"\x1b]2;a]0;evilb\x07",
            "a stray escape would end the sequence early"
        );
    }

    #[test]
    fn the_title_stack_codes_are_the_xterm_ones() {
        assert_eq!(SAVE_TITLE, b"\x1b[22;2t");
        assert_eq!(RESTORE_TITLE, b"\x1b[23;2t");
    }
}

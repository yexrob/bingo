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
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    supports_keyboard_enhancement,
};
use ratatui::backend::CrosstermBackend;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::clock::Now;
use crate::graphics::Transport;
use crate::graphics::tmux;
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

    /// Put a message where the desktop can see it, for a window nobody is
    /// looking at.
    fn notify(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Hand the terminal a selection for its own clipboard.
    fn copy(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Hand the terminal the pictures the frame just drew placeholders for
    /// (design §5). Out of band, as the title and the clipboard are: they
    /// paint no cell of their own — the cells are already on the screen, and
    /// these are what the terminal draws into them.
    fn place(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// How many rows it has, for the screenful printed back on the way out.
    fn rows(&self) -> u16;
}

pub struct Tui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// The last title written; an unchanged one costs no bytes.
    title: Option<String>,
}

impl Tui {
    pub fn enter() -> io::Result<Self> {
        // The background and the graphics are read before the terminal is
        // ours: each probe wants a plain terminal to write its escape to and
        // read the answer from, and they run one after the other so neither
        // reads the other's answer.
        crate::theme::detect();
        crate::graphics::detect();
        let mut out = io::stdout();
        out.write_all(SAVE_TITLE)?;
        enable_raw_mode()?;
        ENTERED.store(true, Ordering::SeqCst);
        install_hook();
        crossterm::execute!(
            out,
            EnterAlternateScreen,
            EnableBracketedPaste,
            // Whether the window is looked at is what decides between a bell
            // and a notification (§6).
            EnableFocusChange
        )?;
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

    fn notify(&mut self, bytes: &[u8]) -> io::Result<()> {
        out_of_band(bytes)
    }

    fn copy(&mut self, bytes: &[u8]) -> io::Result<()> {
        out_of_band(bytes)
    }

    fn place(&mut self, bytes: &[u8]) -> io::Result<()> {
        out_of_band(bytes)
    }

    fn rows(&self) -> u16 {
        self.terminal.size().map(|size| size.height).unwrap_or(0)
    }
}

// ---- what a window nobody is looking at says (design §6) ----------------

/// What there is to say to the desktop. The words are §6's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notification {
    /// Something wants a person: a card is open.
    NeedsYou,
    /// The turn a person started has finished.
    Done,
}

impl Notification {
    fn body(self) -> &'static str {
        match self {
            Notification::NeedsYou => "needs you",
            Notification::Done => "done",
        }
    }
}

/// Which escape a terminal understands. `777` is the one most of them take;
/// the two that grew their own take `9`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Osc777,
    Osc9,
}

/// The bytes for one notification, as this terminal wants them.
pub fn notification(what: Notification) -> Vec<u8> {
    let term_program = std::env::var("TERM_PROGRAM").ok();
    let term = std::env::var("TERM").ok();
    tmux::wrapped(
        message(what, dialect(term_program.as_deref())),
        envelope(multiplexed(
            term.as_deref(),
            std::env::var_os("TMUX").is_some(),
        )),
    )
}

/// What a notification travels in. Every multiplexer gets tmux's envelope,
/// screen included: a notification is worth one try through the only
/// passthrough this surface writes, and there is nothing to fall back to —
/// unlike a picture, whose cells would be tofu if the try failed.
fn envelope(multiplexed: bool) -> Transport {
    match multiplexed {
        true => Transport::Tmux,
        false => Transport::Bare,
    }
}

/// iTerm2 and Terminal.app answer to `OSC 9`; everything else that notifies at
/// all — kitty, foot, WezTerm, Ghostty, rxvt — answers to `OSC 777`, and a
/// terminal that answers to neither ignores both.
pub fn dialect(term_program: Option<&str>) -> Dialect {
    match term_program {
        Some("iTerm.app" | "Apple_Terminal") => Dialect::Osc9,
        _ => Dialect::Osc777,
    }
}

/// Whether a multiplexer is between this surface and the terminal, and so
/// whether the sequence has to be passed through it.
pub fn multiplexed(term: Option<&str>, tmux: bool) -> bool {
    tmux || term.is_some_and(|term| term.starts_with("tmux") || term.starts_with("screen"))
}

fn message(what: Notification, dialect: Dialect) -> Vec<u8> {
    let body = what.body();
    match dialect {
        Dialect::Osc777 => format!("\x1b]777;notify;bingo;{body}\x07").into_bytes(),
        Dialect::Osc9 => format!("\x1b]9;bingo · {body}\x07").into_bytes(),
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
        DisableFocusChange,
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

    #[test]
    fn a_notification_is_one_osc_sequence_in_the_dialect_the_terminal_takes() {
        assert_eq!(
            message(Notification::NeedsYou, Dialect::Osc777),
            "\x1b]777;notify;bingo;needs you\x07".as_bytes(),
        );
        assert_eq!(
            message(Notification::Done, Dialect::Osc9),
            "\x1b]9;bingo · done\x07".as_bytes(),
        );
    }

    #[test]
    fn the_two_terminals_with_their_own_escape_get_it_and_the_rest_get_777() {
        assert_eq!(dialect(Some("iTerm.app")), Dialect::Osc9);
        assert_eq!(dialect(Some("Apple_Terminal")), Dialect::Osc9);
        assert_eq!(dialect(Some("WezTerm")), Dialect::Osc777);
        assert_eq!(dialect(Some("ghostty")), Dialect::Osc777);
        assert_eq!(dialect(None), Dialect::Osc777);
    }

    #[test]
    fn a_multiplexer_is_passed_through_with_the_escape_doubled() {
        assert!(multiplexed(Some("tmux-256color"), false));
        assert!(multiplexed(Some("screen"), false));
        assert!(multiplexed(Some("xterm-256color"), true), "TMUX is set");
        assert!(!multiplexed(Some("xterm-256color"), false));

        let bare = message(Notification::NeedsYou, Dialect::Osc777);
        assert_eq!(
            tmux::wrapped(bare.clone(), envelope(true)),
            [
                b"\x1bPtmux;".to_vec(),
                b"\x1b\x1b]777;notify;bingo;needs you\x07".to_vec(),
                b"\x1b\\".to_vec(),
            ]
            .concat(),
        );
        assert_eq!(
            tmux::wrapped(bare.clone(), envelope(false)),
            bare,
            "and nothing when not"
        );
    }
}

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

use std::io::{self, IsTerminal, Stdout, Write};
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
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::graphics::Transport;
use crate::graphics::tmux;
use crate::layout::{self, Layout};
use crate::render::{Detection, Render};

#[cfg(any(windows, test))]
mod image_paste;
mod probe;
#[cfg(test)]
mod resizing;

/// Push the window title onto the terminal's stack, and pop it back at exit,
/// so the shell's own title survives the session.
const SAVE_TITLE: &[u8] = b"\x1b[22;2t";
const RESTORE_TITLE: &[u8] = b"\x1b[23;2t";

/// DEC mode 2026, which every frame is written between: the terminal holds
/// what arrives and paints the whole of it at once, so a draw is composited
/// rather than torn (design §6 — smoothness is the absence of tearing, and
/// every cue is read through it). A terminal that has never heard of the mode
/// drops an unknown private mode without a mark on the screen, so nothing is
/// asked and nothing is probed.
const BEGIN_FRAME: &[u8] = b"\x1b[?2026h";
const END_FRAME: &[u8] = b"\x1b[?2026l";

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
    fn detection(&mut self) -> Detection {
        Detection::default()
    }
    fn draw(&mut self, layout: &Layout, render: &Render) -> io::Result<()>;

    /// Out-of-band bytes: they paint no cell, so they go between frames.
    fn title(&mut self, text: &str) -> io::Result<()>;

    fn bell(&mut self) -> io::Result<()>;

    /// Put a message where the desktop can see it, for a window nobody is
    /// looking at.
    fn notify(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Hand the terminal a selection for its own clipboard.
    fn copy(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Ask the terminal something. Out of band like the rest: a question
    /// paints no cell, and the answer comes back through the key stream where
    /// [`crate::late`] hears it whole ([`crate::run::look`], M71).
    fn ask(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Hand the terminal the pictures the frame just drew placeholders for
    /// (design §5). Out of band, as the title and the clipboard are: they
    /// paint no cell of their own — the cells are already on the screen, and
    /// these are what the terminal draws into them.
    fn place(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// How big it is, in columns and rows: what the screenful printed back on
    /// the way out is cut to, and what says whether there is room for the
    /// opening to play (design §11).
    fn size(&self) -> (u16, u16);
}

pub struct Tui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    /// The last title written; an unchanged one costs no bytes.
    title: Option<String>,
    /// Whether this run brackets its frames, settled once from the
    /// environment ([`synchronizes`]).
    synchronized: bool,
    detection: Detection,
}

impl Tui {
    pub fn enter() -> io::Result<Self> {
        // The background and the graphics are read before the terminal is
        // ours: each probe wants a plain terminal to write its escape to and
        // read the answer from, and they run one after the other so neither
        // reads the other's answer.
        let detection = detect();
        let mut out = io::stdout();
        enable_raw_mode()?;
        ENTERED.store(true, Ordering::SeqCst);
        if out.is_terminal() {
            out.write_all(SAVE_TITLE)?;
        }
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
        #[cfg(windows)]
        image_paste::start(|marker| {
            out_of_band(&osc_title(&image_paste::title("bingo", Some(marker))))
        });
        Ok(Self {
            terminal,
            detection,
            title: None,
            synchronized: synchronizes(
                std::env::var("TERM").ok().as_deref(),
                std::env::var_os("TMUX").is_some(),
            ),
        })
    }

    pub fn leave(&mut self) -> io::Result<()> {
        restore();
        Ok(())
    }
}

impl Screen for Tui {
    fn detection(&mut self) -> Detection {
        std::mem::take(&mut self.detection)
    }
    fn draw(&mut self, layout: &Layout, render: &Render) -> io::Result<()> {
        if !self.synchronized {
            paint(&mut self.terminal, layout, render)?;
            return Ok(());
        }
        // The closing half is written whatever the frame did: a terminal left
        // holding an update that never ends shows nothing at all.
        out_of_band(BEGIN_FRAME)?;
        let drawn = paint(&mut self.terminal, layout, render);
        let ended = out_of_band(END_FRAME);
        drawn?;
        ended
    }

    fn title(&mut self, text: &str) -> io::Result<()> {
        if !io::stdout().is_terminal() || !ENTERED.load(Ordering::SeqCst) {
            return Ok(());
        }
        #[cfg(windows)]
        let text = image_paste::marked_title(text);
        #[cfg(not(windows))]
        let text = text.to_owned();
        if self.title.as_deref() == Some(text.as_str()) {
            return Ok(());
        }
        out_of_band(&osc_title(&text))?;
        self.title = Some(text);
        Ok(())
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

    fn ask(&mut self, bytes: &[u8]) -> io::Result<()> {
        out_of_band(bytes)
    }

    fn place(&mut self, bytes: &[u8]) -> io::Result<()> {
        out_of_band(bytes)
    }

    fn size(&self) -> (u16, u16) {
        self.terminal
            .size()
            .map(|size| (size.width, size.height))
            .unwrap_or((0, 0))
    }
}

fn paint<B: Backend>(
    terminal: &mut Terminal<B>,
    layout: &Layout,
    render: &Render,
) -> Result<(), B::Error> {
    terminal.draw(|frame| layout::paint(layout, frame, render))?;
    Ok(())
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

/// Whether this run brackets its frames in mode 2026.
///
/// A terminal that has never heard of the mode drops it, so it is written
/// blind — but a multiplexer is a terminal of its own and *consumes* it,
/// which is the one case where writing it can cost something. tmux acts on
/// the mode only from 3.7 (2026-06-26, `CHANGES`: "Add support for
/// applications to use synchronized output mode (DECSET 2026) to prevent
/// screen tearing during rapid updates"), and 3.7/3.7a act on it wrongly —
/// they leave the frame unpainted until something unrelated redraws the pane
/// or a one-second timeout fires (3.7b: "Fix so that the end of a
/// synchronized update again triggers a redraw"). tmux already wraps what it
/// repaints in the outer terminal's own synchronized update (its `Sync`
/// capability has been DECSET 2026 since 3.4), so there is nothing here for
/// it to add and a version to be wrong about. Under one, the frame goes
/// bare — as it did before any of this.
pub fn synchronizes(term: Option<&str>, tmux: bool) -> bool {
    !multiplexed(term, tmux)
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
    #[cfg(windows)]
    image_paste::stop();
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
    if out.is_terminal() {
        let _ = out.write_all(RESTORE_TITLE);
    }
    let _ = out.flush();
}

/// Read environment and probe I/O once; the resulting inputs belong to the run.
fn detect() -> Detection {
    let ask = crate::theme::Ask {
        indexed: std::env::var("TERM").is_ok_and(|term| term.contains("256color")),
        no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
        truecolor: matches!(
            std::env::var("COLORTERM").ok().as_deref(),
            Some("truecolor" | "24bit")
        ),
        ascii: std::env::var_os("BINGO_ASCII").is_some_and(|v| v == "1"),
        look: crate::theme::look(std::env::var("BINGO_THEME").ok().as_deref()),
    };
    let palette = crate::theme::follows(ask).then(terminal_palette).flatten();
    Detection {
        ask,
        foreground: palette.as_ref().and_then(|p| p.foreground),
        ground: palette.as_ref().map(|p| p.background),
        heard: graphics_probe(),
        motion: crate::clock::moves(std::env::var("BINGO_MOTION").ok().as_deref()),
    }
}

struct TerminalPalette {
    foreground: Option<(u16, u16, u16)>,
    background: (u16, u16, u16),
}

fn terminal_palette() -> Option<TerminalPalette> {
    let mut options = terminal_colorsaurus::QueryOptions::default();
    options.timeout = crate::theme::PROBE;
    let rgb = |c: terminal_colorsaurus::Color| (c.r, c.g, c.b);
    let palette = terminal_colorsaurus::color_palette(options).map(|p| TerminalPalette {
        foreground: Some(rgb(p.foreground)),
        background: rgb(p.background),
    });
    probe_palette(palette, || {
        let mut options = terminal_colorsaurus::QueryOptions::default();
        options.timeout = crate::theme::PROBE;
        terminal_colorsaurus::background_color(options).map(rgb)
    })
}

fn probe_palette<E>(
    palette: Result<TerminalPalette, E>,
    background: impl FnOnce() -> Result<(u16, u16, u16), E>,
) -> Option<TerminalPalette> {
    match palette {
        Ok(palette) => Some(palette),
        Err(_) => background().ok().map(|background| TerminalPalette {
            foreground: None,
            background,
        }),
    }
}

fn graphics_probe() -> Option<crate::graphics::Heard> {
    if !crate::graphics::wanted(std::env::var("BINGO_GRAPHICS").ok().as_deref()) {
        return None;
    }
    let transport = tmux::transport(
        std::env::var("TERM").ok().as_deref(),
        std::env::var_os("TMUX").is_some(),
    )?;
    let passthrough = tmux::reach_hidden(tmux::passthrough(transport));
    Some(probe::measured(crate::graphics::Heard {
        probe: crate::graphics::probe::parse(&probe::exchange(transport, passthrough)),
        transport,
        passthrough,
        measured: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_probe_keeps_a_background_only_answer() {
        let bg = (1, 2, 3);
        let fg = (4, 5, 6);
        let both = probe_palette(
            Ok::<_, ()>(TerminalPalette {
                foreground: Some(fg),
                background: bg,
            }),
            || panic!("a complete palette needs no fallback"),
        )
        .unwrap();
        assert_eq!(both.foreground, Some((4, 5, 6)));
        assert_eq!(both.background, (1, 2, 3));
        let only_bg = probe_palette(Err(()), || Ok(bg)).unwrap();
        assert_eq!(only_bg.foreground, None);
        assert_eq!(only_bg.background, (1, 2, 3));
        assert!(probe_palette(Err(()), || Err(())).is_none());
    }

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

    /// The bracket is written as bytes, like every other out-of-band sequence
    /// in this file, so this pins it to the spelling crossterm itself uses:
    /// mode 2026 set and reset, and nothing wrapped around them.
    #[test]
    fn a_frame_is_bracketed_by_the_two_halves_of_mode_2026() {
        let mut begin = Vec::new();
        let mut end = Vec::new();
        crossterm::queue!(begin, crossterm::terminal::BeginSynchronizedUpdate)
            .expect("a sequence into a vector");
        crossterm::queue!(end, crossterm::terminal::EndSynchronizedUpdate)
            .expect("a sequence into a vector");
        assert_eq!(BEGIN_FRAME, begin);
        assert_eq!(END_FRAME, end);
        assert_eq!(BEGIN_FRAME, b"\x1b[?2026h");
        assert_eq!(END_FRAME, b"\x1b[?2026l");
    }

    /// The one listener the bracket is not written to is a multiplexer, which
    /// eats it rather than passing it on.
    #[test]
    fn a_frame_goes_bare_through_a_multiplexer_and_bracketed_everywhere_else() {
        assert!(synchronizes(Some("xterm-256color"), false));
        assert!(synchronizes(Some("xterm-ghostty"), false));
        assert!(synchronizes(None, false), "a terminal that says nothing");
        assert!(!synchronizes(Some("tmux-256color"), false));
        assert!(!synchronizes(Some("screen"), false));
        assert!(!synchronizes(Some("xterm-256color"), true), "TMUX is set");
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

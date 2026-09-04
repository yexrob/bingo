//! The look that follows the terminal (M71), through a real pty.
//!
//! The palette is the terminal's own ground, read before the first frame and
//! read again while the run lasts. Only a terminal can answer that question,
//! and only a terminal can change its answer under a running surface — so this
//! is where the whole path is driven: the question out, the answer back, and
//! the ink on the screen the other colour.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::*;

// ---- the look that follows the terminal (M71) ---------------------------

/// What this milestone's scenes read off the screen, beside the scenes
/// themselves: the harness proper knows nothing about palettes.
impl Terminal {
    /// The system's appearance has turned: from the next question on, this
    /// terminal says its ground is the light one (M71).
    fn turn_light(&self) {
        self.light.store(true, Ordering::SeqCst);
    }

    /// The colour the row carrying `needle` ends in, as the terminal at the
    /// other end has it. The end of that row is the answer's own last word, so
    /// what it is drawn in is the palette's plain text ink and not a bullet or
    /// a band.
    fn ink(&self, needle: &str) -> Option<(u8, u8, u8)> {
        let parser = self.parser.lock().unwrap();
        let screen = parser.screen();
        let row = (0..ROWS).find(|y| row_text(screen, *y).contains(needle))?;
        (0..COLS)
            .rev()
            .filter_map(|x| screen.cell(row, x))
            .filter(|cell| !cell.contents().trim().is_empty())
            .find_map(|cell| match cell.fgcolor() {
                vt100::Color::Rgb(red, green, blue) => Some((red, green, blue)),
                _ => None,
            })
    }

    /// Wait until that row is drawn in ink of the wanted kind: pale, which is
    /// what a dark ground is written on, or near-black, which is what a light
    /// one is.
    fn wait_ink(&self, needle: &str, pale: bool) {
        let deadline = Instant::now() + LIMIT;
        while Instant::now() < deadline {
            if self.ink(needle).is_some_and(|ink| bright(ink) == pale) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "timed out waiting for {needle:?} to be drawn in {} ink; it is {:?}",
            match pale {
                true => "pale",
                false => "near-black",
            },
            self.ink(needle)
        );
    }
}

/// One row of the terminal's screen, as text.
fn row_text(screen: &vt100::Screen, row: u16) -> String {
    (0..COLS)
        .filter_map(|x| screen.cell(row, x))
        .map(|cell| cell.contents().to_string())
        .collect()
}

/// Whether ink of this colour is the pale kind. The two palettes are a warm
/// off-white over a dark ground and a warm near-black over a light one
/// (`docs/design/tui.md` §4), so which side of the middle the ink falls on is
/// the whole of what a test has to know — and it stays true through any later
/// tuning of the eight.
fn bright((red, green, blue): (u8, u8, u8)) -> bool {
    u16::from(red) + u16::from(green) + u16::from(blue) > 3 * 128
}

/// A person comes back to the window, which is one of the two moments the run
/// asks the terminal what ground it has.
const FOCUS_GAINED: &[u8] = b"\x1b[I";

/// The look follows the terminal for as long as the run lasts. This terminal
/// says its ground is dark, so the answer is written in pale ink; then its
/// ground turns light under the running surface, and the question a person's
/// return to the window puts brings the other palette back with it.
#[test]
fn a_terminal_whose_ground_turns_light_is_followed_within_one_focus() {
    let mut terminal = Terminal::under(&[], SCRIPT, Answers::Da1Only, Ground::Answered);
    terminal.wait_for("? for shortcuts");
    terminal.send(b"say hello\r");
    terminal.wait_for("Hello from the pty.");
    terminal.wait_ink("Hello from the pty.", true);

    terminal.turn_light();
    terminal.send(FOCUS_GAINED);
    terminal.wait_ink("Hello from the pty.", false);
    assert!(
        counted(&terminal.written(), GROUND_QUERY) > 1,
        "the question was put again"
    );

    // And back: the same window, the same run, the ground dark again.
    terminal.light.store(false, Ordering::SeqCst);
    terminal.send(FOCUS_GAINED);
    terminal.wait_ink("Hello from the pty.", true);

    terminal.send(&[0x04]);
    terminal.leave();
}

/// A person who named a look is not asked about it — not at start, where the
/// probe would otherwise spend its milliseconds, and not on any focus after.
#[test]
fn a_named_look_is_never_asked_what_ground_the_terminal_has() {
    let mut terminal = Terminal::under(&[], SCRIPT, Answers::Da1Only, Ground::Named);
    terminal.wait_for("? for shortcuts");
    terminal.send(FOCUS_GAINED);
    terminal.send(b"say hello\r");
    terminal.wait_for("Hello from the pty.");
    assert_eq!(
        counted(&terminal.written(), GROUND_QUERY),
        0,
        "nothing was asked"
    );
    assert!(
        terminal.ink("Hello from the pty.").is_some_and(bright),
        "and the look a person named is the one it drew in"
    );
    terminal.send(&[0x04]);
    terminal.leave();
}

/// What crossterm 0.29 makes of a mode-2031 report, measured — because the
/// answer is what shuts that door (M71). `CSI ? 997 ; 1 n` is neither passed
/// on nor dropped: `parse_csi` answers `Ok(None)` for it, which its parser
/// reads as an unfinished sequence, so the report and every key struck after
/// it sit in the buffer until one of them makes a sequence it can call a DA1
/// reply — and the whole lot goes with it. A terminal that was asked to report
/// its scheme would cost a person their keyboard, so bingo never asks.
#[test]
fn a_theme_report_holds_crossterms_parser_and_swallows_what_follows() {
    let mut terminal = Terminal::open(&[]);
    terminal.wait_for("? for shortcuts");
    terminal.send(b"\x1b[?997;1n");
    std::thread::sleep(BETWEEN_KEYS);
    terminal.send(b"held");
    std::thread::sleep(BETWEEN_KEYS);
    // `c` is a final byte its parser has a rule for: the buffer parses as a
    // DA1 reply, which is crossterm's own to keep, and empties.
    terminal.send(b"c");
    std::thread::sleep(BETWEEN_KEYS);
    terminal.send(b"typed");
    terminal.wait_for("typed");
    let screen = terminal.screen();
    assert!(
        !screen.contains("held"),
        "everything between the report and the byte that ended it is gone:\n{screen}"
    );

    for _ in 0..b"typed".len() {
        terminal.send(&[0x7f]);
    }
    terminal.send(&[0x04]);
    terminal.leave();
}

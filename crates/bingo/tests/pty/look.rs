//! The look that follows the terminal (M71), through a real pty.
//!
//! The palette is the terminal's own ground, read before the first frame and
//! read again while the run lasts. Only a terminal can answer that question,
//! and only a terminal can change its answer under a running surface — so this
//! is where the whole path is driven: the question out, the answer back, and
//! the screen the other palette.
//!
//! What is read off the screen is the band a person's own line sits on, and not
//! the words: since M73 the ink is the terminal's own in every look, so prose
//! carries nothing for a swap to change and follows a scheme flip with no
//! question asked at all. That is asserted here too — through a real terminal,
//! which is the only place SGR 39 can be seen for what it is.

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

    /// The band a person's own line sits on, as the terminal at the other end
    /// has it: the row's own words and the ground under them. That line is the
    /// one thing on this screen the palette paints (design §4) — since M73 the
    /// ink is the terminal's own — so it is the row whose cells carry a ground
    /// at all.
    fn band(&self) -> Option<(String, (u8, u8, u8))> {
        let parser = self.parser.lock().unwrap();
        let screen = parser.screen();
        (0..ROWS).find_map(|y| match screen.cell(y, 0)?.bgcolor() {
            vt100::Color::Rgb(red, green, blue) => Some((row_text(screen, y), (red, green, blue))),
            _ => None,
        })
    }

    /// Wait until that band is drawn on the ground of the wanted palette: the
    /// dark one's raised tint, or the light one's.
    fn wait_band(&self, light: bool) {
        let deadline = Instant::now() + LIMIT;
        while Instant::now() < deadline {
            if self.band().is_some_and(|(_, tint)| bright(tint) == light) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "timed out waiting for the band on the {} tint; it is {:?}",
            match light {
                true => "light",
                false => "dark",
            },
            self.band()
        );
    }

    /// Whether every cell of the row carrying `needle` is written in the
    /// terminal's own foreground — SGR 39 and no colour of bingo's, which is
    /// what body text is drawn in since M73, in every look.
    fn own_ink(&self, needle: &str) -> bool {
        let parser = self.parser.lock().unwrap();
        let screen = parser.screen();
        let Some(row) = (0..ROWS).find(|y| row_text(screen, *y).contains(needle)) else {
            return false;
        };
        (0..COLS)
            .filter_map(|x| screen.cell(row, x))
            .filter(|cell| !cell.contents().trim().is_empty())
            .all(|cell| cell.fgcolor() == vt100::Color::Default)
    }
}

/// One row of the terminal's screen, as text.
fn row_text(screen: &vt100::Screen, row: u16) -> String {
    (0..COLS)
        .filter_map(|x| screen.cell(row, x))
        .map(|cell| cell.contents().to_string())
        .collect()
}

/// Whether a colour of this weight is the light palette's. Its raised tint is
/// one step down from paper and the dark palette's is one step up from night
/// (`docs/design/tui.md` §4), so which side of the middle a tint falls on is
/// the whole of what a test has to know — and it stays true through any later
/// tuning of either.
fn bright((red, green, blue): (u8, u8, u8)) -> bool {
    u16::from(red) + u16::from(green) + u16::from(blue) > 3 * 128
}

/// A person comes back to the window, which is one of the two moments the run
/// asks the terminal what ground it has.
const FOCUS_GAINED: &[u8] = b"\x1b[I";

/// The look follows the terminal for as long as the run lasts. This terminal
/// says its ground is dark, so a person's line is banded on the dark tint;
/// then its ground turns light under the running surface, and the question a
/// person's return to the window puts brings the other palette back with it.
///
/// The answer's own words are the terminal's ink throughout, in both palettes
/// and at every step between them (M73) — nothing of bingo's is spent on prose,
/// which is why a scheme flip reaches the words before the question is even
/// asked.
#[test]
fn a_terminal_whose_ground_turns_light_is_followed_within_one_focus() {
    let mut terminal = Terminal::under(&[], SCRIPT, Answers::Da1Only, Ground::Answered);
    terminal.wait_for("? for shortcuts");
    terminal.send(b"say hello\r");
    terminal.wait_for("Hello from the pty.");
    terminal.wait_band(false);
    let (line, _) = terminal.band().expect("a person's own line is banded");
    assert!(
        line.contains("hello"),
        "and that band is their line: {line:?}"
    );
    assert!(
        terminal.own_ink("Hello from the pty."),
        "the answer is written in the terminal's own foreground"
    );

    terminal.turn_light();
    terminal.send(FOCUS_GAINED);
    terminal.wait_band(true);
    assert!(
        counted(&terminal.written(), GROUND_QUERY) > 1,
        "the question was put again"
    );
    assert!(
        terminal.own_ink("Hello from the pty."),
        "and the words never changed hands: the terminal remapped them itself"
    );

    // And back: the same window, the same run, the ground dark again.
    terminal.light.store(false, Ordering::SeqCst);
    terminal.send(FOCUS_GAINED);
    terminal.wait_band(false);

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
        terminal.band().is_some_and(|(_, tint)| !bright(tint)),
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

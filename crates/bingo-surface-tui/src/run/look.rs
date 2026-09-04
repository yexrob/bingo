//! The look, following the terminal for as long as the run lasts (M71).
//!
//! Which of the two palettes bingo draws in is the terminal's own to say, and
//! a terminal that follows the system's appearance says something different an
//! hour later. [`crate::theme`] holds that answer in a slot; this is when the
//! slot is written — the question at the moments a change is likely, and the
//! answer whenever one comes back.
//!
//! Two doors lead in, and one of them is nailed shut.
//!
//! **Mode 2031**, where the terminal says so itself, is the shut one. A
//! terminal that has been told `CSI ? 2031 h` reports `CSI ? 997 ; 1 n` (dark)
//! or `CSI ? 997 ; 2 n` (light) whenever its scheme changes, and crossterm
//! 0.29 does not pass that report on. Worse than dropping it, it *holds* it:
//! `parse_csi` sends every `CSI ?` sequence whose final byte is neither `u`
//! nor `c` to `Ok(None)` (`event/sys/unix/parse.rs`), which its `Parser`
//! reads as "not a whole sequence yet" and keeps in the buffer — where every
//! byte a person types afterwards is appended to a sequence that can never
//! parse, until one of them happens to be a `c` or a `u` and the whole lot is
//! thrown away as an error. A report would cost a person their keyboard, so
//! bingo never sets the mode (`crates/bingo/tests/pty/look.rs` measures it).
//!
//! **`OSC 11`**, where bingo asks, is the open one — the same question the
//! probe asks before the first frame ([`theme::QUESTION`]), put again on
//! [`Term::FocusGained`] and on a slow clock while the run is idle. The answer
//! lands in crossterm's key stream, where [`crate::late`] already hears an
//! `OSC` reply whole and hands it here rather than to the bindings.
//!
//! These are functions over the run rather than more of its methods: `Run`'s
//! own `impl` is spread as far as it may be (`scripts/check_discipline.sh`
//! §5).
//!
//! [`Term::FocusGained`]: crossterm::event::Event::FocusGained

use std::io;
use std::time::{Duration, Instant};

use bingo_sdk::SessionState;

use crate::terminal::Screen;
use crate::theme;

use super::Run;

/// How long an idle run leaves the terminal alone between questions. Long
/// enough that the bytes are nothing at all; short enough that a person whose
/// system turned light under a window they never left sees bingo follow while
/// they are still looking at it.
const RE_ASK: Duration = Duration::from_secs(30);

/// What the terminal is owed, held until the frame boundary where every other
/// out-of-band byte is written.
#[derive(Debug)]
pub(crate) struct Owed {
    /// A question nobody has put yet.
    question: bool,
    /// When the last one went out, which is what the idle clock counts from.
    /// The probe asked one before the first frame, so the run starts having
    /// just asked.
    asked: Instant,
}

impl Default for Owed {
    fn default() -> Self {
        Self {
            question: false,
            asked: Instant::now(),
        }
    }
}

impl Owed {
    /// When the terminal is next worth asking.
    pub(super) fn due(&self) -> Instant {
        self.asked + RE_ASK
    }
}

/// Whether asking again is worth the bytes: the look is the terminal's to say
/// at all, nothing is owed already, and no turn is running — a person who is
/// watching a turn is not the one who just changed their system's appearance,
/// and a reply that lands between two keystrokes is rarer while nobody types.
pub(super) fn asking(run: &Run) -> bool {
    theme::follows() && !run.look.question && !run.session.tree.sessions().any(SessionState::busy)
}

/// The clock the loop waits on: a wake when the terminal is worth asking
/// again, and nothing at all while it is not.
pub(super) async fn wait(asking: bool, next: Instant) {
    match asking {
        true => tokio::time::sleep_until(next.into()).await,
        false => std::future::pending().await,
    }
}

/// Owe the terminal the question. A person who named a look with
/// `BINGO_THEME`, and a terminal with no palette to follow, are never asked.
pub(super) fn ask(run: &mut Run) {
    run.look.question |= theme::follows();
}

/// A reply the terminal sent, read: an answer to that question swaps the look,
/// and every frame after it is drawn in the other palette. Anything else is
/// another probe's reply and is left alone.
///
/// A swap that changes nothing says nothing; a swap that changes the look
/// needs no announcing either, because the screen itself is the message — and
/// the frame that carries it is the one the loop paints on the way out of this
/// pass.
pub(super) fn answered(reply: &[u8]) {
    if let Some(light) = theme::answered(reply) {
        theme::swap(light);
    }
}

/// Put the question, where one is owed. Between frames, with the title and the
/// clipboard: it paints no cell.
pub(super) fn pay(run: &mut Run, screen: &mut dyn Screen) -> io::Result<()> {
    if !std::mem::take(&mut run.look.question) {
        return Ok(());
    }
    run.look.asked = Instant::now();
    screen.ask(theme::QUESTION)
}

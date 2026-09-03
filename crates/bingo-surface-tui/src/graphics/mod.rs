//! What this terminal can draw beside the words.
//!
//! Design §5's image row: a picture is pixels where the terminal speaks the
//! kitty graphics protocol, and `[image: …]` everywhere else. Which of the two
//! is *asked*, once, at start-up — never assumed from `TERM`, which says what
//! a terminal calls itself and not what it can do — the way the background
//! colour is asked (`theme.rs`).
//!
//! - [`probe`] is the question and the reading of the answer, both pure.
//! - [`picture`] is one picture a frame drew: where it came from and how many
//!   cells it took.
//! - [`kitty`] is the protocol as bytes.
//! - [`decoded`] keeps the pixels, so a picture is decoded once.
//! - [`stored`] keeps what the terminal is holding, and says what to send.

pub mod decoded;
pub mod kitty;
pub mod picture;
pub mod probe;
pub mod stored;

pub use decoded::Decoded;
pub use picture::Picture;
pub use probe::Probe;
pub use stored::Stored;

/// One cell of this terminal, in pixels. What turns a picture's size into a
/// number of cells, and the reason a terminal that will not say draws no
/// pictures: a guess of 8×16 would draw every picture the wrong shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub width: u16,
    pub height: u16,
}

/// How this run draws a picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Graphics {
    /// The chip of design §5, which every terminal can draw.
    #[default]
    Off,
    Kitty {
        cell: Cell,
    },
}

impl From<Probe> for Graphics {
    /// Both halves or neither: a terminal that speaks the protocol but will
    /// not say how big a cell is cannot be drawn into.
    fn from(probe: Probe) -> Self {
        match (probe.kitty, probe.cell) {
            (true, Some(cell)) => Graphics::Kitty { cell },
            _ => Graphics::Off,
        }
    }
}

/// Whether this run draws pictures at all: `BINGO_GRAPHICS=off` says no, as
/// `BINGO_MOTION=off` and `BINGO_ASCII=1` say their own noes.
pub fn wanted(setting: Option<&str>) -> bool {
    setting != Some("off")
}

/// What this run draws pictures with, settled by [`detect`] before the first
/// frame. A run that never asked draws none: the question is a write and a
/// read, which is not something a draw may do.
#[cfg(not(test))]
pub fn chosen() -> Graphics {
    CHOSEN.get().copied().unwrap_or_default()
}

#[cfg(not(test))]
static CHOSEN: std::sync::OnceLock<Graphics> = std::sync::OnceLock::new();

/// Ask the terminal, once, before it is taken. Called from `Tui::enter`
/// beside `theme::detect`, one after the other and never at the same time:
/// both write an escape to the terminal and read what comes back.
#[cfg(not(test))]
pub fn detect() {
    let _ = CHOSEN.set(asked());
}

#[cfg(test)]
pub fn detect() {}

#[cfg(not(test))]
fn asked() -> Graphics {
    if !wanted(std::env::var("BINGO_GRAPHICS").ok().as_deref()) {
        return Graphics::Off;
    }
    // A multiplexer is a terminal of its own that would have to pass the
    // pictures on, and passing them on is not this milestone's (M46
    // non-goals). Asking through one would answer for the wrong terminal.
    if crate::terminal::multiplexed(
        std::env::var("TERM").ok().as_deref(),
        std::env::var_os("TMUX").is_some(),
    ) {
        return Graphics::Off;
    }
    Graphics::from(ask())
}

/// What the terminal answered, read. The reading is the same on every
/// platform; the asking is what differs.
#[cfg(not(test))]
fn ask() -> Probe {
    probe::parse(&exchange())
}

/// Put the three queries on the terminal and read until DA1 comes back.
///
/// The terminal is opened directly rather than through stdout, so a run whose
/// output is redirected still asks the terminal a person is looking at, and
/// the handle is non-blocking, so a terminal that answers nothing costs the
/// timeout and not the run: a blocking read of a tty has no deadline, and a
/// thread abandoned in one would hold a lock the next frame wants.
#[cfg(all(unix, not(test)))]
fn exchange() -> Vec<u8> {
    use std::io::Write;

    let Ok(mut tty) = tty() else {
        return Vec::new();
    };
    // Raw mode, or the answer waits for a newline that will never be typed.
    if crossterm::terminal::enable_raw_mode().is_err() {
        return Vec::new();
    }
    let asked = tty.write_all(probe::QUERY).and_then(|()| tty.flush());
    let answer = match asked {
        Ok(()) => listen(&mut tty),
        Err(_) => Vec::new(),
    };
    let _ = crossterm::terminal::disable_raw_mode();
    answer
}

/// No Windows console host speaks the kitty graphics protocol, and a console
/// that will never answer would cost every start-up the whole timeout. The
/// question is not asked there — design §5's chip is what is drawn, which is
/// what an unanswered question comes to anyway.
#[cfg(all(not(unix), not(test)))]
fn exchange() -> Vec<u8> {
    Vec::new()
}

/// The terminal itself, opened so that a read answers at once whether or not
/// anything has arrived (`O_NONBLOCK`). A process with no controlling
/// terminal has none to open, and draws no pictures.
#[cfg(all(unix, not(test)))]
fn tty() -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/tty")
}

/// Read until the terminal has answered or the clock runs out. Nothing here
/// blocks: an empty read is a wait, and the waiting is bounded.
#[cfg(all(unix, not(test)))]
fn listen(tty: &mut std::fs::File) -> Vec<u8> {
    use std::io::Read;

    let deadline = std::time::Instant::now() + crate::theme::PROBE;
    let mut answer = Vec::new();
    let mut buffer = [0u8; 256];
    while !probe::answered(&answer) && answer.len() < MOST {
        match tty.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => answer.extend_from_slice(&buffer[..read]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(BETWEEN);
            }
            Err(_) => break,
        }
    }
    answer
}

/// How long to wait between two looks at a terminal that has not answered
/// yet. Short enough that a start-up spends no visible time on it, long
/// enough that the wait is not a spin.
#[cfg(all(unix, not(test)))]
const BETWEEN: std::time::Duration = std::time::Duration::from_millis(2);

/// The most an answer may be. A terminal that will not stop talking is not
/// answering the question that was asked.
#[cfg(all(unix, not(test)))]
const MOST: usize = 4096;

#[cfg(test)]
thread_local! {
    /// What one test draws pictures with. Thread-local because the suite runs
    /// in parallel, as the theme's own override is.
    static OVERRIDE: std::cell::Cell<Graphics> = const { std::cell::Cell::new(Graphics::Off) };
}

#[cfg(test)]
pub fn chosen() -> Graphics {
    OVERRIDE.with(std::cell::Cell::get)
}

/// Draw whatever `f` draws on a terminal of this kind.
#[cfg(test)]
pub fn with<R>(graphics: Graphics, f: impl FnOnce() -> R) -> R {
    let previous = OVERRIDE.with(|slot| slot.replace(graphics));
    let out = f();
    OVERRIDE.with(|slot| slot.set(previous));
    out
}

/// A terminal that draws pictures, with the cell most of them have.
#[cfg(test)]
pub fn drawing() -> Graphics {
    Graphics::Kitty {
        cell: Cell {
            width: 10,
            height: 20,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_are_drawn_only_when_both_halves_of_the_answer_came_back() {
        let cell = Cell {
            width: 10,
            height: 20,
        };
        assert_eq!(
            Graphics::from(Probe {
                kitty: true,
                cell: Some(cell)
            }),
            Graphics::Kitty { cell }
        );
        assert_eq!(
            Graphics::from(Probe {
                kitty: true,
                cell: None
            }),
            Graphics::Off,
            "no cell size is no picture, rather than a guessed one"
        );
        assert_eq!(
            Graphics::from(Probe {
                kitty: false,
                cell: Some(cell)
            }),
            Graphics::Off
        );
    }

    #[test]
    fn only_off_switches_the_pictures_off() {
        assert!(!wanted(Some("off")));
        assert!(wanted(None));
        assert!(wanted(Some("")));
        assert!(wanted(Some("1")));
    }
}

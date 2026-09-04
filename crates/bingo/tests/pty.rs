//! The full-screen surface through a real pty.
//!
//! A `TestBackend` proves what is painted; only a terminal can prove what is
//! *left behind*. The surface takes the alternate screen for the whole run,
//! which would take the conversation with it on the way out, so it prints the
//! last screenful back into the shell's own screen — and `--no-print-on-exit`
//! does not. Both are assertions about the normal screen after the child has
//! gone, which is what `vt100` gives us and nothing else does.

// An integration test is not `cfg(test)`; the test-only lint relief is spelled
// out.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// How long any one wait may take before the run is called stalled.
const LIMIT: Duration = Duration::from_secs(30);
const ROWS: u16 = 24;
const COLS: u16 = 80;
/// One reply, from the scripted provider.
const SCRIPT: &str = r#"{"responses":[{"steps":[{"text":"Hello from the pty."}]}]}"#;

/// A `Read` of the picture in the session's directory, then a word about it:
/// the one path by which a picture reaches a transcript without a person
/// pasting one (ADR-0040 §1).
const READS_A_PICTURE: &str = r#"{"responses":[
  {"steps":[{"toolCall":{"name":"Read","input":{"file_path":"shot.png"}}}]},
  {"steps":[{"text":"That is the picture."}]}]}"#;

/// What the terminal at the other end of the pty answers the graphics probe
/// with. Every terminal answers DA1; only one that speaks the kitty protocol
/// answers the graphics query, only some say how big a cell is, and only the
/// ones on M48's list draw the placeholder cells a picture is made of.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Answers {
    /// kitty: `OK`, a cell of 10×20 pixels, its own name, then DA1.
    Pictures,
    /// WezTerm: the same `OK` and the same cell, under a name that is not on
    /// the list — it would draw tofu and a stray picture at the cursor.
    Tofu,
    /// iTerm2, Apple Terminal: DA1 and nothing else.
    Da1Only,
    /// tmux 3.6b with `allow-passthrough on`, an outer Ghostty. tmux answers
    /// the bare XTVERSION for itself and answers it first — it replies to its
    /// own pane before it has forwarded anything (`input.c`) — and the outer
    /// terminal's four answers come out of the envelope behind it.
    ThroughTmux,
    /// The same tmux with the passthrough off: the envelope is dropped whole,
    /// so its own name is all there is and no DA1 reply ever lands.
    TmuxAlone,
}

impl Answers {
    fn reply(self) -> &'static [u8] {
        match self {
            Answers::Pictures => {
                b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1bP>|kitty(0.46.2)\x1b\\\x1b[?62;c"
            }
            Answers::Tofu => {
                b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1bP>|WezTerm 20240203-110809-5046fc22\x1b\\\x1b[?65;4;6;18;22c"
            }
            Answers::Da1Only => b"\x1b[?62;c",
            Answers::ThroughTmux => {
                b"\x1bP>|tmux 3.6b\x1b\\\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1bP>|ghostty 1.3.1\x1b\\\x1b[?62;22c"
            }
            Answers::TmuxAlone => b"\x1bP>|tmux 3.6b\x1b\\",
        }
    }

    /// Whether the child is to believe it is inside tmux. `TMUX` is what a
    /// real one sets, and it is what decides the envelope.
    fn multiplexed(self) -> bool {
        matches!(self, Answers::ThroughTmux | Answers::TmuxAlone)
    }
}

/// What the graphics probe asks, as it appears in the child's output: seeing
/// it is what tells the fake terminal to answer. Wrapped or bare, the query
/// itself reads the same — under tmux there is one more `ESC` in front of it.
const GRAPHICS_QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q";

/// The same query in tmux's passthrough envelope: `DCS tmux;` and then the
/// APC with its `ESC` doubled.
const WRAPPED_QUERY: &[u8] = b"\x1bPtmux;\x1b\x1b_Gi=31,s=1,v=1,a=q";

/// One transmitted picture, in the envelope tmux carries it in.
const WRAPPED_TRANSMIT: &[u8] = b"\x1bPtmux;\x1b\x1b_Ga=T,f=100";

/// A `bingo` on a pty, with everything it reads and writes in one place.
struct Terminal {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    parser: Arc<Mutex<vt100::Parser>>,
    /// Every byte the child wrote, for an assertion about a sequence that
    /// paints no cell and so leaves no mark on the screen.
    written: Arc<Mutex<Vec<u8>>>,
    #[allow(dead_code)]
    home: tempfile::TempDir,
}

impl Terminal {
    fn open(extra: &[&str]) -> Terminal {
        Terminal::opened(extra, SCRIPT, Answers::Da1Only)
    }

    fn opened(extra: &[&str], script_text: &str, answers: Answers) -> Terminal {
        let home = tempfile::tempdir().unwrap();
        let script = home.path().join("script.json");
        std::fs::write(&script, script_text).unwrap();

        let pty = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_bingo"));
        command.args(["--cwd", &home.path().to_string_lossy()]);
        command.args(extra);
        command.env("HOME", home.path());
        command.env("BINGO_FAKE_SCRIPT", &script);
        command.env("TERM", "xterm-256color");
        // The terminal under test is this pty and nothing the suite happens
        // to be running inside: a truecolor claim would send the theme probe
        // looking for an answer nobody here gives, and whether there is a
        // multiplexer is the scene's to say and not the suite's.
        match answers.multiplexed() {
            true => command.env("TMUX", "/tmp/tmux-1000/default,4242,0"),
            false => command.env_remove("TMUX"),
        };
        command.env_remove("COLORTERM");
        command.env_remove("NO_COLOR");
        command.cwd(home.path());
        let child = pty.slave.spawn_command(command).unwrap();
        drop(pty.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let written = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::new(Mutex::new(pty.master.take_writer().unwrap()));
        let mut reader = pty.master.try_clone_reader().unwrap();
        let sink = Arc::clone(&parser);
        let log = Arc::clone(&written);
        let back = Arc::clone(&writer);
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            let mut asked = false;
            while let Ok(read) = reader.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                sink.lock().unwrap().process(&buffer[..read]);
                let mut log = log.lock().unwrap();
                log.extend_from_slice(&buffer[..read]);
                // The answer goes back the moment the question is seen, which
                // is the only ordering a real terminal ever promises.
                if !asked && contains(&log, GRAPHICS_QUERY) {
                    asked = true;
                    let mut back = back.lock().unwrap();
                    back.write_all(answers.reply()).unwrap();
                    back.flush().unwrap();
                }
            }
        });
        Terminal {
            child,
            writer,
            parser,
            written,
            home,
        }
    }

    /// Everything the child has written, escapes and all.
    fn written(&self) -> Vec<u8> {
        self.written.lock().unwrap().clone()
    }

    /// Everything on the screen the terminal is showing right now.
    fn screen(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    fn send(&mut self, bytes: &[u8]) {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    }

    /// Wait for `needle` to appear, or say what was there instead.
    fn wait_for(&self, needle: &str) {
        let deadline = Instant::now() + LIMIT;
        while Instant::now() < deadline {
            if self.screen().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "timed out waiting for {needle:?}\n--- screen ---\n{}",
            self.screen()
        );
    }

    /// Let it finish and answer with the screen the shell is left looking at.
    fn leave(mut self) -> String {
        let deadline = Instant::now() + LIMIT;
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                // The last write may still be in flight.
                std::thread::sleep(Duration::from_millis(200));
                return self.screen();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        panic!("it never left\n--- screen ---\n{}", self.screen());
    }
}

/// One turn, then `ctrl+d` on an empty composer.
fn one_turn_then_leave(extra: &[&str]) -> String {
    let mut terminal = Terminal::open(extra);
    terminal.wait_for("? for shortcuts");
    terminal.send(b"say hello\r");
    terminal.wait_for("Hello from the pty.");
    terminal.send(&[0x04]);
    terminal.leave()
}

#[test]
fn leaving_prints_the_last_screenful_into_the_shells_own_screen() {
    let shell = one_turn_then_leave(&[]);
    assert!(
        shell.contains("Hello from the pty."),
        "the conversation survives the alternate screen:\n{shell}"
    );
    assert!(
        shell.contains("say hello"),
        "and so does what was asked:\n{shell}"
    );
    assert!(
        !shell.contains("? for shortcuts"),
        "the furniture does not:\n{shell}"
    );
}

#[test]
fn no_print_on_exit_leaves_the_shell_as_it_was() {
    let shell = one_turn_then_leave(&["--no-print-on-exit"]);
    assert!(
        !shell.contains("Hello from the pty."),
        "nothing was printed back:\n{shell}"
    );
    assert!(
        shell.trim().is_empty(),
        "the screen is the shell's own:\n{shell}"
    );
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    count(haystack, needle) > 0
}

fn count(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// A picture in the transcript on a terminal that says it draws pictures:
/// the bytes of it go out once, as a virtual placement, and the chip that
/// stands in for one everywhere else is not drawn (design §5, M46).
#[test]
fn a_terminal_that_answers_the_graphics_probe_is_sent_the_picture() {
    let file = bingo_pictures::testing::png_bytes(1200, 900);
    let mut terminal = Terminal::opened(&[], READS_A_PICTURE, Answers::Pictures);
    std::fs::write(terminal.home.path().join("shot.png"), &file).unwrap();
    terminal.wait_for("? for shortcuts");
    terminal.send(b"look at it\r");
    terminal.wait_for("That is the picture.");
    let written = terminal.written();
    assert_eq!(
        count(&written, b"\x1b_Ga=T,f=100"),
        1,
        "the picture went out once, whole, and only once"
    );
    assert!(
        contains(&written, b"U=1"),
        "as a virtual placement the cells stand in for"
    );
    // M48 brick 2: the bytes are cut to the cells they will cover, so a
    // picture far bigger than its block costs the block and not itself.
    let sent = transmitted(&written);
    let size = bingo_pictures::png_size(&sent).expect("a PNG went out");
    assert!(
        size.0 <= 1200 && size.1 < 900,
        "the block's pixels, not the file's: {size:?}"
    );
    assert!(
        sent.len() * 4 < file.len(),
        "{} of {} bytes went out",
        sent.len(),
        file.len()
    );
    assert!(
        !terminal.screen().contains("[image:"),
        "and no chip was drawn:\n{}",
        terminal.screen()
    );
    terminal.send(&[0x04]);
    terminal.leave();
}

/// The PNG of the one `a=T` transmission in `written`, decoded out of the APC
/// sequence that carried it.
fn transmitted(written: &[u8]) -> Vec<u8> {
    use base64::Engine;
    let at = written
        .windows(6)
        .position(|w| w == b"\x1b_Ga=T")
        .expect("a transmission");
    let body = &written[at..];
    let keys = body.iter().position(|b| *b == b';').expect("a payload") + 1;
    let end = body
        .windows(2)
        .position(|w| w == b"\x1b\\")
        .expect("a terminator");
    base64::engine::general_purpose::STANDARD
        .decode(&body[keys..end])
        .expect("base64")
}

/// M48 brick 1: a terminal that answers the graphics query and is not known
/// to draw a placeholder cell gets the chip. It would otherwise paint tofu
/// where the picture goes and leave a stray copy of it at the cursor
/// (wezterm#986, bugs.kde.org 523718).
#[test]
fn a_terminal_that_says_ok_and_draws_no_placeholder_gets_the_chip() {
    let mut terminal = Terminal::opened(&[], READS_A_PICTURE, Answers::Tofu);
    std::fs::write(
        terminal.home.path().join("shot.png"),
        bingo_pictures::testing::png_bytes(100, 200),
    )
    .unwrap();
    terminal.wait_for("? for shortcuts");
    terminal.send(b"look at it\r");
    terminal.wait_for("[image: image/png]");
    assert!(
        !contains(&terminal.written(), b"\x1b_Ga=T"),
        "nothing was transmitted to a terminal that would not draw it"
    );
    terminal.send(&[0x04]);
    terminal.leave();
}

/// The same picture on a terminal that answers only DA1: the chip, and not
/// one byte of graphics protocol.
#[test]
fn a_terminal_that_answers_only_da1_gets_the_chip() {
    let mut terminal = Terminal::opened(&[], READS_A_PICTURE, Answers::Da1Only);
    std::fs::write(
        terminal.home.path().join("shot.png"),
        bingo_pictures::testing::png_bytes(100, 200),
    )
    .unwrap();
    terminal.wait_for("? for shortcuts");
    terminal.send(b"look at it\r");
    terminal.wait_for("[image: image/png]");
    assert!(
        !contains(&terminal.written(), b"\x1b_Ga=T"),
        "nothing was transmitted to a terminal that cannot draw it"
    );
    terminal.send(&[0x04]);
    terminal.leave();
}

/// M49: inside tmux the probe and the picture both travel in the passthrough
/// envelope, and the outer terminal — the one that answers out of it — is
/// what decides whether there is a picture at all.
#[test]
fn a_picture_under_tmux_travels_in_the_passthrough_envelope() {
    let mut terminal = Terminal::opened(&[], READS_A_PICTURE, Answers::ThroughTmux);
    std::fs::write(
        terminal.home.path().join("shot.png"),
        bingo_pictures::testing::png_bytes(1200, 900),
    )
    .unwrap();
    terminal.wait_for("? for shortcuts");
    terminal.send(b"look at it\r");
    terminal.wait_for("That is the picture.");
    let written = terminal.written();
    assert!(
        contains(&written, WRAPPED_QUERY),
        "the question went to the terminal behind tmux"
    );
    assert_eq!(
        count(&written, WRAPPED_TRANSMIT),
        1,
        "and so did the picture, once, in an envelope of its own"
    );
    assert!(
        !terminal.screen().contains("[image:"),
        "and no chip was drawn:\n{}",
        terminal.screen()
    );
    terminal.send(&[0x04]);
    terminal.leave();
}

/// The passthrough off: tmux answers for itself, nothing behind it answers at
/// all, and what a person gets is the chip — not a picture-shaped hole.
#[test]
fn a_tmux_that_carries_nothing_through_gets_the_chip() {
    let mut terminal = Terminal::opened(&[], READS_A_PICTURE, Answers::TmuxAlone);
    std::fs::write(
        terminal.home.path().join("shot.png"),
        bingo_pictures::testing::png_bytes(100, 200),
    )
    .unwrap();
    terminal.wait_for("? for shortcuts");
    terminal.send(b"look at it\r");
    terminal.wait_for("[image: image/png]");
    let written = terminal.written();
    assert!(
        contains(&written, WRAPPED_QUERY),
        "it was asked through the envelope all the same"
    );
    assert!(
        !contains(&written, b"\x1b_Ga=T"),
        "and nothing was transmitted to a terminal that never answered"
    );
    terminal.send(&[0x04]);
    terminal.leave();
}

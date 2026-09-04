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
use std::sync::atomic::{AtomicBool, Ordering};
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

/// A turn that writes a file, so going back to it has something on disk to
/// put back as well as a conversation (M67).
const WRITES_A_NOTE: &str = r#"{"responses":[
  {"steps":[{"toolCall":{"name":"Write","input":{"file_path":"note.md","content":"written by the turn\n"}}}]},
  {"steps":[{"text":"Wrote the note."}]}]}"#;

/// An answer that names a picture in its own words and calls nothing: the
/// path by which a picture reaches the transcript without a tool at all
/// (M51). The file is in the session's directory, so the destination is
/// relative the way a model writes one.
const NAMES_A_PICTURE: &str = r#"{"responses":[
  {"steps":[{"text":"Here it is:\n\n![the shot](shot.png)\n\nThat is all."}]}]}"#;

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
    /// The same tmux, with the passthrough on and nothing behind it answering
    /// inside the window: its own name is all the probe hears, and no DA1
    /// reply ever lands.
    TmuxAlone,
    /// The same tmux, with `allow-passthrough` off. Nothing is asked at all,
    /// so nothing is answered either — the stub `tmux` on the child's `PATH`
    /// is the whole of this scene.
    TmuxPassthroughOff,
    /// tmux 3.6b, passthrough on, and an outer Ghostty whose last two answers
    /// come out of the envelope after the probe's window has run out (M60).
    /// tmux's own name, the kitty `OK` and the cell arrive in time; the name
    /// and DA1 land in crossterm's key stream instead.
    ThroughTmuxLate,
}

impl Answers {
    /// What the terminal answers the moment it sees the question.
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
            Answers::TmuxAlone | Answers::TmuxPassthroughOff => b"\x1bP>|tmux 3.6b\x1b\\",
            Answers::ThroughTmuxLate => b"\x1bP>|tmux 3.6b\x1b\\\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t",
        }
    }

    /// What it answers only after the probe has given up — the reply that
    /// lands in the key stream rather than in the probe's own read.
    fn late(self) -> Option<&'static [u8]> {
        match self {
            Answers::ThroughTmuxLate => Some(b"\x1bP>|ghostty 1.3.1\x1b\\\x1b[?62;22c"),
            _ => None,
        }
    }

    /// Whether the child is to believe it is inside tmux. `TMUX` is what a
    /// real one sets, and it is what decides the envelope.
    fn multiplexed(self) -> bool {
        self.passthrough().is_some()
    }

    /// What the stub `tmux` on the child's `PATH` says about
    /// `allow-passthrough`, or `None` for a scene with no tmux in it. The stub
    /// is what makes the answer the scene's rather than the machine's: a real
    /// tmux on the box would be asked about a socket that is not there.
    fn passthrough(self) -> Option<&'static str> {
        match self {
            Answers::ThroughTmux | Answers::TmuxAlone | Answers::ThroughTmuxLate => Some("on"),
            Answers::TmuxPassthroughOff => Some("off"),
            Answers::Pictures | Answers::Tofu | Answers::Da1Only => None,
        }
    }
}

/// What the terminal at the other end says about its own ground, which is what
/// decides the palette bingo draws in (M71). Every scene but this milestone's
/// leaves `COLORTERM` unset: a terminal of eight colours has no palette to
/// follow, so nothing is asked and nothing has to be answered.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ground {
    /// Eight colours, and no question ever put.
    Eight,
    /// Twenty-four bits and a ground the terminal will say: dark to begin
    /// with, and light from the moment the test turns it.
    Answered,
    /// Twenty-four bits and `BINGO_THEME=dark` — a look a person named, which
    /// the terminal is not asked about, then or ever.
    Named,
}

impl Ground {
    /// Whether this scene's terminal answers the colour question at all.
    fn answers(self) -> bool {
        self == Ground::Answered
    }

    fn env(self, command: &mut CommandBuilder) {
        match self {
            Ground::Eight => command.env_remove("COLORTERM"),
            Ground::Answered => command.env("COLORTERM", "truecolor"),
            Ground::Named => {
                command.env("COLORTERM", "truecolor");
                command.env("BINGO_THEME", "dark");
            }
        }
    }
}

/// The colour question, as it appears in the child's output: the probe's own
/// (`terminal_colorsaurus` asks for the ink, the ground and DA1 in one write)
/// and every one the run puts afterwards (M71).
const GROUND_QUERY: &[u8] = b"\x1b]11;?";

/// What a terminal answers it with: the ink and the ground of one look, and —
/// for the probe, which asks for both and reads until DA1 — the DA1 that ends
/// that read. A later question asks only for the ground, so only the ground is
/// answered: everything else would land in the key stream for nothing.
fn ground_reply(light: bool, probe: bool) -> Vec<u8> {
    let (ink, ground) = match light {
        true => ("2424/2020/1a1a", "fdfd/f6f6/e3e3"),
        false => ("ecec/e7e7/dfdf", "1e1e/1e1e/2e2e"),
    };
    let mut out = Vec::new();
    if probe {
        out.extend_from_slice(format!("\x1b]10;rgb:{ink}\x1b\\").as_bytes());
    }
    out.extend_from_slice(format!("\x1b]11;rgb:{ground}\x1b\\").as_bytes());
    if probe {
        out.extend_from_slice(b"\x1b[?62;c");
    }
    out
}

/// How long after the question a late answer arrives: the window the surface
/// itself waits under tmux, and a margin on top of it. Driven from the
/// surface's own constant so the two cannot drift apart.
const LATE: Duration =
    Duration::from_millis(bingo_surface_tui::PROBE_THROUGH.as_millis() as u64 + 400);

/// How far apart two keystrokes of one gesture are sent. Two writes back to
/// back arrive in one read, and one read of `ESC ESC` does not become two
/// `esc` events; a person's two escapes are a moment apart. It is a lower
/// bound on the gap and never a deadline — a slower machine makes it longer,
/// which is the direction that cannot fail.
const BETWEEN_KEYS: Duration = Duration::from_millis(200);

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
    /// Set once the scene's late answer has gone down the wire, so a test
    /// waits on the write itself and never on a clock of its own.
    answered_late: Arc<AtomicBool>,
    /// Whether the ground this terminal answers with is the light one. A test
    /// turns it under the running child, the way a system's appearance turns
    /// under a terminal that follows it (M71).
    light: Arc<AtomicBool>,
    home: tempfile::TempDir,
}

impl Terminal {
    fn open(extra: &[&str]) -> Terminal {
        Terminal::opened(extra, SCRIPT, Answers::Da1Only)
    }

    fn opened(extra: &[&str], script_text: &str, answers: Answers) -> Terminal {
        Terminal::under(extra, script_text, answers, Ground::Eight)
    }

    fn under(extra: &[&str], script_text: &str, answers: Answers, ground: Ground) -> Terminal {
        let home = tempfile::tempdir().unwrap();
        let script = home.path().join("script.json");
        std::fs::write(&script, script_text).unwrap();
        // The suite reaches nothing outward. A run with a terminal asks once
        // a day whether a newer release is out (M63); a test's run says no,
        // as it says no to every other outward call.
        std::fs::create_dir_all(home.path().join(".bingo")).unwrap();
        std::fs::write(
            home.path().join(".bingo/settings.json"),
            r#"{ "update": { "check": false } }"#,
        )
        .unwrap();

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
        stub_tmux(&mut command, home.path(), answers.passthrough());
        // The terminal under test is this pty and nothing the suite happens
        // to be running inside: what colour it has is the scene's to say, and
        // so is whether there is a multiplexer.
        match answers.multiplexed() {
            true => command.env("TMUX", "/tmp/tmux-1000/default,4242,0"),
            false => command.env_remove("TMUX"),
        };
        ground.env(&mut command);
        command.env_remove("NO_COLOR");
        command.cwd(home.path());
        let child = pty.slave.spawn_command(command).unwrap();
        drop(pty.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let written = Arc::new(Mutex::new(Vec::new()));
        let writer = Arc::new(Mutex::new(pty.master.take_writer().unwrap()));
        let answered_late = Arc::new(AtomicBool::new(answers.late().is_none()));
        let light = Arc::new(AtomicBool::new(false));
        let mut reader = pty.master.try_clone_reader().unwrap();
        let sink = Arc::clone(&parser);
        let log = Arc::clone(&written);
        let back = Arc::clone(&writer);
        let done = Arc::clone(&answered_late);
        let ground_now = Arc::clone(&light);
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            let mut asked = false;
            let mut grounds = 0;
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
                    let mut out = back.lock().unwrap();
                    out.write_all(answers.reply()).unwrap();
                    out.flush().unwrap();
                    drop(out);
                    // The rest of it, once the window has run out — on a
                    // thread of its own, so the child's output keeps being
                    // read while the wait runs.
                    if let Some(rest) = answers.late() {
                        answer_late(Arc::clone(&back), Arc::clone(&done), rest);
                    }
                }
                // The colour question, answered every time it is put, with the
                // ground the scene is in now (M71).
                while ground.answers() && grounds < counted(&log, GROUND_QUERY) {
                    let reply = ground_reply(ground_now.load(Ordering::SeqCst), grounds == 0);
                    grounds += 1;
                    let mut out = back.lock().unwrap();
                    out.write_all(&reply).unwrap();
                    out.flush().unwrap();
                }
            }
        });
        Terminal {
            child,
            writer,
            parser,
            written,
            answered_late,
            light,
            home,
        }
    }

    /// Wait until the scene's late answer has been written, or say so.
    fn wait_late(&self) {
        let deadline = Instant::now() + LIMIT;
        while Instant::now() < deadline {
            if self.answered_late.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("the late answer was never written");
    }

    /// The child's home, which is also its working directory: where a file a
    /// turn wrote lands, and where a rewind puts it back.
    fn home(&self) -> &std::path::Path {
        self.home.path()
    }

    /// Everything the child has written, escapes and all.
    fn written(&self) -> Vec<u8> {
        self.written.lock().unwrap().clone()
    }

    /// Everything on the screen the terminal is showing right now.
    fn screen(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

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

    fn send(&mut self, bytes: &[u8]) {
        let mut writer = self.writer.lock().unwrap();
        writer.write_all(bytes).unwrap();
        writer.flush().unwrap();
    }

    /// Wait for the child to write `needle`, escapes and all — for a
    /// sequence that paints no cell and so never reaches the screen.
    fn wait_written(&self, needle: &[u8]) {
        let deadline = Instant::now() + LIMIT;
        while Instant::now() < deadline {
            if contains(&self.written(), needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "timed out waiting for {needle:?} in the child's output\n--- screen ---\n{}",
            self.screen()
        );
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

/// A `!` line runs in the shell there and then and its output lands in the
/// conversation, with no model turn spent on it (M65). The item is journaled
/// whole, so the line and what it wrote reach the screen together.
#[test]
fn a_bang_line_runs_the_shell_and_leaves_its_output_in_the_transcript() {
    let mut terminal = Terminal::open(&[]);
    terminal.wait_for("? for shortcuts");
    terminal.send(b"!echo landed\r");
    terminal.wait_for("$ echo landed");
    let screen = terminal.screen();
    assert_eq!(
        screen.matches("landed").count(),
        2,
        "the line, and under it what it wrote:\n{screen}"
    );
    assert!(
        !screen.contains("Hello from the pty."),
        "the model was never asked:\n{screen}"
    );
    terminal.send(&[0x04]);
    terminal.leave();
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

/// Every frame is written between the two halves of DEC mode 2026, so the
/// terminal composites it instead of painting it as it arrives (design §6).
/// A `TestBackend` sees cells and never bytes; this is the only place the
/// bracket is visible.
#[test]
fn every_frame_is_written_inside_a_synchronized_update() {
    const BEGIN: &[u8] = b"\x1b[?2026h";
    const END: &[u8] = b"\x1b[?2026l";
    let mut terminal = Terminal::open(&[]);
    terminal.wait_for("? for shortcuts");
    terminal.send(b"say hello\r");
    terminal.wait_for("Hello from the pty.");
    let written = terminal.written();
    let (begins, ends) = (count(&written, BEGIN), count(&written, END));
    assert!(begins > 1, "one bracket per frame, and there were frames");
    assert!(
        begins == ends || begins == ends + 1,
        "every frame that opened has closed, bar the one in flight: \
         {begins} begun, {ends} ended"
    );
    let first = written
        .windows(BEGIN.len())
        .position(|w| w == BEGIN)
        .expect("a frame was begun");
    let closed = written
        .windows(END.len())
        .position(|w| w == END)
        .expect("a frame was ended");
    assert!(first < closed, "and the frame is inside its own bracket");
    terminal.send(&[0x04]);
    terminal.leave();

    // A multiplexer is a terminal of its own: it eats the mode rather than
    // passing it on, and only tmux ≥ 3.7 acts on it at all — 3.7 and 3.7a
    // wrongly. So under one the frame goes out bare.
    let mut through = Terminal::opened(&[], SCRIPT, Answers::ThroughTmux);
    through.wait_for("? for shortcuts");
    through.send(b"say hello\r");
    through.wait_for("Hello from the pty.");
    let under_tmux = through.written();
    assert_eq!(count(&under_tmux, BEGIN), 0, "nothing to say to tmux");
    assert_eq!(count(&under_tmux, END), 0);
    through.send(&[0x04]);
    through.leave();
}

/// `esc esc` on an empty composer lists the turns, and `⏎` goes back to one:
/// the file the turn wrote is gone again and the transcript says what it
/// dropped (M67, ADR-0045). The picker itself is M11e's and unchanged — what
/// is new is that a `rewind` is in the catalogue for it to offer.
#[test]
fn esc_twice_rewinds_the_turn_and_the_file_it_wrote() {
    let mut terminal = Terminal::opened(
        &["--allowed-tools", "Write"],
        WRITES_A_NOTE,
        Answers::Da1Only,
    );
    terminal.wait_for("? for shortcuts");
    terminal.send(b"write me a note\r");
    terminal.wait_for("Wrote the note.");
    let note = terminal.home().join("note.md");
    assert!(note.is_file(), "the turn wrote it");

    // Two escapes, a moment apart: any other key between them would say they
    // were not one gesture.
    terminal.send(b"\x1b");
    std::thread::sleep(BETWEEN_KEYS);
    terminal.send(b"\x1b");
    terminal.wait_for("Rewind to");
    terminal.send(b"\r");
    terminal.wait_for("rewound,");

    assert!(
        !note.exists(),
        "a file the turn created is gone again:\n{}",
        terminal.screen()
    );
    let screen = terminal.screen();
    assert!(
        !screen.contains("Wrote the note."),
        "and the turn is out of the transcript:\n{screen}"
    );
    terminal.send(&[0x04]);
    terminal.leave();
}

// ---- the look that follows the terminal (M71) ---------------------------

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

/// Answer the rest of it after [`LATE`], from a thread of its own.
fn answer_late(
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    done: Arc<AtomicBool>,
    rest: &'static [u8],
) {
    std::thread::spawn(move || {
        std::thread::sleep(LATE);
        let mut out = writer.lock().unwrap();
        out.write_all(rest).unwrap();
        out.flush().unwrap();
        done.store(true, Ordering::SeqCst);
    });
}

/// Put a `tmux` on the child's `PATH` that answers one word about
/// `allow-passthrough`, so the scene decides what the surface is told rather
/// than whatever tmux the machine happens to carry.
fn stub_tmux(command: &mut CommandBuilder, home: &std::path::Path, says: Option<&str>) {
    let Some(says) = says else {
        return;
    };
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let tmux = bin.join("tmux");
    std::fs::write(&tmux, format!("#!/bin/sh\nprintf '{says}\\n'\n")).unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&tmux, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    command.env("PATH", format!("{}:{path}", bin.display()));
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

/// How many times `needle` occurs in `haystack`.
fn counted(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
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
    // The cells are on the screen a frame before the pixels are: fitting a
    // picture to them is a decode, and the run does it off the loop (M61).
    terminal.wait_written(b"\x1b_Ga=T,f=100");
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

/// A picture the answer named in its own words, on a terminal that draws
/// pictures (M51): the chip stays as the line it hangs from, the file is read
/// in between frames, and the bytes go out once.
#[test]
fn a_picture_an_answer_named_in_markdown_is_read_in_and_sent() {
    let mut terminal = Terminal::opened(&[], NAMES_A_PICTURE, Answers::Pictures);
    std::fs::write(
        terminal.home.path().join("shot.png"),
        bingo_pictures::testing::png_bytes(300, 200),
    )
    .unwrap();
    terminal.wait_for("? for shortcuts");
    terminal.send(b"show me the shot\r");
    terminal.wait_for("[image: the shot]");
    // The read happens between frames, so the bytes come after the chip.
    terminal.wait_written(b"\x1b_Ga=T,f=100");
    let written = terminal.written();
    assert_eq!(
        count(&written, b"\x1b_Ga=T,f=100"),
        1,
        "read in once and sent once"
    );
    assert!(contains(&written, b"U=1"), "as a virtual placement");
    let sent = transmitted(&written);
    assert!(
        bingo_pictures::png_size(&sent).is_some(),
        "and what went out is a PNG"
    );
    let screen = terminal.screen();
    assert!(
        screen.contains("[image: the shot]"),
        "the chip stays: it is the line the picture hangs from\n{screen}"
    );
    assert!(
        screen.contains("That is all."),
        "and the words after it are still there\n{screen}"
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
    terminal.wait_written(WRAPPED_TRANSMIT);
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

/// M60 bricks 1, 2 and 4: the outer terminal finishes answering after the
/// probe's window has run out. Its reply lands in crossterm's key stream —
/// `alt+P`, `>`, `|`, the name, `alt+\` — where before it was typed into the
/// composer and made the box grow a row. Nothing of it reaches the composer
/// now, and the answer it carries turns the pictures on for the next frame.
#[test]
fn an_answer_that_lands_after_the_probe_is_eaten_and_still_counted() {
    let mut terminal = Terminal::opened(&[], READS_A_PICTURE, Answers::ThroughTmuxLate);
    std::fs::write(
        terminal.home.path().join("shot.png"),
        bingo_pictures::testing::png_bytes(1200, 900),
    )
    .unwrap();
    terminal.wait_for("? for shortcuts");
    terminal.wait_late();
    // A keystroke behind the late answer, and the screen that shows it: the
    // pty keeps its order, so a composer holding this and nothing else is a
    // composer the reply never reached. The box is where the bug showed —
    // `> Gi=31;OK>|ghostty 1.3.1`, and a second row of it once the words
    // outgrew the width, which is the layout jump (M60 Verified).
    terminal.send(b"look at it");
    terminal.wait_for("look at it");
    let composer = terminal.screen();
    for typed in ["Gi=31", ">|ghostty", "OK>"] {
        assert!(
            !composer.contains(typed),
            "{typed:?} was typed into the composer:\n{composer}"
        );
    }
    terminal.send(b"\r");
    terminal.wait_for("That is the picture.");
    terminal.wait_written(WRAPPED_TRANSMIT);
    let written = terminal.written();
    assert_eq!(
        count(&written, WRAPPED_TRANSMIT),
        1,
        "the picture the late answer paid for went out, once, in an envelope"
    );
    assert!(
        !terminal.screen().contains("[image:"),
        "and no chip was drawn:\n{}",
        terminal.screen()
    );
    terminal.send(&[0x04]);
    terminal.leave();
}

/// M60 bricks 3 and 4: tmux says `allow-passthrough` is off, so the question
/// is not asked at all. Nothing wrapped is written, no window is spent waiting
/// for an answer that was promised not to come, and the chip is what a picture
/// draws as.
#[test]
fn a_passthrough_tmux_says_is_off_is_never_asked_through() {
    let mut terminal = Terminal::opened(&[], READS_A_PICTURE, Answers::TmuxPassthroughOff);
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
        !contains(&written, WRAPPED_QUERY),
        "no envelope was sent into a passthrough that drops it"
    );
    assert!(
        !contains(&written, GRAPHICS_QUERY),
        "and the question was not asked bare either"
    );
    // The wait that is not spent is the question that is not asked: there is
    // no window to time, because nothing was ever waited for. A wall-clock
    // bound here would pin this machine instead (AGENTS.md).
    assert!(
        !contains(&written, b"\x1b_Ga=T"),
        "and nothing was transmitted to a terminal that was never reached"
    );
    terminal.send(&[0x04]);
    terminal.leave();
}

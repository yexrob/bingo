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

/// A `bingo` on a pty, with everything it reads and writes in one place.
struct Terminal {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    parser: Arc<Mutex<vt100::Parser>>,
    #[allow(dead_code)]
    home: tempfile::TempDir,
}

impl Terminal {
    fn open(extra: &[&str]) -> Terminal {
        let home = tempfile::tempdir().unwrap();
        let script = home.path().join("script.json");
        std::fs::write(&script, SCRIPT).unwrap();

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
        command.cwd(home.path());
        let child = pty.slave.spawn_command(command).unwrap();
        drop(pty.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let mut reader = pty.master.try_clone_reader().unwrap();
        let sink = Arc::clone(&parser);
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while let Ok(read) = reader.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                sink.lock().unwrap().process(&buffer[..read]);
            }
        });
        Terminal {
            child,
            writer: pty.master.take_writer().unwrap(),
            parser,
            home,
        }
    }

    /// Everything on the screen the terminal is showing right now.
    fn screen(&self) -> String {
        self.parser.lock().unwrap().screen().contents()
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).unwrap();
        self.writer.flush().unwrap();
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

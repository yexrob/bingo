//! Black-box: the permission ladder `--print` puts to a person at a terminal.
//!
//! The line offers exactly the answers the interaction carries and no more
//! (ADR-0039 §2). A call a session rule could silence offers one; a write into
//! a sensitive path, which no rule may silence, does not — and the key that
//! would have installed one is not on the line and does not answer.
//!
//! Only a terminal asks at all: `--print` off a tty refuses without a word,
//! which is what the other tests here see. So these run on a pty.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

/// How long any one wait may take before the run is called stalled.
const LIMIT: Duration = Duration::from_secs(30);

/// A write a session rule could silence: an ordinary file in the session's
/// own directory.
const WRITES_A_NOTE: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"Write","input":{
        "file_path":"note.md","content":"written by the turn\n"}}}]},
    {"steps":[{"text":"done"}]}
]}"#;

/// A write no rule may silence: `.git` decides what the tools themselves may
/// do next, so the gate asks about it whatever the allow table says.
const WRITES_A_HOOK: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"Write","input":{
        "file_path":".git/hooks/pre-commit","content":"written by the turn\n"}}}]},
    {"steps":[{"text":"done"}]}
]}"#;

#[test]
fn a_permission_a_rule_could_silence_offers_the_session_rung() {
    let mut run = AtTheTerminal::open(WRITES_A_NOTE, "write the note");
    let asked = run.until("[permission]");
    assert!(asked.contains("[a]lways this session"), "{asked}");
    run.answer("y");
    let code = run.finish();
    assert_eq!(code, Some(0), "{}", run.transcript());
    assert!(run.wrote("note.md"), "a yes did not write the file");
}

/// The rung the interaction does not carry is not on the line, and the key
/// that would have picked it answers nothing: the call is refused rather than
/// allowed by a keystroke the kernel never offered.
#[test]
fn a_permission_no_rule_could_silence_offers_no_session_rung() {
    let mut run = AtTheTerminal::open(WRITES_A_HOOK, "write the hook");
    let asked = run.until("[permission]");
    assert!(asked.contains("[y]es"), "{asked}");
    assert!(asked.contains("[n]o"), "{asked}");
    assert!(
        !asked.contains("[a]lways this session"),
        "a rung the kernel would refuse was offered: {asked}"
    );
    run.answer("a");
    let code = run.finish();
    assert_eq!(code, Some(0), "{}", run.transcript());
    assert!(
        !run.wrote(".git/hooks/pre-commit"),
        "a key that was not offered allowed the call"
    );
}

/// `bingo --print` with a person at the keyboard: the run's own pty, and
/// everything it has written so far.
struct AtTheTerminal {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    seen: Arc<Mutex<Vec<u8>>>,
    home: tempfile::TempDir,
}

impl AtTheTerminal {
    fn open(script: &str, prompt: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 200,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let child = pty
            .slave
            .spawn_command(spelled(script, prompt, &home))
            .unwrap();
        drop(pty.slave);

        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let mut reader = pty.master.try_clone_reader().unwrap();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            while let Ok(read) = reader.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buffer[..read]);
            }
        });
        Self {
            child,
            writer: pty.master.take_writer().unwrap(),
            seen,
            home,
        }
    }

    /// Everything written so far, once it holds `needle`.
    fn until(&self, needle: &str) -> String {
        let started = Instant::now();
        loop {
            let so_far = String::from_utf8_lossy(&self.seen.lock().unwrap()).into_owned();
            if so_far.contains(needle) {
                return so_far;
            }
            assert!(
                started.elapsed() < LIMIT,
                "no {needle:?} within {LIMIT:?}: {so_far}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Answer, once the kernel's own guard against a keystroke that was
    /// already on its way has run out: an answer inside it is refused as
    /// `NOT_READY`, so the run waits the guard out rather than racing it.
    fn answer(&mut self, line: &str) {
        let guard = u64::try_from(bingo_core::session::INTERACTION_GUARD_MS).unwrap_or(400);
        std::thread::sleep(Duration::from_millis(guard * 2));
        writeln!(self.writer, "{line}").unwrap();
        self.writer.flush().unwrap();
    }

    /// The run's exit code, or `None` where the platform has no code for how
    /// it ended.
    fn finish(&mut self) -> Option<i32> {
        let status = self.child.wait().unwrap();
        i32::try_from(status.exit_code()).ok()
    }

    fn transcript(&self) -> String {
        String::from_utf8_lossy(&self.seen.lock().unwrap()).into_owned()
    }

    fn wrote(&self, path: &str) -> bool {
        self.home.path().join(path).exists()
    }
}

/// The run, spelled out: the scripted provider, a home of its own, and no
/// outward call — a run with a terminal would otherwise ask once a day
/// whether a newer release is out (M63).
fn spelled(script: &str, prompt: &str, home: &tempfile::TempDir) -> CommandBuilder {
    let path = home.path().join("script.json");
    std::fs::write(&path, script).unwrap();
    std::fs::create_dir_all(home.path().join(".bingo")).unwrap();
    std::fs::write(
        home.path().join(".bingo/settings.json"),
        r#"{ "update": { "check": false } }"#,
    )
    .unwrap();
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_bingo"));
    command.args(["--print", "--cwd", &home.path().to_string_lossy(), prompt]);
    command.env("HOME", home.path());
    command.env("BINGO_FAKE_SCRIPT", &path);
    command.env("TERM", "xterm-256color");
    command.env_remove("ANTHROPIC_API_KEY");
    command.env_remove("OPENAI_API_KEY");
    command.cwd(home.path());
    command
}

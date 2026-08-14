//! Composer power tools (D86): the kill ring behind the readline kills, the
//! `ctrl+x ctrl+e` chord, and the `$EDITOR` round trip.
//!
//! Everything the state machine needs lives in [`Composer`], which `Chat` owns
//! and `chat_tail.rs` only wires keys to. The editor round trip is split in
//! two on purpose: [`compose_with`] is the content half (draft in, draft out)
//! and takes the command as an argument, so the acceptance tests drive it
//! without a TTY and without writing to the process environment; [`run_editor`]
//! is the host half that resolves the environment, hands the terminal over and
//! takes it back.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::EventStream;

use crate::tui::chat::{Chat, EditKind};

/// How many killed spans the ring remembers (readline's default is 10).
pub const KILL_RING_MAX: usize = 10;

/// How long `ctrl+x` stays armed waiting for its second key. The chord also
/// dies on the next key whatever it is, so this only matters when the user
/// pressed `ctrl+x` and then walked away.
pub const CHORD_WINDOW: Duration = Duration::from_millis(1500);

/// The info-tier line when neither `$VISUAL` nor `$EDITOR` is set.
pub const NO_EDITOR_HINT: &str = "set $EDITOR to edit the prompt in your editor";
/// The info-tier line when the editor exited non-zero: the draft it was handed stands.
pub const EDITOR_KEPT_HINT: &str = "editor exited without saving · the prompt is unchanged";
/// The info-tier line when the editor exited cleanly having written nothing.
///
/// Says the likely cause because the common one destroys work silently: an
/// editor that opens a window and returns immediately (`code`, `zed`, `subl`
/// and friends without their wait flag) is read back before the user has typed
/// a character, so the edit lands in a file this call has already finished with.
pub const EDITOR_UNCHANGED_HINT: &str = "the editor saved no changes · an editor that opens a window needs its wait flag (e.g. \"code -w\")";

/// Which end of the cursor a kill ate. Consecutive kills in the same direction
/// join into one ring entry (readline: a `ctrl+w ctrl+w` yanks both words back
/// in the order they were typed, not just the last one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KillDir {
    Back,
    Forward,
}

/// A yank that is still the most recent thing that happened, so `alt+y` knows
/// what to replace and where.
#[derive(Debug, Clone, Copy)]
struct Yank {
    seq: u64,
    start: usize,
    len: usize,
    depth: usize,
}

/// The composer's readline state: kill ring, and the two "immediately after"
/// rules (kill coalescing and yank-pop).
///
/// Both rules are decided by a key counter rather than a clock: [`Composer::tick`]
/// runs once per key event, and "immediately after" means the previous
/// sequence number. A key that went to a dialog or a dropdown still ticks, so
/// it breaks the chain — which is what readline means by an intervening command.
#[derive(Debug, Default)]
pub struct Composer {
    ring: VecDeque<String>,
    last_kill: Option<(u64, KillDir)>,
    last_yank: Option<Yank>,
    seq: u64,
    chord: Option<Instant>,
}

impl Composer {
    /// One key event arrived. Returns the new sequence number.
    pub fn tick(&mut self) -> u64 {
        self.seq = self.seq.wrapping_add(1);
        self.seq
    }

    /// Record a killed span. Empty kills (a `ctrl+w` at the start of the line)
    /// are dropped rather than pushed as an empty entry that `ctrl+y` would
    /// yank as nothing.
    pub fn kill(&mut self, text: String, dir: KillDir) {
        if text.is_empty() {
            return;
        }
        let coalesce = self.last_kill == Some((self.seq.wrapping_sub(1), dir));
        self.last_kill = Some((self.seq, dir));
        match (coalesce, self.ring.front_mut()) {
            // The span the previous kill took sits next to this one in the
            // text, so the entry is rebuilt in text order, not press order.
            (true, Some(front)) => match dir {
                KillDir::Back => front.insert_str(0, &text),
                KillDir::Forward => front.push_str(&text),
            },
            _ => {
                self.ring.push_front(text);
                if self.ring.len() > KILL_RING_MAX {
                    self.ring.pop_back();
                }
            }
        }
    }

    /// The text `ctrl+y` inserts, if the ring holds anything.
    pub fn top(&self) -> Option<&str> {
        self.ring.front().map(String::as_str)
    }

    /// How many entries the ring holds (the bound is [`KILL_RING_MAX`]).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn ring_len(&self) -> usize {
        self.ring.len()
    }

    /// Remember where `ctrl+y` just put the top entry, so that an `alt+y` on
    /// the very next key can replace exactly that span.
    pub fn note_yank(&mut self, start: usize, len: usize) {
        self.last_yank = Some(Yank {
            seq: self.seq,
            start,
            len,
            depth: 0,
        });
    }

    /// `alt+y`: rotate the ring and report `(start, old_len, replacement)` for
    /// the span the previous key yanked. `None` when the previous key was not
    /// a yank, or when the ring has nothing to rotate to — `alt+y` out of
    /// context does nothing at all rather than inserting something.
    pub fn yank_pop(&mut self) -> Option<(usize, usize, String)> {
        let previous = self.last_yank?;
        if previous.seq != self.seq.wrapping_sub(1) || self.ring.len() < 2 {
            return None;
        }
        let depth = (previous.depth + 1) % self.ring.len();
        let text = self.ring.get(depth)?.clone();
        self.last_yank = Some(Yank {
            seq: self.seq,
            start: previous.start,
            len: text.len(),
            depth,
        });
        Some((previous.start, previous.len, text))
    }

    /// `ctrl+x`: arm the chord for one key.
    pub fn arm_chord(&mut self, now: Instant) {
        self.chord = Some(now);
    }

    /// Consume the armed chord, reporting whether it is still live. Called
    /// once per key event, so anything other than `ctrl+e` clears it.
    pub fn take_chord(&mut self, now: Instant) -> bool {
        self.chord
            .take()
            .is_some_and(|at| now.duration_since(at) <= CHORD_WINDOW)
    }
}

/// What the `$EDITOR` round trip produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorOutcome {
    /// The editor saved: the file's content, trailing newline trimmed.
    Edited(String),
    /// The editor exited zero having left the file byte-identical to the draft
    /// it was handed.
    ///
    /// Told apart from [`EditorOutcome::Edited`] on purpose. The two are the
    /// same *result* — the draft stands — but not the same *event*, and
    /// collapsing them made the one way this round trip loses work look like a
    /// success: an editor that forks a window and returns is read back and its
    /// file removed before the user has typed anything, so the edit they go on
    /// to save has nowhere left to land. Silence there reads as "bingo dropped
    /// my edit", which is exactly what happened, with nothing on screen saying so.
    Unchanged,
    /// The editor exited non-zero. The draft it was handed stands.
    Kept,
    /// Neither `$VISUAL` nor `$EDITOR` names an editor.
    Unset,
    /// The editor could not be run, or its file could not be read or written.
    Failed(String),
}

impl EditorOutcome {
    /// The info-tier line for an outcome that left the draft alone. A
    /// successful edit says nothing: the new content is the feedback.
    pub fn note(&self) -> Option<String> {
        match self {
            EditorOutcome::Edited(_) => None,
            EditorOutcome::Unchanged => Some(EDITOR_UNCHANGED_HINT.to_string()),
            EditorOutcome::Kept => Some(EDITOR_KEPT_HINT.to_string()),
            EditorOutcome::Unset => Some(NO_EDITOR_HINT.to_string()),
            EditorOutcome::Failed(e) => Some(format!("could not run the editor: {e}")),
        }
    }
}

/// Resolve the editor command. `$VISUAL` wins over `$EDITOR` (readline's own
/// order: `$VISUAL` is the full-screen one, which is what a prompt wants), and
/// a variable set to whitespace counts as unset.
pub fn editor_command(visual: Option<&str>, editor: Option<&str>) -> Option<String> {
    [visual, editor]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|value| !value.is_empty())
        .map(str::to_string)
}

/// Split the command into program and leading arguments on whitespace.
/// `$EDITOR` routinely carries flags (`code -w`, `emacs -nw`); shell quoting is
/// not honoured, because running it through a shell would cost the portability
/// this has no need to spend.
fn split_command(command: &str) -> Option<(&str, Vec<&str>)> {
    let mut parts = command.split_whitespace();
    let program = parts.next()?;
    Some((program, parts.collect()))
}

/// A temp file this process owns for the length of one edit. Pid-tagged and
/// counted, so two edits — including two tests on two threads — never collide.
fn draft_path() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("bingo-prompt-{}-{n}.md", std::process::id()))
}

/// Write `draft` to a temp file, run `command` over it inheriting this
/// terminal, and read it back. The file is removed on every path out.
///
/// The editor owns the terminal for the length of this call; the caller is
/// responsible for having handed it over (see [`run_editor`]).
pub fn edit_draft(command: &str, draft: &str) -> EditorOutcome {
    let Some((program, args)) = split_command(command) else {
        return EditorOutcome::Unset;
    };
    let path = draft_path();
    if let Err(e) = std::fs::write(&path, draft) {
        return EditorOutcome::Failed(e.to_string());
    }
    let status = std::process::Command::new(program)
        .args(args)
        .arg(&path)
        .status();
    let outcome = match status {
        Err(e) => EditorOutcome::Failed(e.to_string()),
        Ok(status) if !status.success() => EditorOutcome::Kept,
        // Read back by *path*, never through a handle opened before the child
        // ran: the majority of editors save by writing a sibling file and
        // renaming it over this one, which leaves the name pointing at a new
        // inode. Anything holding the old one would read the draft back
        // verbatim and call it an edit.
        Ok(_) => match std::fs::read_to_string(&path) {
            Ok(text) if text == draft => EditorOutcome::Unchanged,
            Ok(text) => EditorOutcome::Edited(trim_trailing_newline(&text)),
            Err(e) => EditorOutcome::Failed(e.to_string()),
        },
    };
    // Only ever this one file, created by this call.
    let _ = std::fs::remove_file(&path);
    outcome
}

/// Editors add the trailing newline a text file is supposed to end with; the
/// composer is a prompt, not a file, so it comes back off.
fn trim_trailing_newline(text: &str) -> String {
    text.strip_suffix('\n')
        .map(|t| t.strip_suffix('\r').unwrap_or(t))
        .unwrap_or(text)
        .to_string()
}

/// The content half of the round trip: hand the composer's draft to `command`
/// and put the answer back. `None` is "no editor configured".
///
/// A replaced draft is one undo step, so `ctrl+_` returns to what was typed
/// before the editor opened.
pub fn compose_with(chat: &mut Chat, command: Option<&str>) {
    let outcome = match command {
        Some(command) => edit_draft(command, &chat.input.clone()),
        None => EditorOutcome::Unset,
    };
    if let EditorOutcome::Edited(text) = &outcome {
        chat.snapshot(EditKind::Bulk);
        chat.set_input(text.clone());
    }
    if let Some(note) = outcome.note() {
        chat.slash_info_lines = vec![note];
    }
    chat.dirty = true;
}

/// Hand the terminal to `$EDITOR` over the current draft, then take it back.
///
/// The stdin reader goes first. Crossterm's [`EventStream`] parks a thread
/// inside `poll_internal`, which *reads* the terminal, and an editor sharing
/// that descriptor would lose keystrokes to it; dropping the stream wakes that
/// thread and ends it, and the caller continues on the fresh one left in its
/// place. The terminal itself is handed over through the D77 claim protocol,
/// so a panic while the editor is up restores the same screen a clean return
/// would have.
pub fn run_editor(chat: &mut Chat, events: &mut EventStream) {
    let command = editor_command(
        std::env::var("VISUAL").ok().as_deref(),
        std::env::var("EDITOR").ok().as_deref(),
    );
    if command.is_none() {
        compose_with(chat, None);
        return;
    }
    *events = EventStream::new();
    let mut suspend = crate::tui::suspend_terminal();
    compose_with(chat, command.as_deref());
    suspend.resume();
    chat.force_redraw = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `$VISUAL` first, `$EDITOR` second, whitespace is not an editor.
    #[test]
    fn visual_outranks_editor_and_blanks_are_unset() {
        assert_eq!(
            editor_command(Some("vim"), Some("nano")),
            Some("vim".to_string())
        );
        assert_eq!(
            editor_command(None, Some("nano")),
            Some("nano".to_string()),
            "falls through to $EDITOR"
        );
        assert_eq!(
            editor_command(Some("  "), Some("nano")),
            Some("nano".to_string()),
            "a blank $VISUAL is not a choice"
        );
        assert_eq!(editor_command(None, None), None);
        assert_eq!(editor_command(Some(""), Some(" ")), None);
    }

    #[test]
    fn a_command_may_carry_flags() {
        let (program, args) = split_command("code -w --new-window").expect("split");
        assert_eq!(program, "code");
        assert_eq!(args, vec!["-w", "--new-window"]);
        assert!(split_command("   ").is_none(), "nothing to run");
    }

    /// One trailing newline goes (editors add it); everything else stays,
    /// including the blank line a user deliberately left in the middle.
    #[test]
    fn only_the_editors_own_trailing_newline_comes_off() {
        assert_eq!(trim_trailing_newline("fix a bug\n"), "fix a bug");
        assert_eq!(trim_trailing_newline("fix a bug\r\n"), "fix a bug");
        assert_eq!(trim_trailing_newline("a\n\nb"), "a\n\nb");
        assert_eq!(trim_trailing_newline("a\n\n"), "a\n", "only one");
        assert_eq!(trim_trailing_newline(""), "");
    }

    #[test]
    fn every_outcome_but_a_successful_edit_says_something() {
        assert_eq!(EditorOutcome::Edited("x".into()).note(), None);
        assert_eq!(
            EditorOutcome::Unset.note().as_deref(),
            Some(NO_EDITOR_HINT),
            "the hint names the variable to set"
        );
        assert_eq!(
            EditorOutcome::Kept.note().as_deref(),
            Some(EDITOR_KEPT_HINT)
        );
        assert!(
            EditorOutcome::Failed("no such file".into())
                .note()
                .is_some_and(|n| n.contains("no such file")),
            "the reason reaches the user"
        );
    }

    /// Consecutive kills in the same direction rebuild one entry in *text*
    /// order; a direction change starts a new one.
    #[test]
    fn same_direction_kills_coalesce() {
        let mut composer = Composer::default();
        composer.tick();
        composer.kill("tui/".into(), KillDir::Back);
        composer.tick();
        composer.kill("src/".into(), KillDir::Back);
        assert_eq!(composer.top(), Some("src/tui/"), "prepended, not appended");
        assert_eq!(composer.ring_len(), 1);

        composer.tick();
        composer.kill("a".into(), KillDir::Forward);
        assert_eq!(composer.ring_len(), 2, "the other direction is a new entry");
        composer.tick();
        composer.kill("b".into(), KillDir::Forward);
        assert_eq!(composer.top(), Some("ab"), "forward kills append");
        assert_eq!(composer.ring_len(), 2);

        // A key in between breaks the chain even in the same direction.
        composer.tick();
        composer.tick();
        composer.kill("c".into(), KillDir::Forward);
        assert_eq!(composer.ring_len(), 3);
        assert_eq!(composer.top(), Some("c"));
    }

    #[test]
    fn empty_kills_never_enter_the_ring() {
        let mut composer = Composer::default();
        composer.tick();
        composer.kill(String::new(), KillDir::Back);
        assert_eq!(composer.ring_len(), 0);
        assert_eq!(composer.top(), None);
    }

    #[test]
    fn the_ring_is_bounded() {
        let mut composer = Composer::default();
        for i in 0..KILL_RING_MAX + 5 {
            // Two ticks apart so nothing coalesces.
            composer.tick();
            composer.tick();
            composer.kill(format!("k{i}"), KillDir::Back);
        }
        assert_eq!(composer.ring_len(), KILL_RING_MAX);
        assert_eq!(composer.top(), Some("k14"), "newest in front");
    }

    /// Yank-pop walks the ring and wraps; it is only ever available on the key
    /// straight after a yank.
    #[test]
    fn yank_pop_rotates_only_right_after_a_yank() {
        let mut composer = Composer::default();
        for text in ["oldest", "middle", "newest"] {
            composer.tick();
            composer.tick();
            composer.kill(text.into(), KillDir::Back);
        }
        assert!(composer.yank_pop().is_none(), "no yank to pop");

        composer.tick();
        composer.note_yank(4, "newest".len());
        composer.tick();
        assert_eq!(
            composer.yank_pop(),
            Some((4, 6, "middle".to_string())),
            "replaces the span the yank filled"
        );
        composer.tick();
        assert_eq!(composer.yank_pop(), Some((4, 6, "oldest".to_string())));
        composer.tick();
        assert_eq!(
            composer.yank_pop(),
            Some((4, 6, "newest".to_string())),
            "wraps"
        );

        // One unrelated key and the rotation is over.
        composer.tick();
        composer.tick();
        assert!(composer.yank_pop().is_none());
    }

    #[test]
    fn a_single_entry_ring_has_nothing_to_pop_to() {
        let mut composer = Composer::default();
        composer.tick();
        composer.kill("only".into(), KillDir::Back);
        composer.tick();
        composer.note_yank(0, 4);
        composer.tick();
        assert!(composer.yank_pop().is_none());
    }

    #[test]
    fn the_chord_expires_and_is_single_use() {
        let mut composer = Composer::default();
        let now = Instant::now();
        composer.arm_chord(now);
        assert!(composer.take_chord(now + Duration::from_millis(200)));
        assert!(!composer.take_chord(now), "consumed by the first key");

        composer.arm_chord(now);
        assert!(
            !composer.take_chord(now + CHORD_WINDOW + Duration::from_millis(1)),
            "a chord the user walked away from is not a chord"
        );
    }
}

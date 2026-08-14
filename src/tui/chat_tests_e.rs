//! Chat state-machine tests, part five: the composer's power tools (D86) —
//! the `$EDITOR` round trip and its chord, the readline motions and kill ring,
//! and the paste-burst hardening.
//!
//! `chat_tests_a` / `b` / `c` / `d` split by size alone (the 4000-line file
//! cap); this file continues them.

use super::tests_a::*;
use super::*;
use crate::tui::composer::{self, EditorOutcome, KILL_RING_MAX, NO_EDITOR_HINT};

fn alt(chat: &mut Chat, c: char) -> bool {
    chat.on_key_at(KeyCode::Char(c), KeyModifiers::ALT, key_time())
}

fn shift(chat: &mut Chat, code: KeyCode) -> bool {
    chat.on_key_at(code, KeyModifiers::SHIFT, key_time())
}

/// The info tier the composer writes its editor notes to.
fn info(chat: &Chat) -> String {
    chat.slash_info_lines.join("\n")
}

/// A directory this test owns: pid-tagged, created here, and removed here —
/// only ever this path.
fn scratch(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("bingo-d86-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    root
}

/// A fake `$EDITOR`: a script that appends `line` to the file it is handed and
/// exits with `code`. Unix only — a portable executable stand-in would have to
/// be a compiled helper binary, and the round trip it would prove is the one
/// [`editor_round_trip_replaces_the_draft`] already proves here.
#[cfg(unix)]
fn fake_editor(root: &std::path::Path, name: &str, line: &str, code: i32) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join(name);
    let script = format!("#!/bin/sh\nprintf '\\n%s\\n' '{line}' >> \"$1\"\nexit {code}\n");
    let _ = std::fs::write(&path, script);
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path.to_string_lossy().to_string()
}

/// A saved edit replaces the draft, is one undo step, and says nothing — the
/// new text on screen is the feedback.
#[cfg(unix)]
#[test]
fn editor_round_trip_replaces_the_draft() {
    let root = scratch("editor-ok");
    let editor = fake_editor(&root, "ok.sh", "second line", 0);
    let mut chat = test_chat();
    chat.set_input("first line");

    composer::compose_with(&mut chat, Some(&editor));
    assert_eq!(
        chat.input, "first line\nsecond line",
        "the file's content comes back, trailing newline trimmed"
    );
    assert_eq!(chat.cursor, chat.input.len(), "caret at the end");
    assert!(info(&chat).is_empty(), "a successful edit is silent");

    // One undo step: ctrl+_ returns to what was typed before the editor opened.
    chat.undo_edit();
    assert_eq!(chat.input, "first line");
    let _ = std::fs::remove_dir_all(&root);
}

/// A non-zero exit keeps the draft. The editor may well have been abandoned
/// deliberately, and throwing the prompt away for it would be the one
/// unrecoverable outcome.
#[cfg(unix)]
#[test]
fn a_failed_editor_keeps_the_draft_and_says_so() {
    let root = scratch("editor-fail");
    let editor = fake_editor(&root, "fail.sh", "ignored", 1);
    let mut chat = test_chat();
    chat.set_input("keep me");

    composer::compose_with(&mut chat, Some(&editor));
    assert_eq!(chat.input, "keep me", "the draft stands");
    assert!(
        info(&chat).contains("unchanged"),
        "and the info tier says why: {}",
        info(&chat)
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// An editor that cannot be run at all is not silence either.
#[test]
fn an_unrunnable_editor_reports_itself() {
    let mut chat = test_chat();
    chat.set_input("draft");
    composer::compose_with(&mut chat, Some("bingo-no-such-editor-d86"));
    assert_eq!(chat.input, "draft");
    assert!(
        info(&chat).contains("could not run the editor"),
        "{}",
        info(&chat)
    );
}

/// With neither variable set the key is not a dead press: it names the
/// variable that would make it work.
#[test]
fn without_an_editor_the_key_says_what_to_set() {
    let mut chat = test_chat();
    chat.set_input("draft");
    composer::compose_with(&mut chat, None);
    assert_eq!(chat.input, "draft", "nothing is touched");
    assert_eq!(info(&chat), NO_EDITOR_HINT);
    assert!(
        info(&chat).contains("$EDITOR"),
        "the hint names the variable"
    );
}

/// Ctrl+G asks the host for the round trip; a pending question outranks it,
/// for the same reason ctrl+o is inert while one is up.
#[test]
fn ctrl_g_requests_the_editor_unless_a_dialog_is_up() {
    let mut chat = test_chat();
    assert!(ctrl(&mut chat, 'g'));
    assert!(chat.open_editor, "the host is asked to open the editor");

    chat.open_editor = false;
    let (tx, _rx) = oneshot::channel();
    chat.pending_ask = Some((
        PermissionRequest::new(
            "Bash",
            "Allow running Bash?",
            vec!["Yes".into(), "No".into()],
        ),
        tx,
    ));
    assert!(ctrl(&mut chat, 'g'));
    assert!(
        !chat.open_editor,
        "a question blocking a turn keeps its keys"
    );
}

/// `ctrl+x ctrl+e` is the same door. The chord is armed for exactly one key:
/// anything else clears it *and* still does its own job.
#[test]
fn the_editor_chord_needs_both_keys_in_a_row() {
    let mut chat = test_chat();
    assert!(ctrl(&mut chat, 'x'), "ctrl+x is consumed by the chord");
    assert!(!chat.open_editor, "and does nothing on its own");
    assert!(ctrl(&mut chat, 'e'));
    assert!(chat.open_editor, "ctrl+e completes it");

    // ctrl+x then something else: no editor, and the other key acted.
    chat.open_editor = false;
    chat.set_input("hello world");
    chat.cursor = chat.input.len();
    assert!(ctrl(&mut chat, 'x'));
    assert!(ctrl(&mut chat, 'a'), "ctrl+a still moves the caret");
    assert!(!chat.open_editor, "the chord did not fire");
    assert_eq!(chat.cursor, 0, "and ctrl+a did its own job");

    // A plain key clears it too, not just the control keys.
    assert!(ctrl(&mut chat, 'x'));
    press(&mut chat, KeyCode::Char('z'));
    assert!(ctrl(&mut chat, 'e'));
    assert!(!chat.open_editor, "an intervening key ends the chord");
    assert_eq!(
        chat.cursor,
        chat.input.len(),
        "and this ctrl+e was end-of-line again"
    );
}

/// Shift+Enter is a newline, not a send. It arrives as its own key only where
/// the kitty keyboard protocol is active (the push at setup, D86); the
/// composer's half is that it inserts rather than submits.
#[test]
fn shift_enter_inserts_a_newline() {
    let mut chat = test_chat();
    type_text(&mut chat, "one");
    assert!(shift(&mut chat, KeyCode::Enter));
    type_text(&mut chat, "two");
    assert_eq!(chat.input, "one\ntwo", "a newline, in place");
    assert!(chat.queued.is_empty(), "and nothing was sent");
    assert!(!chat.busy, "no turn started");
}

/// Paste-burst detection needs consecutive keys under the 10ms threshold; the
/// shared `key_time` clock deliberately steps 50ms, so a burst needs its own.
struct Burst {
    at: std::time::Instant,
}

impl Burst {
    fn new() -> Self {
        Self {
            at: std::time::Instant::now(),
        }
    }

    fn key(&mut self, chat: &mut Chat, code: KeyCode) -> bool {
        self.at += std::time::Duration::from_millis(1);
        chat.on_key_at(code, KeyModifiers::empty(), self.at)
    }

    fn text(&mut self, chat: &mut Chat, text: &str) {
        for c in text.chars() {
            let code = if c == '\n' {
                KeyCode::Enter
            } else {
                KeyCode::Char(c)
            };
            self.key(chat, code);
        }
    }
}

/// A paste that arrives as a key burst lands as text: its Enters are newlines,
/// nothing is submitted, and the `@` and `/` in it open no dropdown — the
/// mention popup would otherwise take the very Enter the rest of the paste
/// needs, and turn one paste into a half-sent message.
#[test]
fn a_key_burst_lands_as_text_and_opens_nothing() {
    let mut chat = test_chat();
    let mut burst = Burst::new();
    burst.text(
        &mut chat,
        "review the diff\n@src/tui/chat.rs\n/model please\nthanks",
    );

    assert_eq!(
        chat.input, "review the diff\n@src/tui/chat.rs\n/model please\nthanks",
        "every Enter in the burst is a literal newline"
    );
    assert!(chat.queued.is_empty(), "nothing was submitted");
    assert!(!chat.busy);
    assert!(chat.mention.is_none(), "no `@` dropdown mid-paste");
    assert!(
        chat.slash_suggestions.is_empty(),
        "and no slash dropdown either"
    );
}

/// `!` and `?` are commands to the composer only when a person pressed them on
/// an empty prompt, so once a burst is recognised they are ordinary characters.
///
/// The heuristic cannot see a paste's first [`PASTE_BURST_KEYS`] characters —
/// they are indistinguishable from typing, which is exactly what bracketed
/// paste exists to fix and why it is the primary path. The keys that leave the
/// prompt empty are what make the guard reachable at all.
#[test]
fn a_recognised_burst_does_not_trip_the_empty_input_keys() {
    let mut chat = test_chat();
    let mut burst = Burst::new();
    for _ in 0..crate::tui::chat::PASTE_BURST_KEYS + 1 {
        burst.key(&mut chat, KeyCode::Backspace);
    }
    assert_eq!(chat.input, "", "backspace on an empty prompt is a no-op");

    burst.key(&mut chat, KeyCode::Char('!'));
    assert!(!chat.bash_mode, "a pasted `!` is not shell mode");
    burst.key(&mut chat, KeyCode::Char('?'));
    assert!(!chat.help_visible, "a pasted `?` is not the panel");
    assert_eq!(chat.input, "!?", "both are characters");
}

/// The end of a burst is only observable from the next event, so that is where
/// the completion surfaces are reconsidered — once, on the first real keystroke.
#[test]
fn the_first_key_after_a_burst_re_evaluates() {
    let mut chat = test_chat();
    let mut burst = Burst::new();
    burst.text(&mut chat, "aaaaaaaa/mod");
    assert!(chat.slash_suggestions.is_empty(), "suppressed during");

    // The pasted text does not start with `/`, so the dropdown stays shut on
    // its own merits; a fresh `/` prompt is what proves the funnel is live again.
    chat.set_input("");
    press(&mut chat, KeyCode::Char('/'));
    assert!(
        !chat.slash_suggestions.is_empty(),
        "typing after the burst completes again"
    );
}

/// Bracketed paste was already one edit rather than one per character; what
/// D86 adds is that it opens no dropdown either.
#[test]
fn a_bracketed_paste_opens_nothing() {
    let mut chat = test_chat();
    chat.on_paste("@src/tui/chat.rs");
    assert_eq!(chat.input, "@src/tui/chat.rs");
    assert!(chat.mention.is_none(), "a pasted `@` is a character");

    let mut chat = test_chat();
    chat.on_paste("/model gpt-5.6-sol");
    assert_eq!(chat.input, "/model gpt-5.6-sol");
    assert!(
        chat.slash_suggestions.is_empty(),
        "a pasted `/` is a character"
    );
    assert!(!chat.pasting, "the flag does not outlive the paste");
}

/// Alt+B/F walk a path segment at a time, which is the point: a mistyped
/// segment can be reached without retyping the whole path.
#[test]
fn word_motion_walks_path_segments() {
    let mut chat = test_chat();
    chat.set_input("open src/tui/chat_tail.rs now");
    chat.cursor = chat.input.len();

    alt(&mut chat, 'b');
    assert_eq!(&chat.input[chat.cursor..], "now");
    alt(&mut chat, 'b');
    assert_eq!(&chat.input[chat.cursor..], "rs now");
    alt(&mut chat, 'b');
    assert_eq!(&chat.input[chat.cursor..], "tail.rs now");
    alt(&mut chat, 'b');
    assert_eq!(&chat.input[chat.cursor..], "chat_tail.rs now");

    alt(&mut chat, 'f');
    assert_eq!(
        &chat.input[chat.cursor..],
        "_tail.rs now",
        "forward mirrors"
    );
}

/// The four kills feed one ring, and consecutive kills in the same direction
/// come back as one span in the order they were typed.
#[test]
fn kills_feed_the_ring_and_coalesce() {
    let mut chat = test_chat();
    chat.set_input("alpha beta gamma");
    chat.cursor = chat.input.len();

    ctrl(&mut chat, 'w');
    ctrl(&mut chat, 'w');
    assert_eq!(chat.input, "alpha ");
    assert_eq!(
        chat.composer.ring_len(),
        1,
        "two kills in a row are one entry"
    );

    ctrl(&mut chat, 'y');
    assert_eq!(
        chat.input, "alpha beta gamma",
        "yanked back in text order, not press order"
    );
}

/// Alt+Backspace and Alt+D are the sub-word kills; ctrl+w keeps the whole
/// whitespace token, exactly as a shell does.
#[test]
fn the_sub_word_kills_stop_inside_a_path() {
    let mut chat = test_chat();
    chat.set_input("edit src/tui/chat.rs");
    chat.cursor = chat.input.len();
    chat.on_key_at(KeyCode::Backspace, KeyModifiers::ALT, key_time());
    assert_eq!(
        chat.input, "edit src/tui/chat.",
        "one segment, not the path"
    );

    let mut chat = test_chat();
    chat.set_input("edit src/tui/chat.rs");
    chat.cursor = chat.input.len();
    ctrl(&mut chat, 'w');
    assert_eq!(chat.input, "edit ", "ctrl+w takes the whole token");

    let mut chat = test_chat();
    chat.set_input("src/tui/chat.rs");
    chat.cursor = 0;
    alt(&mut chat, 'd');
    assert_eq!(chat.input, "/tui/chat.rs", "alt+d kills forward");
    assert_eq!(chat.cursor, 0);
}

/// Ctrl+Y inserts the newest kill; Alt+Y immediately after rotates the ring
/// over exactly what was just inserted, and wraps.
#[test]
fn yank_and_yank_pop_walk_the_ring() {
    let mut chat = test_chat();
    chat.set_input("one two three");
    chat.cursor = chat.input.len();

    // ctrl+e between the kills breaks the coalescing chain without editing.
    ctrl(&mut chat, 'w');
    ctrl(&mut chat, 'e');
    ctrl(&mut chat, 'w');
    ctrl(&mut chat, 'e');
    ctrl(&mut chat, 'w');
    assert_eq!(chat.input, "");
    assert_eq!(chat.composer.ring_len(), 3);

    ctrl(&mut chat, 'y');
    assert_eq!(chat.input, "one ", "the newest kill");
    alt(&mut chat, 'y');
    assert_eq!(chat.input, "two ", "replaced in place");
    alt(&mut chat, 'y');
    assert_eq!(chat.input, "three");
    alt(&mut chat, 'y');
    assert_eq!(chat.input, "one ", "and wraps");
    assert_eq!(chat.cursor, chat.input.len());
}

/// Alt+Y is a binding only in the moment after a yank. Anywhere else it does
/// nothing at all rather than inserting something the user did not ask for.
#[test]
fn yank_pop_out_of_context_does_nothing() {
    let mut chat = test_chat();
    chat.set_input("text");
    chat.cursor = chat.input.len();
    ctrl(&mut chat, 'w');
    ctrl(&mut chat, 'y');
    press(&mut chat, KeyCode::Char('!'));
    let before = chat.input.clone();
    assert!(!alt(&mut chat, 'y'), "not consumed");
    assert_eq!(chat.input, before, "and nothing changed");
}

/// The ring is bounded: a long editing session cannot grow it without limit.
#[test]
fn the_ring_stays_bounded_through_the_keys() {
    let mut chat = test_chat();
    for i in 0..KILL_RING_MAX + 4 {
        chat.set_input(format!("word{i}"));
        chat.cursor = chat.input.len();
        ctrl(&mut chat, 'w');
        ctrl(&mut chat, 'e');
    }
    assert_eq!(chat.composer.ring_len(), KILL_RING_MAX);
}

/// Ctrl+P/Ctrl+N are ↑/↓, routed through the same function — including D83's
/// rule that a queued message the turn has already taken is not pulled back.
#[test]
fn ctrl_p_and_ctrl_n_mirror_the_arrows() {
    let mut chat = chat_with_history("d86-history");
    chat.record_history("first prompt");
    chat.record_history("second prompt");

    assert!(ctrl(&mut chat, 'p'));
    assert_eq!(chat.input, "second prompt", "ctrl+p is ↑");
    assert!(ctrl(&mut chat, 'p'));
    assert_eq!(chat.input, "first prompt");
    assert!(ctrl(&mut chat, 'n'));
    assert_eq!(chat.input, "second prompt", "ctrl+n is ↓");
}

/// The queue pull-back reaches ctrl+p because it is the same function, and it
/// loses the same race to the turn (D83).
#[test]
fn ctrl_p_pulls_back_a_queued_message_and_loses_the_same_race() {
    let mut chat = chat_with_history("d86-pullback");
    chat.busy = true;
    chat.set_input("steer me");
    chat.submit();
    assert_eq!(chat.queued.len(), 1);

    assert!(ctrl(&mut chat, 'p'));
    assert_eq!(
        chat.input, "steer me",
        "ctrl+p pulls it back into the composer"
    );
    assert!(chat.queued.is_empty());

    // And the race: a message the turn already took stays taken.
    chat.set_input("too late");
    chat.submit();
    let taken = chat.steer.take();
    assert_eq!(taken.len(), 1);
    assert!(ctrl(&mut chat, 'p'));
    assert_eq!(chat.input, "", "the composer is left alone");
    assert_eq!(chat.queued.len(), 1, "the queue waits for the event");
}

/// The outcome type carries its own copy, so the host and the tests cannot
/// disagree about what the user is told.
#[test]
fn a_saved_edit_is_the_only_silent_outcome() {
    assert!(EditorOutcome::Edited("x".into()).note().is_none());
    for outcome in [
        EditorOutcome::Kept,
        EditorOutcome::Unset,
        EditorOutcome::Failed("boom".into()),
    ] {
        assert!(outcome.note().is_some(), "{outcome:?} says something");
    }
}

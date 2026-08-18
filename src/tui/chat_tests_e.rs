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

/// A fake `$EDITOR` that saves the way vim's default `backupcopy=auto` does:
/// write a sibling file, then rename it over the path. The path keeps its name
/// and loses its inode, which is exactly what a read-back holding onto the old
/// file would miss.
#[cfg(unix)]
fn renaming_editor(root: &std::path::Path, name: &str, line: &str, code: i32) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join(name);
    let script = format!(
        "#!/bin/sh\ncp \"$1\" \"$1.new\"\nprintf '\\n%s\\n' '{line}' >> \"$1.new\"\nmv -f \"$1.new\" \"$1\"\nexit {code}\n"
    );
    let _ = std::fs::write(&path, script);
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path.to_string_lossy().to_string()
}

/// D93/6: the round trip follows the *path*, not the file that was there when
/// the draft was written. An editor that saves by renaming a new file over the
/// path is the common case (vim, and every "atomic save" implementation), and
/// its edit has to come back exactly like an in-place one's.
#[cfg(unix)]
#[test]
fn a_rename_style_editor_round_trips_like_an_in_place_one() {
    let root = scratch("editor-rename");
    let editor = renaming_editor(&root, "rename.sh", "second line", 0);
    let mut chat = test_chat();
    chat.set_input("first line");

    composer::compose_with(&mut chat, Some(&editor));
    assert_eq!(
        chat.input, "first line\nsecond line",
        "a renamed-over file is the edit, not a stale inode"
    );
    assert!(info(&chat).is_empty(), "a successful edit is silent");
    let _ = std::fs::remove_dir_all(&root);
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

/// D93, the way this round trip actually loses work: an editor that opens a
/// window and returns straight away.
///
/// `code`/`zed`/`subl` without their wait flag exit zero before the user has
/// typed anything, so the file is read back untouched and removed — and the
/// edit they go on to save has nowhere left to land. It used to be reported as
/// a successful edit, which is to say as silence: "I edited it and bingo threw
/// it away", with nothing on screen saying so. Now it says so.
#[cfg(unix)]
#[test]
fn an_editor_that_saves_nothing_no_longer_passes_for_a_saved_edit() {
    use std::os::unix::fs::PermissionsExt;
    let root = scratch("editor-detached");
    let path = root.join("windowed.sh");
    // Exits zero, having written nothing — which is what a windowed editor
    // looks like from here at the moment the file is read back.
    let _ = std::fs::write(&path, "#!/bin/sh\nexit 0\n");
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    let editor = path.to_string_lossy().to_string();

    let mut chat = test_chat();
    chat.set_input("keep me");
    composer::compose_with(&mut chat, Some(&editor));
    assert_eq!(chat.input, "keep me", "the draft stands");
    assert_eq!(
        info(&chat),
        composer::EDITOR_UNCHANGED_HINT,
        "and the outcome is stated rather than swallowed"
    );
    assert!(
        info(&chat).contains("wait flag"),
        "the note names the cure: {}",
        info(&chat)
    );
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
    chat.stub_ask(PermissionRequest::new(
        "Bash",
        "Allow running Bash?",
        vec!["Yes".into(), "No".into()],
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
    assert!(chat.main_queue().is_empty(), "and nothing was sent");
    assert!(!chat.conv.busy, "no turn started");
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
    assert!(chat.main_queue().is_empty(), "nothing was submitted");
    assert!(!chat.conv.busy);
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
    chat.conv.busy = true;
    chat.set_input("steer me");
    chat.submit();
    assert_eq!(chat.main_queue().len(), 1);

    assert!(ctrl(&mut chat, 'p'));
    assert_eq!(
        chat.input, "steer me",
        "ctrl+p pulls it back into the composer"
    );
    assert!(chat.main_queue().is_empty());

    // And the race: a message the turn already took stays taken.
    chat.set_input("too late");
    chat.submit();
    assert_eq!(chat.take_steering().len(), 1);
    assert!(ctrl(&mut chat, 'p'));
    assert_eq!(chat.input, "", "the composer is left alone");
    assert!(
        chat.main_queue().is_empty(),
        "the barrier took it, and the pull-back changed nothing"
    );
}

/// The outcome type carries its own copy, so the host and the tests cannot
/// disagree about what the user is told.
#[test]
fn a_saved_edit_is_the_only_silent_outcome() {
    assert!(EditorOutcome::Edited("x".into()).note().is_none());
    for outcome in [
        EditorOutcome::Unchanged,
        EditorOutcome::Kept,
        EditorOutcome::Unset,
        EditorOutcome::Failed("boom".into()),
    ] {
        assert!(outcome.note().is_some(), "{outcome:?} says something");
    }
}

// -- D92: theme, highlighting, diff gutter -------------------------------

/// The gutter is a property of the shared diff builder, so it has to show up on
/// both diff surfaces at once — the approval preview and the completed-edit
/// rows. If these two ever disagree, someone has grown a second diff renderer.
#[test]
fn both_diff_surfaces_render_the_same_gutter() {
    const DIFF: &str = "--- a/f.rs\n+++ b/f.rs\n@@ -3,3 +3,3 @@\n keep\n-gone\n+new\n";

    // Surface 1: the pre-approval preview inside the permission dialog.
    let mut chat = test_chat();
    chat.stub_ask(crate::ui::PermissionRequest {
        title: "Edit file".into(),
        question: "Make this edit?".into(),
        options: vec!["Yes".into()],
        descriptions: vec![None],
        free_text: false,
        kind: crate::ui::AskKind::Permission,
        preview: Some(crate::ui::AskPreview::Diff(DIFF.to_string())),
        scope: None,
    });
    let dialog = visible(&mut chat, 120, 40);
    assert!(dialog.contains("3 3  keep"), "preview gutter: {dialog}");
    assert!(dialog.contains("4   -gone"), "preview removal: {dialog}");
    assert!(dialog.contains("  4 +new"), "preview addition: {dialog}");

    // Surface 2: the completed-edit rows in the flow.
    let mut chat = test_chat();
    chat.conv.busy = true;
    chat.conv.messages.push(msg(Role::Assistant, ""));
    chat.conv.stream_msg = Some(0);
    chat.events.send(UiEvent::ToolStart {
        name: "Edit".into(),
    });
    chat.drain_events();
    chat.events.send(UiEvent::ToolReady {
        tool_call_id: "edit-1".into(),
        name: "Edit".into(),
        input: serde_json::json!({ "file_path": "f.rs" }),
        standalone: false,
    });
    chat.drain_events();
    chat.events
        .send(UiEvent::ToolDone(crate::query::ToolCallDone {
            tool_call_id: "edit-1".into(),
            name: "Edit".into(),
            summary: "f.rs".into(),
            output: String::new(),
            status: crate::query::ToolCallStatus::Done,
            diff: Some(DIFF.to_string()),
            duration_ms: 4,
        }));
    chat.drain_events();
    if let Some(activity) = chat
        .conv
        .messages
        .iter_mut()
        .flat_map(|m| &mut m.activities)
        .next()
    {
        activity.expanded = true;
    }
    let flow = visible(&mut chat, 120, 40);
    assert!(flow.contains("3 3  keep"), "edit-row gutter: {flow}");
    assert!(flow.contains("4   -gone"), "edit-row removal: {flow}");
    assert!(flow.contains("  4 +new"), "edit-row addition: {flow}");
}

/// `/theme` has to reach the diff rows too. They are baked when the edit lands,
/// so a switch that only rebuilt the markdown cache would recolour the prose and
/// leave every diff on the old palette.
#[test]
fn switching_theme_recolours_baked_diff_rows() {
    let mut chat = test_chat();
    chat.conv.busy = true;
    chat.conv.messages.push(msg(Role::Assistant, ""));
    chat.conv.stream_msg = Some(0);
    chat.events.send(UiEvent::ToolStart {
        name: "Edit".into(),
    });
    chat.drain_events();
    chat.events.send(UiEvent::ToolReady {
        tool_call_id: "edit-1".into(),
        name: "Edit".into(),
        input: serde_json::json!({ "file_path": "f.rs" }),
        standalone: false,
    });
    chat.drain_events();
    chat.events
        .send(UiEvent::ToolDone(crate::query::ToolCallDone {
            tool_call_id: "edit-1".into(),
            name: "Edit".into(),
            summary: "f.rs".into(),
            output: String::new(),
            status: crate::query::ToolCallStatus::Done,
            diff: Some("+++ b/f.rs\n@@ -1,1 +1,1 @@\n+added\n".to_string()),
            duration_ms: 4,
        }));
    chat.drain_events();

    let gutter_color = |chat: &Chat| {
        chat.conv
            .messages
            .iter()
            .flat_map(|m| &m.activities)
            // Row 0 is the `@@` header; the gutter starts on the one after it.
            .flat_map(|a| a.content.iter().skip(1))
            .find_map(|line| line.segs.first().map(|s| s.style.fg))
    };
    chat.run_slash("theme light");
    let light = gutter_color(&chat);
    assert_eq!(
        light,
        Some(Some(chat.theme.text_muted)),
        "the gutter follows the live theme"
    );
    chat.run_slash("theme dark");
    let dark = gutter_color(&chat);
    assert_eq!(dark, Some(Some(chat.theme.text_muted)));
    assert_ne!(light, dark, "the two themes really do differ");
}

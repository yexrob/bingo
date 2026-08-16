//! Chat state-machine tests, part six: what the console does and does not print
//! (D94's delivery rerouting, D98's quiet console).
//!
//! Five parts. The first pins what main *stopped* printing: an agent's spawn
//! and completion used to hang a `◉ name · task` row off whatever assistant
//! message happened to be last, and now they do not — while the signal they
//! carried still lands on the agent's own row in the background dialog and in
//! the accounting store. The second is the one line that still comes through,
//! and the reason it does: a failure cannot depend on the main agent choosing
//! to narrate it. The third is the digest debounce, which turns a burst of mail
//! into one turn. The fourth is what a woken turn does when it ends: since D103
//! retired D102's silence contract, exactly what a typed turn does. The fifth
//! is the tiering D106 gave the transcript — the dispatch row and its live
//! progress, the settled cost, the `●` notice and the `@name❯` line.

use super::tests_a::*;
use super::*;

use crate::agents::AgentKind;
use crate::api::types::{ContentBlock, Message};
use crate::tui::buffer::BufferId;
use crate::tui::notify::{Notifier, NotifyChannel, TerminalEnv};
use crate::tui::test_util::chat_at;
use crate::watch::{WatchKind, WatchState};

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn assistant(text: &str) -> Message {
    Message {
        role: crate::api::types::Role::Assistant,
        content: vec![ContentBlock::Text {
            text: text.to_string(),
        }],
    }
}

/// Register a hire on the chat's own session, the way a `Task` spawn would.
fn seed_agent(chat: &Chat, name: &str) {
    chat.session.agents.insert(
        name,
        AgentKind::Hire,
        None,
        "test instance".to_string(),
        chat.session.clone(),
    );
}

fn lifecycle(label: &str, status: WatchState, detail: Option<&str>) -> UiEvent {
    UiEvent::WatchEvent {
        label: label.to_string(),
        kind: WatchKind::Agent,
        status,
        detail: detail.map(str::to_string),
        duration_ms: 0,
        payload: None,
        signal: None,
        notifies_main: false,
        dispatch: true,
    }
}

/// Main as the user sees it: every rendered row, flattened.
fn main_rows(chat: &mut Chat) -> Vec<String> {
    chat.build_rows(80);
    chat.doc
        .rows
        .iter()
        .map(|r| r.line.plain_text().trim_end().to_string())
        .collect()
}

/// A chat wired to the bell channel, its startup title already drained.
fn chat_with_bell() -> Chat {
    let mut chat = test_chat();
    chat.set_notifier(Notifier::new(NotifyChannel::Bell, &TerminalEnv::default()));
    let _ = chat.notify.take();
    chat
}

fn emitted(chat: &mut Chat) -> String {
    String::from_utf8_lossy(&chat.notify.take()).to_string()
}

// ---------------------------------------------------------------------------
// A. main stops being the bus
// ---------------------------------------------------------------------------

/// The inventory this batch was written against: a spawn and a completion
/// arriving while main is idle used to add a `◉ scout · fix the parser` row
/// with a `⎿ done` under it, stapled to the last assistant message — a reply
/// that had nothing to do with either event.
#[test]
fn a_spawn_and_a_completion_add_no_rows_to_an_idle_console() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::User, "have someone look"));
    chat.messages
        .push(msg(Role::Assistant, "I have asked scout"));
    assert!(
        chat.stream_msg.is_none(),
        "the turn is over: what arrives now is the bus, not the conversation"
    );
    let before = main_rows(&mut chat);

    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Running,
        None,
    ));
    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Done,
        Some("done"),
    ));

    assert_eq!(
        main_rows(&mut chat),
        before,
        "main flow is byte-identical: no spawn row, no completion row"
    );
    assert!(
        chat.messages.iter().all(|m| m.activities.is_empty()),
        "and nothing was hung off an older reply"
    );
}

/// The signal is rerouted, not dropped — and D107 is where the reroute ends.
///
/// D94 stopped printing a main-idle lifecycle event in the console and D95
/// filed it in a team feed instead; this pinned the filing. D107 retired that
/// feed with the directory column that was its only reader, so the surviving
/// destination is the one the user actually reads: the instance's own row in
/// the background dialog, where a finished run is what the row says it is.
/// The console still prints nothing, which is the half of the claim that was
/// always the point.
#[test]
fn the_lifecycle_signal_reaches_the_dialog_and_not_the_console() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    seed_agent(&chat, "scout");
    chat.refresh_conversations();

    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Running,
        None,
    ));
    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Done,
        Some("fixed the parser"),
    ));

    assert!(
        chat.messages.iter().all(|m| m.activities.is_empty()),
        "nothing was hung off a reply that had nothing to do with it"
    );
    chat.open_background_dialog();
    let rows: Vec<String> = chat.dialog_rows().iter().map(|row| row.text()).collect();
    assert!(
        rows.iter().any(|row| row.contains("@scout")),
        "the instance is on the dialog's roster: {rows:?}"
    );
}

/// The other half of the reroute: a completed agent's report is in its own
/// conversation, and the accounting says so with an unread count (drawn in the
/// background dialog since D107). Nothing new was built for this — the count
/// follows the instance's history, and the lifecycle event's own registry sweep
/// is what re-reads it — so the test exists to pin that the chain holds end to
/// end now that main prints nothing.
#[test]
fn a_completion_bumps_the_dm_instead_of_main() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    seed_agent(&chat, "scout");
    chat.refresh_conversations();

    let dm = BufferId::Dm("scout".to_string());
    assert_eq!(
        chat.buffers
            .get(&dm)
            .map(crate::tui::buffer::Buffer::unread),
        Some(0),
        "a conversation seen for the first time starts read"
    );
    let before = main_rows(&mut chat);

    // The agent finishes: its reply lands in its history, then the lifecycle
    // event arrives — the order the domain actually produces.
    chat.session.agents.finish(
        "scout",
        vec![
            // A DM the user sent, then the reply to it: the badge counts the
            // pair lane (D99), and an agent's report on the task main gave it
            // is main's news rather than the user's.
            crate::api::types::Message::user_text(format!(
                "{}\nhow is the parser?",
                crate::tool::agent::DM_FROM_USER_MARKER
            )),
            assistant("the parser is fixed"),
        ],
        0,
    );
    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Done,
        Some("done"),
    ));

    assert_eq!(
        main_rows(&mut chat),
        before,
        "main is untouched by someone else's completion"
    );
    assert_eq!(
        chat.buffers
            .get(&dm)
            .map(crate::tui::buffer::Buffer::unread),
        Some(2),
        "the report is in the DM, and the DM is what carries the badge"
    );
    assert_eq!(
        chat.buffers
            .get(&dm)
            .map(crate::tui::buffer::Buffer::mention),
        Some(true),
        "a DM is addressed to the user by construction"
    );
}

/// The exception, and the reason the rule is about *when* rather than *what*:
/// `Agent` is a hidden tool, so this watch row is the only row the Task call the
/// user just watched the model make will ever have. Inside the turn it is main
/// content and it stays.
#[test]
fn the_running_turn_keeps_the_row_for_its_own_task_call() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, "hiring scout"));
    chat.stream_msg = Some(chat.messages.len() - 1);

    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Running,
        None,
    ));

    let rows = main_rows(&mut chat);
    assert!(
        rows.iter().any(|r| r.contains("◉ @scout: fix the parser")),
        "the turn's own tool row is not bus noise: {rows:?}"
    );
}

/// A background command is main's own tool and keeps the walk-back it always
/// had: D94 reroutes agents, and only agents.
#[test]
fn a_command_watch_still_reaches_the_last_reply() {
    let mut chat = test_chat();
    chat.messages
        .push(msg(Role::Assistant, "watching the build"));
    assert!(chat.stream_msg.is_none());

    chat.apply_event(UiEvent::WatchEvent {
        label: "cargo watch".into(),
        kind: WatchKind::Command,
        status: WatchState::Running,
        detail: Some("round 1".into()),
        duration_ms: 0,
        payload: None,
        signal: None,
        notifies_main: false,
        dispatch: true,
    });

    let rows = main_rows(&mut chat);
    assert!(
        rows.iter().any(|r| r.contains("⏺ cargo watch")),
        "a command watch is main's own tool: {rows:?}"
    );
}

// ---------------------------------------------------------------------------
// B. the one line that still comes through (D98)
// ---------------------------------------------------------------------------

/// Bad news is the single exception to "nothing of an agent's life renders in
/// @main": the turn that would have narrated a crash may never run.
#[test]
fn a_failed_run_writes_one_alert_line_and_rings() {
    let mut chat = chat_with_bell();
    chat.apply_event(lifecycle(
        "scout #3 · fix the parser",
        WatchState::Failed,
        Some("subagent failed: connection reset"),
    ));

    let rows = main_rows(&mut chat);
    let alert: Vec<&String> = rows.iter().filter(|r| r.contains("⚠ @scout")).collect();
    assert_eq!(
        alert.len(),
        1,
        "exactly one line, not one per event: {rows:?}"
    );
    assert!(
        alert[0].contains("subagent failed: connection reset"),
        "the reason travels with the name: {alert:?}"
    );
    assert!(
        !alert[0].contains('❯'),
        "nobody typed it: no bubble putting the harness's words in the user's mouth"
    );
    assert!(
        crate::tui::chat::is_state_line(&chat.messages[0].text),
        "and it is classified as one"
    );
    assert!(
        emitted(&mut chat).contains('\x07'),
        "a failure reaches a user who is in another window"
    );
}

/// `Done` and `Cancelled` say themselves through the dispatch row's own state.
/// A second line for each would be the flood D94 removed, wearing a badge.
#[test]
fn a_finished_or_cancelled_run_writes_nothing() {
    let mut chat = chat_with_bell();
    chat.messages
        .push(msg(Role::Assistant, "I have asked scout"));
    let before = main_rows(&mut chat);

    chat.apply_event(lifecycle(
        "scout #3 · fix the parser",
        WatchState::Done,
        Some("done"),
    ));
    chat.apply_event(lifecycle(
        "zoe #1 · read the logs",
        WatchState::Cancelled,
        Some("stopped"),
    ));

    assert_eq!(main_rows(&mut chat), before, "the flow is byte-identical");
    assert_eq!(
        emitted(&mut chat),
        "",
        "and nothing went looking for the user"
    );
}

/// The alert keeps its send stamp. It is news, about someone, at a moment that
/// matters — "the build broke" reads differently at 09:02 and at 17:40 — where
/// the other state lines describe *now* and have nothing to stamp.
#[test]
fn the_alert_line_keeps_its_stamp() {
    let mut chat = test_chat();
    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Failed,
        None,
    ));
    let stamp = crate::tui::buffer::stamp(chat.messages[0].at);
    let rows = main_rows(&mut chat);
    assert!(
        rows.iter()
            .any(|r| r.contains("⚠ @scout") && r.trim_end().ends_with(&stamp)),
        "{rows:?}"
    );
}

// ---------------------------------------------------------------------------
// C. the digest debounce (D98)
// ---------------------------------------------------------------------------

/// One room post used to buy one woken turn. A burst now buys one digest: the
/// wake waits for the room to stop talking.
#[test]
fn a_burst_of_room_mail_wakes_once_after_the_quiet_window() {
    let mut chat = test_chat();
    chat.session
        .channels
        .deliver_to_main("scout", "one", None, false);
    assert!(!chat.digest_mail(), "the window has only just opened");
    assert!(
        chat.mail_wake.is_some(),
        "and the clock is armed, which is what keeps the tick alive"
    );
    assert!(
        chat.needs_tick(),
        "waiting mail keeps the frame loop running"
    );

    // A second message inside the window restarts it: the room is still talking.
    chat.tick += super::chat_tail::MAIL_QUIET_TICKS - 1;
    chat.session
        .channels
        .deliver_to_main("zoe", "two", None, false);
    assert!(
        !chat.digest_mail(),
        "the window restarted with the new message"
    );

    chat.tick += super::chat_tail::MAIL_QUIET_TICKS - 1;
    assert!(!chat.digest_mail(), "still inside the restarted window");
    chat.tick += 1;
    assert!(chat.digest_mail(), "the room went quiet; digest the batch");
    assert!(
        !chat.digest_mail(),
        "and exactly once — a second ask for the same batch is the flood again"
    );
}

/// v6 (D118): a room line naming nobody is penned, not mailed — it keeps the
/// frame loop ticking toward the age pump but opens no quiet window; the
/// mention releases the pen and only then does the debounce clock start.
#[test]
fn an_unnamed_room_line_waits_in_the_pen_and_release_starts_the_clock() {
    let mut chat = test_chat();
    chat.session
        .channels
        .create(
            "crew",
            vec![crate::channels::MAIN_NAME.into(), "scout".into()],
            crate::channels::ChannelMode::Free,
        )
        .unwrap_or_else(|e| panic!("{e}"));
    let _ = chat.session.channels.post("scout", "crew", "fyi: started");
    assert!(!chat.session.channels.has_main_mail(), "penned, not mailed");
    assert!(
        chat.needs_tick(),
        "a pen holds the frame loop open for the age pump"
    );
    chat.tick += super::chat_tail::MAIL_QUIET_TICKS + 1;
    assert!(!chat.digest_mail(), "no quiet window opens on penned mail");
    assert!(chat.mail_wake.is_none(), "the clock only starts at release");

    let _ = chat.session.channels.post("scout", "crew", "@main look");
    assert!(
        chat.session.channels.has_main_mail(),
        "the mention released the pen"
    );
    assert!(
        !chat.digest_mail(),
        "released mail still waits out the ordinary window"
    );
    chat.tick += super::chat_tail::MAIL_QUIET_TICKS;
    assert!(
        chat.digest_mail(),
        "then digests once, backlog and mention together"
    );
}

/// A room that never stops talking would restart the window forever. The
/// deadline is the floor under that: the digest runs on whatever has arrived.
#[test]
fn a_chatty_room_cannot_starve_the_wake_past_the_deadline() {
    let mut chat = test_chat();
    let step = super::chat_tail::MAIL_QUIET_TICKS - 1;
    chat.session
        .channels
        .deliver_to_main("scout", "0", None, false);
    assert!(!chat.digest_mail());
    let mut fired = false;
    for i in 1..=(super::chat_tail::MAIL_DEADLINE_TICKS / step + 1) {
        chat.tick += step;
        chat.session
            .channels
            .deliver_to_main("scout", &format!("{i}"), None, false);
        if chat.digest_mail() {
            fired = true;
            break;
        }
    }
    assert!(
        fired,
        "the deadline fires even though the quiet window never elapsed"
    );
    assert!(
        chat.tick >= super::chat_tail::MAIL_DEADLINE_TICKS,
        "and not before it: {}",
        chat.tick
    );
}

/// Urgent is the one thing that does not queue behind the window — and it rings
/// on arrival, whether or not a turn is what happens next.
#[test]
fn urgent_direct_mail_rings_and_skips_the_window() {
    let mut chat = chat_with_bell();
    chat.session
        .channels
        .deliver_to_main("scout", "I need the deploy key", None, true);
    assert!(chat.digest_mail(), "urgent does not wait out the window");
    assert!(
        emitted(&mut chat).contains('\x07'),
        "and it reaches a user who is in another window"
    );
}

/// The bell survives a turn that beat the tick to the mail: the drain and the
/// ring are different readers, so the flag is cleared by the reader that rings.
#[test]
fn the_ring_survives_a_turn_that_drained_the_mail_first() {
    let mut chat = chat_with_bell();
    chat.session
        .channels
        .deliver_to_main("scout", "blocked", None, true);
    let drained = chat.session.channels.drain_main_mail();
    assert_eq!(
        drained.len(),
        1,
        "a running turn absorbed it at its next round"
    );

    assert!(!chat.digest_mail(), "there is nothing left to digest");
    assert!(
        emitted(&mut chat).contains('\x07'),
        "but the bell it asked for is still owed"
    );
}

/// The injected form names the sender, and `line_source` — the single
/// recognizer of scaffolding shapes — reads it back.
#[test]
fn a_direct_message_to_main_carries_the_sender_into_the_inbox() {
    let chat = test_chat();
    chat.session
        .channels
        .deliver_to_main("scout", "the migration is done", None, false);
    let mail = chat.session.channels.drain_main_mail();
    assert_eq!(mail.len(), 1);
    let mut lines = mail[0].lines();
    assert_eq!(
        crate::tui::buffer::line_source(lines.next().unwrap_or_default()),
        Some(crate::tui::buffer::LineSource::Agent {
            name: "scout".to_string()
        }),
        "the marker is a header line, the way [DM from user] is: {mail:?}"
    );
    assert_eq!(lines.next(), Some("the migration is done"));
}

// ---------------------------------------------------------------------------
// D97 — the content-image registry and its open flows
// ---------------------------------------------------------------------------

/// A pid-tagged scratch directory for one test.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bingo-d97-{name}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn png_bytes(n: u8) -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend(std::iter::repeat_n(n, 96));
    bytes
}

/// A viewer that records the path it was handed instead of opening a window.
/// The command is a value, exactly as `composer::compose_with` takes the
/// editor's — no trait, no mock, just "the program is a parameter".
#[cfg(unix)]
fn recording_viewer(root: &std::path::Path, receipt: &std::path::Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join("viewer.sh");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$1\" > '{}'\nexit 0\n",
        receipt.display()
    );
    let _ = std::fs::write(&path, script);
    let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    path.to_string_lossy().to_string()
}

/// The receipt the viewer wrote. The spawn is detached by design — the TUI must
/// not wait on somebody's image viewer — so the test does the waiting the
/// production path deliberately does not.
fn wait_for(path: &std::path::Path) -> String {
    for _ in 0..200 {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.trim().is_empty()
        {
            return text;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("the viewer never ran: {}", path.display());
}

/// A picture the session showed and a picture a tool produced both land in the
/// list, newest first, each named the way the user would name it. An avatar
/// does not: it is chrome, and the rule has to hold at the tee, not in a
/// reader's head.
#[test]
fn content_images_register_newest_first_and_avatars_do_not() {
    let mut chat = chat_at(100, 40);
    let dir = scratch("register");
    let shot = dir.join("screenshot.png");
    std::fs::write(&shot, png_bytes(7)).expect("write");

    // A picture that placed on screen (agent prose, tool output, a URL).
    chat.handle(crate::ui::UiEvent::ImageReady {
        url: "https://example.com/plot.png".to_string(),
        meta: Some(crate::ui::ImageMeta {
            cols: 20,
            rows: 10,
            bytes: png_bytes(1),
        }),
    });
    // A tool that handed the model a file.
    chat.handle(crate::ui::UiEvent::ToolDone(crate::query::ToolCallDone {
        tool_call_id: "1".to_string(),
        name: "Read".to_string(),
        summary: "Read".to_string(),
        output: crate::tool::read::image_result_line(&shot, 104),
        status: crate::query::ToolCallStatus::Done,
        diff: None,
        duration_ms: 1,
    }));
    // And a portrait, transmitted the same way and registered nowhere.
    chat.faces.insert(crate::tui::avatar::index_of("scout"));

    let listed: Vec<String> = chat
        .image_registry
        .newest_first()
        .iter()
        .map(|e| e.source.clone())
        .collect();
    assert_eq!(
        listed,
        vec![
            shot.display().to_string(),
            "https://example.com/plot.png".to_string()
        ],
        "newest first, each under its own label"
    );
    assert!(
        !listed.iter().any(|s| s.contains("avatar")),
        "avatars are chrome and never register: {listed:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A failed image load is not a picture, so it is not in the list.
#[test]
fn a_failed_load_registers_nothing() {
    let mut chat = chat_at(100, 40);
    chat.handle(crate::ui::UiEvent::ImageReady {
        url: "https://example.com/gone.png".to_string(),
        meta: None,
    });
    assert!(
        chat.image_registry.newest_first().is_empty(),
        "nothing rendered, nothing to open"
    );
}

/// `/images` lists what the session showed, and Enter opens the browsed one
/// through the injected viewer.
#[cfg(unix)]
#[test]
fn slash_images_lists_and_enter_opens_the_browsed_image() {
    use crossterm::event::{KeyCode, KeyModifiers};

    let mut chat = chat_at(100, 40);
    let dir = scratch("picker");
    let receipt = dir.join("opened.txt");
    chat.image_opener = Some(recording_viewer(&dir, &receipt));

    let older = dir.join("older.png");
    let newer = dir.join("newer.png");
    std::fs::write(&older, png_bytes(1)).expect("write");
    std::fs::write(&newer, png_bytes(2)).expect("write");
    chat.image_registry.register_file(&older, 0, 104);
    chat.image_registry.register_file(&newer, 0, 104);

    chat.run_slash("images");
    let menu = chat.images_menu.clone().expect("the picker opened");
    let labels: Vec<String> = menu.items.iter().map(|i| i.label.clone()).collect();
    assert!(
        labels[0].contains("newer.png") && labels[1].contains("older.png"),
        "newest first, with source and size: {labels:?}"
    );
    assert!(
        labels[0].contains(" B"),
        "the size is on the row: {labels:?}"
    );

    // ↓ to the older one, Enter opens it.
    assert!(chat.images_menu_key(KeyCode::Down, KeyModifiers::NONE));
    assert!(chat.images_menu_key(KeyCode::Enter, KeyModifiers::NONE));
    assert!(chat.images_menu.is_none(), "Enter closes the picker");
    let opened = wait_for(&receipt);
    assert_eq!(
        opened.trim(),
        older.display().to_string(),
        "the browsed row is the image that opened"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Esc closes the picker, and the Esc layer walk finds it — it reuses the
/// `Menu` slot, so a new layer would have been a second way to say the same
/// thing.
#[test]
fn the_images_picker_lives_in_the_menu_esc_layer() {
    let mut chat = chat_at(100, 40);
    let dir = scratch("esc");
    let file = dir.join("a.png");
    std::fs::write(&file, png_bytes(1)).expect("write");
    chat.image_registry.register_file(&file, 0, 104);
    chat.run_slash("images");
    assert!(chat.menu_open(), "the picker is a menu");
    assert_eq!(
        chat.esc_layer(),
        Some(crate::tui::chat::chat_tail::EscLayer::Menu),
        "the layer walk finds it in the Menu slot"
    );
    assert!(chat.on_key(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE
    ));
    assert!(chat.images_menu.is_none(), "one Esc closes it");
    let _ = std::fs::remove_dir_all(&dir);
}

/// With nothing to show, `/images` says so on the info tier instead of opening
/// an empty surface.
#[test]
fn slash_images_says_so_when_there_is_nothing() {
    let mut chat = chat_at(100, 40);
    chat.run_slash("images");
    assert!(chat.images_menu.is_none(), "no picker");
    assert!(
        chat.slash_info_lines
            .iter()
            .any(|l| l.contains("no images")),
        "{:?}",
        chat.slash_info_lines
    );
}

/// A click on an image row resolves to that row's registry entry — both the
/// picture itself and the `#[image N]` marker a user's bubble carries.
#[cfg(unix)]
#[test]
fn a_click_on_an_image_row_opens_that_image() {
    let mut chat = chat_at(100, 40);
    let dir = scratch("click");
    let receipt = dir.join("opened.txt");
    chat.image_opener = Some(recording_viewer(&dir, &receipt));
    let file = dir.join("chart.png");
    std::fs::write(&file, png_bytes(4)).expect("write");
    let id = chat.image_registry.register_file(&file, 0, 104);
    chat.image_registry.set_marker(id, 3);

    // The rendered image block: every row carries the URL it was loaded from.
    let mut line = crate::tui::line::Line::styled("#[image]", SegStyle::plain());
    line.image = Some(crate::tui::line::ImageRef {
        url: file.display().to_string(),
        cols: 4,
        rows: 2,
        row: 0,
    });
    assert_eq!(
        chat.image_at_row(&crate::tui::chat::Row::new(line)),
        Some(id),
        "the image row addresses its entry"
    );
    // The marker inside a user's bubble is the same picture by another name.
    let marker = crate::tui::chat::Row::new(crate::tui::line::Line::styled(
        "look at #[image 3] please",
        SegStyle::plain(),
    ));
    assert_eq!(
        chat.image_at_row(&marker),
        Some(id),
        "and so does the marker the composer inserted"
    );

    chat.open_image(id);
    let opened = wait_for(&receipt);
    assert_eq!(opened.trim(), file.display().to_string());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A row with no picture behind it is not a click target: `doc_click` falls
/// through to the collapse-group ranges it always had.
#[test]
fn an_ordinary_row_is_not_an_image_click() {
    let chat = chat_at(100, 40);
    let row = crate::tui::chat::Row::new(crate::tui::line::Line::styled(
        "just prose",
        SegStyle::plain(),
    ));
    assert_eq!(chat.image_at_row(&row), None);
}

/// In the transcript pager `o` acts on the picture in view, and the pager
/// itself never spawns anything — it names the row and the loop opens it.
#[test]
fn the_transcript_o_key_names_the_visible_image_row() {
    use crossterm::event::{KeyCode, KeyModifiers};

    let mut line = crate::tui::line::Line::styled("#[image]", SegStyle::plain());
    line.image = Some(crate::tui::line::ImageRef {
        url: "plot.png".to_string(),
        cols: 4,
        rows: 2,
        row: 0,
    });
    let rows = vec![
        crate::tui::chat::Row::new(crate::tui::line::Line::styled("prose", SegStyle::plain())),
        crate::tui::chat::Row::new(line),
    ];
    let mut state = crate::tui::transcript::TranscriptState::new(rows, 10);
    assert_eq!(
        crate::tui::transcript::on_key(&mut state, KeyCode::Char('o'), KeyModifiers::NONE),
        crate::tui::transcript::Action::OpenImage(1),
        "`o` names the image row in view"
    );
    // ctrl+o still closes: the new binding is the bare letter only.
    assert_eq!(
        crate::tui::transcript::on_key(&mut state, KeyCode::Char('o'), KeyModifiers::CONTROL),
        crate::tui::transcript::Action::Close
    );
    // And with nothing to open it is a no-op rather than an error.
    let mut empty = crate::tui::transcript::TranscriptState::new(
        vec![crate::tui::chat::Row::new(crate::tui::line::Line::styled(
            "prose",
            SegStyle::plain(),
        ))],
        10,
    );
    assert_eq!(
        crate::tui::transcript::on_key(&mut empty, KeyCode::Char('o'), KeyModifiers::NONE),
        crate::tui::transcript::Action::None
    );
    assert!(
        crate::tui::transcript::footer(&state, 120, &crate::tui::theme::Theme::dark())
            .plain_text()
            .contains("o image"),
        "and the footer says so"
    );
}

/// A viewer that will not start is one info line — the tier for something the
/// user asked to read and did not get — and never a panic or a hang.
#[test]
fn a_failed_open_lands_on_the_info_tier() {
    let mut chat = chat_at(100, 40);
    let dir = scratch("failopen");
    let file = dir.join("x.png");
    std::fs::write(&file, png_bytes(1)).expect("write");
    let id = chat.image_registry.register_file(&file, 0, 104);
    chat.image_opener = Some("bingo-no-such-viewer-d97".to_string());
    chat.open_image(id);
    assert!(
        chat.slash_info_lines
            .iter()
            .any(|l| l.contains("could not open image")),
        "{:?}",
        chat.slash_info_lines
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// D99: @main gets a real unread. D94 left it with none at all once the
/// relay lines retired, so main could speak into a conversation the reader
/// was not in and the bar said nothing. Main's prose counts; the D98 failure
/// alert counts *and* wants you; entering the console clears both.
#[test]
fn the_console_counts_what_main_says_while_you_are_elsewhere() {
    let mut chat = test_chat();
    seed_agent(&chat, "scout");
    chat.refresh_conversations();
    // The console's count is only readable from somewhere else: `observe`
    // zeroes it outright while @main is the conversation being read. D103 drove
    // this through `switch_to`; the accounting store is what it was always
    // about, so it is driven through the store.
    chat.buffers.set_active(BufferId::Dm("scout".to_string()));

    let console = || BufferId::Hub;
    assert_eq!(
        chat.buffers.get(&console()).map(|b| b.unread()),
        Some(0),
        "nothing has been said yet"
    );

    chat.apply_turn_start();
    chat.apply_event(crate::ui::UiEvent::TextDelta("here is the answer".into()));
    chat.apply_event(crate::ui::UiEvent::TurnEnd);
    assert_eq!(chat.buffers.get(&console()).map(|b| b.unread()), Some(1));
    assert_eq!(
        chat.buffers.get(&console()).map(|b| b.mention()),
        Some(false),
        "main answering is news, not a summons"
    );

    chat.push_agent_alert("scout · fix the parser", Some("connection reset"));
    assert_eq!(chat.buffers.get(&console()).map(|b| b.unread()), Some(2));
    assert_eq!(
        chat.buffers.get(&console()).map(|b| b.mention()),
        Some(true),
        "an alert is the one line nobody chose to say"
    );

    chat.buffers.set_active(BufferId::Hub);
    assert_eq!(chat.buffers.get(&console()).map(|b| b.unread()), Some(0));
    assert_eq!(
        chat.buffers.get(&console()).map(|b| b.mention()),
        Some(false)
    );
}

/// D99 review: the console's user-role rows are not all the user's. A failure
/// alert, an ask receipt, an interrupt marker — the runtime reporting — must
/// not wear the human's portrait, or the gutter says the human wrote them. They
/// keep the *indentation*, so the message column does not jog around them, and
/// they leave the run, so the next thing main says re-leads with main's face:
/// the visual break the interruption already is. The same ruling the DM tail's
/// live-only states have carried since D97.
#[test]
fn a_state_line_takes_the_indentation_and_nobodys_face() {
    for images in [false, true] {
        let mut chat = chat_at(78, 40);
        chat.chat_avatars = true; // the gutter under test follows the one avatar switch (D110)
        if images {
            chat.image_cap = Some(crate::tui::gfx::ImageCap::default_cells());
        }
        chat.messages
            .push(msg(Role::Assistant, "scout is on the parser."));
        chat.push_agent_alert("scout · fix the parser", Some("connection reset"));
        chat.messages
            .push(msg(Role::Assistant, "I will hire a replacement."));
        chat.build_rows(78);
        let rows: Vec<String> = chat.doc.rows.iter().map(|r| r.line.plain_text()).collect();
        let row_with = |needle: &str| -> String {
            rows.iter()
                .find(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("no row contains {needle:?}: {rows:#?}"))
                .clone()
        };

        let gutter = crate::tui::avatar::gutter_width(images);
        let alert = row_with("⚠ @scout");
        let cut = alert.find('⚠').unwrap_or(0);
        assert_eq!(
            crate::tui::line::text_width(&alert[..cut]),
            gutter,
            "the alert starts at the gutter's edge ({images}): {alert:?}"
        );
        assert_eq!(
            alert[..cut].trim(),
            "",
            "and the cells before it are blank — no chip, no portrait ({images}): {alert:?}"
        );
        assert!(
            !alert[..cut].contains(crate::tui::gfx::PLACEHOLDER),
            "least of all an image ({images}): {alert:?}"
        );

        // The run broke: main's reply after the alert opens a fresh one.
        let before = row_with("scout is on the parser.");
        let after = row_with("I will hire a replacement.");
        assert_eq!(
            before[..before.find('⏺').unwrap_or(0)],
            after[..after.find('⏺').unwrap_or(0)],
            "main re-leads with its own face after the interruption ({images})"
        );
        assert_ne!(
            after[..after.find('⏺').unwrap_or(0)].trim(),
            "",
            "and that face is drawn, not blank ({images}): {after:?}"
        );
    }
}

/// A steered message is the user's own words and keeps the user's face: it is
/// not a state line, and the `↪` marker says where in the reply it landed
/// rather than that nobody wrote it.
#[test]
fn a_steered_message_still_wears_the_users_face() {
    let mut chat = chat_at(78, 40);
    chat.chat_avatars = true; // this test's subject predates the one avatar switch (D110)
    chat.messages.push(msg(Role::Assistant, "working on it"));
    chat.absorb_steered(&[crate::steer::SteerItem {
        id: 1,
        text: "also check the lexer".to_string(),
    }]);
    chat.build_rows(78);
    let row = chat
        .doc
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .find(|row| row.contains("also check the lexer"))
        .unwrap_or_else(|| panic!("the steered line renders"));
    assert!(
        row.starts_with(" U "),
        "the user typed it, so the face is right: {row:?}"
    );
}

// ---------------------------------------------------------------------------
// D103 — the woken turn ends like any other
// ---------------------------------------------------------------------------

/// Open a turn the way `submit_auto` does: an injected notification rather than
/// the user's words.
fn woken_turn(chat: &mut Chat) {
    chat.apply_turn_start();
}

/// D102 gave a woken turn a second ending — a marker that rendered as nothing.
/// D103 takes it back on the parity ruling: CC's leader narrates, and the noise
/// control is the wake debounce plus the dispatch row's own state. So a digest
/// turn's prose is main speaking, in main's own flow, like every other turn.
#[test]
fn a_woken_turn_renders_its_prose_as_main_speaking() {
    let mut chat = test_chat();

    woken_turn(&mut chat);
    chat.apply_event(UiEvent::TextDelta(
        "scout wants a decision on the schema".into(),
    ));
    chat.apply_event(UiEvent::TurnEnd);
    past_settle(&mut chat);

    assert!(
        main_rows(&mut chat)
            .iter()
            .any(|row| row.contains("scout wants a decision on the schema")),
        "prose from a woken turn is main speaking, and main speaks in @main"
    );
}

/// The retired marker has no reader left: it is text on the wire like any other
/// text, so it renders verbatim wherever it lands. Pinned because "renders as
/// nothing" was a real rule for one batch and its removal has to be observable.
#[test]
fn the_retired_quiet_marker_is_ordinary_prose_now() {
    let mut chat = test_chat();
    let before = main_rows(&mut chat);

    woken_turn(&mut chat);
    chat.apply_event(UiEvent::TextDelta("[[quiet]]".into()));
    chat.apply_event(UiEvent::TurnEnd);
    past_settle(&mut chat);

    let rows = main_rows(&mut chat);
    assert_ne!(rows, before, "a woken turn that speaks prints rows");
    assert!(
        rows.iter().any(|row| row.contains("[[quiet]]")),
        "verbatim, with no render rule reading it: {rows:?}"
    );
}

/// The badge follows the same reading: a woken turn that says something counts
/// on the console the way a typed one does. `note_console` carries the unread
/// *and* the conversation's activity clock, and D102's guard in front of it is
/// gone.
#[test]
fn a_woken_turn_counts_on_the_console_like_any_other() {
    let mut chat = test_chat();
    // Somewhere else is where a console badge can be read at all: `observe`
    // zeroes the count outright while @main is the conversation being read.
    chat.buffers.set_active(BufferId::Dm("scout".to_string()));

    woken_turn(&mut chat);
    chat.apply_event(UiEvent::TextDelta("the migration finished".into()));
    chat.apply_event(UiEvent::TurnEnd);

    assert_eq!(
        chat.buffers
            .get(&BufferId::Hub)
            .map(crate::tui::buffer::Buffer::unread),
        Some(1),
        "main spoke, so the console has something to come back for"
    );
}

// ---------------------------------------------------------------------------
// E. the transcript's tiers (D106)
// ---------------------------------------------------------------------------

/// Put a dispatch row on a running turn and give its instance a run to show.
fn dispatching(chat: &mut Chat, name: &str, description: &str, activity: &[&str], tokens: u64) {
    seed_agent(chat, name);
    chat.session.agents.set_progress_snapshot(
        name,
        crate::agents::AgentProgress {
            started_at: Some(std::time::Instant::now()),
            output_tokens: tokens,
            tool_uses: activity.len(),
            recent_activity: activity.iter().map(|a| a.to_string()).collect(),
        },
    );
    chat.apply_event(lifecycle(
        &format!("{name} · {description}"),
        WatchState::Running,
        None,
    ));
}

/// The dispatch row's live half: the last three things the instance did, oldest
/// first, the first of them on the `⎿` connector. It is drawn from the registry
/// on the tick, so it says what is true now rather than what was true when the
/// row was created.
#[test]
fn a_dispatch_row_shows_the_last_three_things_the_agent_did() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, "hiring scout"));
    chat.stream_msg = Some(chat.messages.len() - 1);
    dispatching(
        &mut chat,
        "scout",
        "fix the parser",
        &[
            "⏺ Grep(fn main)",
            "⏺ Read(src/lexer.rs)",
            "⏺ Bash(cargo test)",
            "⏺ Edit(src/lexer.rs)",
        ],
        8_300,
    );
    chat.tick();

    let rows = main_rows(&mut chat);
    let at = rows
        .iter()
        .position(|r| r.contains("◉ @scout: fix the parser"))
        .unwrap_or_else(|| panic!("dispatch row: {rows:?}"));
    assert!(rows[at + 1].contains("⎿  ⏺ Read(src/lexer.rs)"), "{rows:?}");
    assert!(rows[at + 2].trim() == "⏺ Bash(cargo test)", "{rows:?}");
    assert!(rows[at + 3].trim() == "⏺ Edit(src/lexer.rs)", "{rows:?}");
    assert!(
        !rows.iter().any(|r| r.contains("Grep(fn main)")),
        "only the last three: {rows:?}"
    );
    assert!(
        !chat.message_settled(chat.messages.len() - 1),
        "a message holding a running dispatch never settles, which is why these \
         rows can be transient at all"
    );
}

/// The window is the budget. CC drops the per-tool rows for one condensed line
/// when the terminal cannot hold them, and so does this.
#[test]
fn a_short_window_condenses_the_dispatch_progress_to_one_line() {
    let mut chat = crate::tui::test_util::chat_at(80, 8);
    chat.messages.push(msg(Role::Assistant, "hiring scout"));
    chat.stream_msg = Some(chat.messages.len() - 1);
    dispatching(
        &mut chat,
        "scout",
        "fix the parser",
        &["⏺ Grep(fn main)", "⏺ Read(src/lexer.rs)"],
        8_300,
    );
    chat.tick();

    let rows = main_rows(&mut chat);
    assert!(
        rows.iter()
            .any(|r| r.contains("In progress… · 2 tool uses · 8.3k tokens")),
        "{rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r.contains("Read(src/lexer.rs)")),
        "the rows it could not afford are gone, not clipped: {rows:?}"
    );
}

/// The dispatch row's settled half, and the only form of it that reaches
/// scrollback. The numbers survive the run they describe: the registry drops a
/// run's progress before it reports the end, so the row keeps its own copy.
#[test]
fn a_finished_dispatch_settles_into_what_the_run_cost() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, "hiring scout"));
    chat.stream_msg = Some(chat.messages.len() - 1);
    dispatching(
        &mut chat,
        "scout",
        "fix the parser",
        &["⏺ Read(src/lexer.rs)", "⏺ Bash(cargo test)"],
        8_300,
    );
    chat.tick();
    // What the domain does one line before it reports the end.
    chat.session.agents.set_progress("scout", None);
    chat.apply_event(UiEvent::WatchEvent {
        label: "scout · fix the parser".into(),
        kind: WatchKind::Agent,
        status: WatchState::Done,
        detail: Some("done".into()),
        duration_ms: 64_000,
        payload: None,
        signal: None,
        notifies_main: false,
        dispatch: true,
    });
    chat.tick();

    let rows = main_rows(&mut chat);
    assert!(
        rows.iter()
            .any(|r| r.contains("⎿  Done (2 tool uses · 8.3k tokens · 1m 4s)")),
        "{rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r.contains("Bash(cargo test)")),
        "the progress rows are not part of what settles: {rows:?}"
    );
    chat.stream_msg = None;
    assert!(
        chat.message_settled(chat.messages.len() - 1),
        "and now the message can settle, which is when this row is printed once \
         and never touched again"
    );
}

/// One round, several agents, one block — CC's grouped tree. Opening any of
/// them takes the group apart, which is how the folded rows keep their content
/// reachable and how the `ctrl+o` transcript sees the full thing.
#[test]
fn several_agents_from_one_round_draw_one_tree() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, "hiring two"));
    chat.stream_msg = Some(chat.messages.len() - 1);
    dispatching(
        &mut chat,
        "scout",
        "fix the parser",
        &["⏺ Read(a.rs)"],
        2_100,
    );
    dispatching(&mut chat, "zoe", "run the tests", &[], 0);
    chat.tick();

    let rows = main_rows(&mut chat);
    assert!(
        rows.iter().any(|r| r.contains("⏺ Running 2 agents…")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|r| r.contains("├─ @scout: fix the parser · 1 tool use · 2.1k tokens")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("│  ⎿  ⏺ Read(a.rs)")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("└─ @zoe: run the tests")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("⎿  Initializing…")),
        "an agent with nothing behind it yet: {rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r.contains("◉ @scout")),
        "and neither of them keeps a row of its own: {rows:?}"
    );

    for label in ["scout · fix the parser", "zoe · run the tests"] {
        chat.apply_event(lifecycle(label, WatchState::Done, Some("done")));
    }
    let rows = main_rows(&mut chat);
    assert!(
        rows.iter().any(|r| r.contains("⏺ 2 agents finished")),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .filter(|r| r.trim_end().ends_with("Done"))
            .count()
            == 2,
        "a row inside the group says the one word: {rows:?}"
    );

    // Opening one of them dissolves the group back into individual rows.
    let last = chat.messages.len() - 1;
    chat.messages[last].activities[0].expanded = true;
    let rows = main_rows(&mut chat);
    assert!(
        !rows.iter().any(|r| r.contains("agents finished")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("◉ @scout: fix the parser")),
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.contains("◉ @zoe: run the tests")),
        "{rows:?}"
    );
}

/// The completion's own line: one dim `●` where the task notification landed in
/// main's context, before main narrates anything. It is gated on the
/// notification actually being main's — a run the user started inside an
/// agent's own conversation reports to nobody — and a *failure* keeps D98's
/// alert instead of getting a second line.
#[test]
fn a_completion_notification_leaves_one_dim_line() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, "hiring scout"));
    chat.stream_msg = Some(chat.messages.len() - 1);
    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Running,
        None,
    ));

    chat.apply_event(UiEvent::WatchEvent {
        label: "scout · fix the parser".into(),
        kind: WatchKind::Agent,
        status: WatchState::Done,
        detail: Some("done".into()),
        duration_ms: 1_000,
        payload: None,
        signal: None,
        notifies_main: true,
        dispatch: true,
    });
    let rows = main_rows(&mut chat);
    assert!(
        rows.iter()
            .any(|r| r.contains("● @scout completed · fix the parser")),
        "{rows:?}"
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r.contains("● @scout completed"))
            .count(),
        1,
        "one line, not one per reader: {rows:?}"
    );

    // A run that reports to nobody prints nothing.
    chat.apply_event(lifecycle("zoe · look around", WatchState::Running, None));
    chat.apply_event(lifecycle(
        "zoe · look around",
        WatchState::Done,
        Some("done"),
    ));
    assert!(
        !main_rows(&mut chat).iter().any(|r| r.contains("● @zoe")),
        "a run registered with notify_owner: false tells the flow nothing"
    );

    // A failure keeps the alert and earns no notice of its own.
    chat.apply_event(UiEvent::WatchEvent {
        label: "writer · draft the notes".into(),
        kind: WatchKind::Agent,
        status: WatchState::Failed,
        detail: Some("connection reset".into()),
        duration_ms: 0,
        payload: None,
        signal: None,
        notifies_main: true,
        dispatch: true,
    });
    let rows = main_rows(&mut chat);
    assert!(
        rows.iter()
            .any(|r| r.contains("⚠ @writer · connection reset")),
        "{rows:?}"
    );
    assert!(!rows.iter().any(|r| r.contains("● @writer")), "{rows:?}");
}

/// v3 made an agent's message to main render nothing; v4 printed one line per
/// arrival; D114 keeps the batching for the rows that are left — consecutive
/// `●` notices coalesce into one block, and the `⚠` alert never joins them.
#[test]
fn a_streak_of_notices_reads_as_one_batch() {
    let mut chat = test_chat();
    seed_agent(&chat, "scout");
    seed_agent(&chat, "writer");
    chat.messages
        .push(msg(Role::Assistant, "dispatching them now."));
    chat.push_agent_notice("scout · fix the lexer");
    chat.push_agent_notice("writer · draft the notes");

    let rows = main_rows(&mut chat);
    let at = |needle: &str| -> usize {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("no row contains {needle:?}: {rows:?}"))
    };
    // Two notices, adjacent: the second joins the first's block instead of
    // opening its own with a blank row (D111 — consecutive arrivals are one
    // batch to the reader, the tool groups' own argument).
    assert_eq!(
        at("● @writer"),
        at("● @scout") + 1,
        "no blank row inside the streak: {rows:?}"
    );
    // The streak itself still opens like any message: a blank above the first.
    assert_eq!(rows[at("● @scout") - 1], "", "{rows:?}");

    // An alert never joins the batch — bad news keeps its own block.
    chat.push_agent_alert("scout · fix the lexer", Some("connection reset"));
    let rows = main_rows(&mut chat);
    let alert = rows
        .iter()
        .position(|r| r.contains("⚠ @scout"))
        .unwrap_or_else(|| panic!("{rows:?}"));
    assert_eq!(
        rows[alert - 1],
        "",
        "the alert opens its own block: {rows:?}"
    );
}

/// The inbox turn (D114): a message an agent sends main writes nothing into
/// the flow. The delivery is untouched — the mail is in main's inbox, the
/// wake and debounce unchanged — and what the user gets is the status
/// layer's fuel: the sender's mail count, cleared when its zoom is visited.
#[test]
fn a_message_from_an_agent_writes_no_line_and_counts_as_mail() {
    let mut chat = test_chat();
    seed_agent(&chat, "scout");
    let body = "the parser was fine; the lexer drops a token when the input ends mid-string";
    let before = main_rows(&mut chat);
    chat.session
        .channels
        .deliver_to_main("scout", body, Some("lexer drops a token at EOF"), false);
    chat.tick();

    assert_eq!(
        main_rows(&mut chat),
        before,
        "an arrival is main's mail, not the user's conversation"
    );
    assert_eq!(
        chat.agent_mail.get("scout"),
        Some(&1),
        "the mirror feeds the sender's dot instead"
    );
    assert!(
        chat.session.channels.has_main_mail(),
        "and the message itself is still in main's inbox, unread and unchanged"
    );
    assert!(
        chat.session
            .channels
            .drain_main_mail()
            .iter()
            .any(|mail| mail.contains(body) && !mail.contains("lexer drops a token at EOF")),
        "the mail is the message, and the summary never entered it"
    );
}

/// The whitelist's question tier (D116): a room post that names the user is
/// the one thing a room may put in the flow — one `⚑` line per mention
/// turn-on, stamped, ringing D79. Further mentions wait behind the lit badge
/// until the room is read; reading re-arms the line.
#[test]
fn a_room_post_naming_the_user_leaves_one_flag_line() {
    let mut chat = test_chat();
    seed_agent(&chat, "scout");
    chat.session
        .channels
        .create(
            "dev-team",
            vec!["scout".to_string(), crate::channels::USER_NAME.to_string()],
            crate::channels::ChannelMode::Free,
        )
        .expect("room created");
    let settle = |chat: &mut Chat| {
        for _ in 0..16 {
            chat.tick();
        }
    };
    settle(&mut chat);

    chat.session
        .channels
        .post("scout", "dev-team", "@user should I deploy with --force?")
        .expect("posted");
    settle(&mut chat);
    let flags = |chat: &mut Chat| {
        main_rows(chat)
            .iter()
            .filter(|r| r.contains("⚑ #dev-team @scout:"))
            .count()
    };
    assert_eq!(flags(&mut chat), 1, "{:?}", main_rows(&mut chat));

    // A second mention behind the same lit badge is the same event.
    chat.session
        .channels
        .post("scout", "dev-team", "@user still waiting")
        .expect("posted");
    settle(&mut chat);
    assert_eq!(flags(&mut chat), 1, "one turn-on, one line");

    // Reading the room re-arms it.
    chat.enter_zoom(crate::tui::zoom::ZoomTarget::Room("dev-team".to_string()));
    chat.leave_zoom(crate::tui::buffer::BufferId::Hub);
    settle(&mut chat);
    chat.session
        .channels
        .post("scout", "dev-team", "@user it shipped")
        .expect("posted");
    settle(&mut chat);
    assert_eq!(
        flags(&mut chat),
        2,
        "a fresh mention after a read flags again"
    );

    // An ordinary post never does.
    chat.session
        .channels
        .post("scout", "dev-team", "pushing the branch now")
        .expect("posted");
    settle(&mut chat);
    assert_eq!(flags(&mut chat), 2, "{:?}", main_rows(&mut chat));
}

/// The other half of the gate: a run main did not dispatch — a room post or a
/// queued message waking a member — completes without a `●` line, even
/// though its task notification reaches main's context exactly as before.
/// Only `dispatch: true` — the run an `Agent` call asked for — prints one.
#[test]
fn a_delivery_triggered_run_completes_without_a_notice() {
    let mut chat = test_chat();
    seed_agent(&chat, "scout");
    chat.messages.push(msg(Role::Assistant, "the room is busy"));
    let before = main_rows(&mut chat);

    chat.apply_event(UiEvent::WatchEvent {
        label: "scout #2 · answer the room".to_string(),
        kind: WatchKind::Agent,
        status: WatchState::Done,
        detail: Some("done".to_string()),
        duration_ms: 0,
        payload: None,
        signal: None,
        notifies_main: true,
        dispatch: false,
    });
    assert_eq!(
        main_rows(&mut chat),
        before,
        "a delivery's end is the tree's business, not the flow's"
    );
}

/// While main is streaming, only the runs *this turn* dispatched may hang a
/// row on the streaming message. A member woken by a room post mid-turn used
/// to appear under whatever main happened to be saying, as a "Running N
/// agents" tree about work the turn never asked for.
#[test]
fn a_streaming_turn_staples_only_its_own_dispatches() {
    let mut chat = test_chat();
    seed_agent(&chat, "scout");
    seed_agent(&chat, "writer");
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);

    chat.apply_event(UiEvent::WatchEvent {
        label: "writer #3 · answer the room".to_string(),
        kind: WatchKind::Agent,
        status: WatchState::Running,
        detail: None,
        duration_ms: 0,
        payload: None,
        signal: None,
        notifies_main: false,
        dispatch: false,
    });
    assert!(
        chat.messages[0].activities.is_empty(),
        "a run the turn did not dispatch stays off its tree"
    );

    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Running,
        None,
    ));
    assert_eq!(
        chat.messages[0].activities.len(),
        1,
        "the turn's own dispatch is stapled as before"
    );
}

/// The bottom row of the tiering table: an instance starting, going idle or
/// being stopped is the tree's business and the pills', never the flow's.
#[test]
fn running_idle_and_stopped_write_no_line() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::User, "have someone look"));
    chat.messages.push(msg(Role::Assistant, "scout is on it"));
    seed_agent(&chat, "scout");
    let before = main_rows(&mut chat);

    chat.apply_event(lifecycle(
        "scout · fix the parser",
        WatchState::Running,
        None,
    ));
    chat.apply_event(lifecycle("scout · fix the parser", WatchState::Idle, None));
    chat.session.agents.mark_idle("scout");
    chat.session.agents.stop("scout").expect("stopped");
    chat.refresh_conversations();
    chat.tick();

    assert_eq!(
        main_rows(&mut chat),
        before,
        "the roster changed and the transcript did not"
    );
}

/// A run's label is `{instance}` first in every shape it takes, so the second
/// run of the same instance is the same person: same name on the row, same
/// face in the gutter. Before D106 the face was keyed on everything up to the
/// first ` · `, which made `scout #3` a stranger.
#[test]
fn a_continuation_run_is_the_same_agent_as_the_first() {
    let mut chat = test_chat();
    chat.chat_avatars = true;
    chat.image_cap = Some(crate::tui::gfx::ImageCap::default_cells());
    chat.messages.push(msg(Role::Assistant, "hiring scout"));
    chat.stream_msg = Some(chat.messages.len() - 1);
    seed_agent(&chat, "scout");
    chat.apply_event(lifecycle(
        "scout #3 · look again",
        WatchState::Running,
        None,
    ));

    let rows = main_rows(&mut chat);
    assert!(
        rows.iter().any(|r| r.contains("@scout: look again")),
        "the row names the instance, not the run: {rows:?}"
    );
    assert!(
        chat.faces.contains(&crate::tui::avatar::index_of("scout")),
        "and claims scout's own face: {:?}",
        chat.faces
    );
}

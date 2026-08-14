//! Chat state-machine tests, part three: the collapse fold's own contract.
//!
//! `chat_tests_a` / `chat_tests_b` split by size alone (the 4000-line file cap); this
//! file continues them.

use super::chat_tail::EscLayer;
use super::tests_a::*;
use super::*;
use serde_json::json;

/// Three grouped reads, each with output of its own.
fn grouped_reads(chat: &mut Chat, calls: &[(&str, &str)]) {
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    for (path, _) in calls {
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Read".into(),
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            tool_call_id: "test-tool".into(),
            name: "Read".into(),
            input: json!({ "file_path": path }),
            standalone: false,
        });
        chat.drain_events();
    }
    for (path, output) in calls {
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
                tool_call_id: "test-tool".into(),
                name: "Read".into(),
                summary: format!("Read {path}"),
                output: (*output).into(),
                status: crate::query::ToolCallStatus::Done,
                duration_ms: 0,
                diff: None,
            }));
    }
    chat.drain_events();
}

/// D78: a folded result is still a result. The fold is the only row a grouped read ever
/// gets, so a member whose output was dropped could never be read again — expanding the
/// group revealed summaries over nothing.
#[test]
fn grouped_results_keep_their_output_for_expansion() {
    let calls = [
        ("a.md", "alpha first\nalpha second"),
        ("b.md", "beta only"),
        ("c.md", "gamma first\ngamma second\ngamma third"),
    ];
    let mut chat = test_chat();
    grouped_reads(&mut chat, &calls);
    finish_turn(&mut chat);

    let collapsed = visible(&mut chat, 120, 40);
    assert!(
        collapsed.contains("Read 3 files"),
        "the fold summary is unchanged: {collapsed}"
    );
    assert!(
        !collapsed.contains("alpha first"),
        "a collapsed group shows no output: {collapsed}"
    );

    let group = chat.messages[0].groups.first().expect("one group");
    assert_eq!(group.activities.len(), 3, "all three folded together");
    for &idx in &group.activities {
        let member = &chat.messages[0].activities[idx];
        assert!(member.expandable(), "member {idx} kept its output");
    }

    assert!(chat.toggle_transcript(), "ctrl+o opens the fold");
    let expanded = visible(&mut chat, 120, 40);
    for (_, output) in calls {
        for line in output.lines() {
            assert!(expanded.contains(line), "{line:?} is readable: {expanded}");
        }
    }
    assert!(
        expanded.contains("Read 2 lines"),
        "the per-row summary survives beside the content: {expanded}"
    );
}

/// D78: the mouse reaches the same output. Every row of an open group is wrapped in the
/// group's own click target, so a member row cannot be opened on its own — the group's
/// state carries its members'.
#[test]
fn clicking_a_group_open_reveals_the_member_output() {
    let calls = [("a.md", "alpha first"), ("b.md", "beta first")];
    let mut chat = test_chat();
    grouped_reads(&mut chat, &calls);
    finish_turn(&mut chat);
    chat.build_rows(120);
    let fold_row = chat
        .doc
        .click_ranges
        .iter()
        .find(|r| matches!(r.target, ClickTarget::Group { .. }))
        .map(|r| r.start)
        .expect("group fold row");

    assert!(chat.doc_click(fold_row), "click opens the group");
    let expanded = visible(&mut chat, 120, 40);
    assert!(
        expanded.contains("alpha first") && expanded.contains("beta first"),
        "the output is on screen: {expanded}"
    );

    chat.build_rows(120);
    let head_row = chat
        .doc
        .click_ranges
        .iter()
        .find(|r| matches!(r.target, ClickTarget::Group { .. }))
        .map(|r| r.start)
        .expect("group head row");
    assert!(chat.doc_click(head_row), "click folds it back");
    let collapsed = visible(&mut chat, 120, 40);
    assert!(
        collapsed.contains("Read 2 files") && !collapsed.contains("alpha first"),
        "back to the summary row: {collapsed}"
    );
}

/// D78: the retained output lives under the budget the model already lives under, and a
/// fold does not buy a row a bigger one.
#[test]
fn a_large_grouped_result_is_bounded_like_a_standalone_one() {
    let big = "a line of tool output\n".repeat(4_000);
    assert!(
        big.chars().count() > MAX_RESULT_CHARS,
        "the fixture has to exceed the budget"
    );
    let content = |standalone: bool| -> Vec<String> {
        let mut chat = test_chat();
        chat.messages.push(msg(Role::Assistant, ""));
        chat.stream_msg = Some(0);
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Read".into(),
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            tool_call_id: "test-tool".into(),
            name: "Read".into(),
            input: json!({"file_path": "big.txt"}),
            standalone,
        });
        chat.drain_events();
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
                tool_call_id: "test-tool".into(),
                name: "Read".into(),
                summary: "Read big.txt".into(),
                output: big.clone(),
                status: crate::query::ToolCallStatus::Done,
                duration_ms: 0,
                diff: None,
            }));
        chat.drain_events();
        chat.messages[0].activities[0]
            .content
            .iter()
            .map(|l| l.plain_text())
            .collect()
    };
    let grouped = content(false);
    let standalone = content(true);
    assert_eq!(grouped, standalone, "one rule, folded or not");
    assert!(!grouped.is_empty(), "the output is kept, not dropped");
    assert!(
        grouped.len() < big.lines().count(),
        "the budget actually bit: {} of {} lines",
        grouped.len(),
        big.lines().count()
    );
    let chars: usize = grouped.iter().map(|l| l.chars().count()).sum();
    assert!(chars <= MAX_RESULT_CHARS, "{chars} chars retained");
}

/// D78 × D76: keeping grouped output must not resurrect it for a call that was stopped
/// before it produced any. An interrupted member has nothing to open, and the fold still
/// refuses to count it as a failure.
#[test]
fn an_interrupted_group_member_still_has_nothing_to_expand() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    for path in ["a.md", "b.md"] {
        let _ = chat.events.send(UiEvent::ToolStart {
            name: "Read".into(),
        });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            tool_call_id: "test-tool".into(),
            name: "Read".into(),
            input: json!({ "file_path": path }),
            standalone: false,
        });
        chat.drain_events();
    }
    for (summary, output, status) in [
        (
            "Read a.md",
            "alpha first",
            crate::query::ToolCallStatus::Done,
        ),
        (
            "Read b.md",
            "interrupted",
            crate::query::ToolCallStatus::Interrupted,
        ),
    ] {
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
                tool_call_id: "test-tool".into(),
                name: "Read".into(),
                summary: summary.into(),
                output: output.into(),
                status,
                duration_ms: 0,
                diff: None,
            }));
    }
    chat.drain_events();
    finish_turn(&mut chat);

    let stopped = &chat.messages[0].activities[1];
    assert!(
        !stopped.expandable(),
        "an interrupted call produced no output to keep"
    );
    assert!(
        chat.messages[0].activities[0].expandable(),
        "its neighbour still keeps its own"
    );

    assert!(chat.toggle_transcript(), "ctrl+o opens the fold");
    let expanded = visible(&mut chat, 120, 40);
    assert!(
        expanded.contains("⎿  Interrupted"),
        "the stopped row names its state: {expanded}"
    );
    assert!(
        !expanded.contains("interrupted\n") && !expanded.contains("  interrupted"),
        "the placeholder text is not shown as output: {expanded}"
    );
    assert!(
        expanded.contains("alpha first"),
        "the finished call is readable: {expanded}"
    );
}

// ---------------------------------------------------------------------------
// D79 — the attention channel's trigger points.
//
// The byte goldens live in `notify.rs`; these assert who pulls the trigger, in
// the state machine that actually pulls it. The notifier is the mock writer:
// nothing reaches a terminal, and `take()` returns exactly what would have.
// ---------------------------------------------------------------------------

use crate::tui::notify::{Notifier, NotifyChannel, TerminalEnv};

/// A chat wired to the bell channel, its startup title already collected.
fn chat_with_bell() -> Chat {
    let mut chat = crate::tui::test_util::chat_at(80, 24);
    chat.set_notifier(Notifier::new(NotifyChannel::Bell, &TerminalEnv::default()));
    let startup = String::from_utf8_lossy(&chat.notify.take()).to_string();
    assert!(
        startup.starts_with("\x1b]2;bingo — "),
        "a session names its directory as soon as it has the terminal: {startup:?}"
    );
    chat
}

fn emitted(chat: &mut Chat) -> String {
    String::from_utf8_lossy(&chat.notify.take()).to_string()
}

/// The title an idle session wears, for this chat's directory.
fn idle_title(chat: &Chat) -> String {
    format!(
        "\x1b]2;bingo — {}\x07",
        crate::tui::notify::cwd_short(&chat.cwd)
    )
}

/// Move the running turn's start stamp far enough back to cross the threshold.
fn age_the_turn(chat: &mut Chat) {
    chat.turn_started = Some(
        std::time::Instant::now()
            .checked_sub(crate::tui::notify::LONG_TURN + std::time::Duration::from_secs(1))
            .expect("the process has been running for longer than the threshold"),
    );
}

/// A permission prompt blocks the turn until it is answered, and the user is
/// the only one who can answer it — so it is the one event that notifies
/// regardless of how long the turn has run.
#[test]
fn a_waiting_permission_prompt_rings() {
    let mut chat = chat_with_bell();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    chat.asks
        .send((
            PermissionRequest::new("Allow running Bash", "cargo build", vec!["Allow".into()]),
            tx,
        ))
        .unwrap();
    assert!(chat.drain_asks(), "the request is accepted");
    assert_eq!(
        emitted(&mut chat),
        "\x07\x1b]2;✳ bingo — waiting for permission\x07",
        "bell first, then the title that says what it is waiting for"
    );

    // A second request cannot be accepted while one is pending, so it cannot
    // ring either — the modal is already on screen.
    let (tx2, _rx2) = tokio::sync::oneshot::channel();
    chat.asks
        .send((
            PermissionRequest::new("Allow running Bash", "cargo test", vec!["Allow".into()]),
            tx2,
        ))
        .unwrap();
    assert!(!chat.drain_asks());
    assert!(
        emitted(&mut chat).is_empty(),
        "no second bell for a queued ask"
    );
}

/// A turn the user sat through is not news. The threshold is wall time, taken
/// before `TurnEnd` clears the start stamp.
#[test]
fn only_a_long_turn_announces_its_end() {
    let mut chat = chat_with_bell();

    chat.handle(UiEvent::TurnStart);
    assert_eq!(
        emitted(&mut chat),
        "\x1b]2;✳ bingo — working…\x07",
        "the title goes busy the moment the turn opens"
    );
    let idle = idle_title(&chat);
    chat.handle(UiEvent::TurnEnd);
    assert_eq!(
        emitted(&mut chat),
        idle,
        "a turn that just started rings nothing; it only hands the title back"
    );

    chat.handle(UiEvent::TurnStart);
    let _ = chat.notify.take();
    age_the_turn(&mut chat);
    chat.handle(UiEvent::TurnEnd);
    assert_eq!(
        emitted(&mut chat),
        format!("\x07{idle}"),
        "a turn long enough to walk away from rings, then goes idle"
    );
}

/// A flow-level failure is the end of the turn; a page-level one is a hint
/// beside a session that carries on, and interrupting the user for it would
/// make the channel worthless.
#[test]
fn only_a_flow_level_failure_announces_itself() {
    let mut chat = chat_with_bell();

    chat.handle(UiEvent::TurnStart);
    let _ = chat.notify.take();
    chat.handle(UiEvent::Error {
        code: "SERVER_ERROR",
        msg: "model list unavailable".into(),
        level: crate::error::ErrorLevel::Page,
        context: crate::error::ErrorContext::ShortSync,
    });
    assert!(
        emitted(&mut chat).is_empty(),
        "a page-level error keeps the busy title and stays quiet"
    );

    let idle = idle_title(&chat);
    chat.handle(UiEvent::Error {
        code: "TIMEOUT",
        msg: "long turn interrupted".into(),
        level: crate::error::ErrorLevel::Full,
        context: crate::error::ErrorContext::LongTurn,
    });
    assert_eq!(
        emitted(&mut chat),
        format!("\x07{idle}"),
        "the failure rings, and the title stops claiming work is in progress"
    );
}

/// The whole channel is opt-out in one key: a chat left with the default
/// notifier drives every trigger and writes nothing.
#[test]
fn the_default_chat_is_silent_on_every_trigger() {
    let mut chat = crate::tui::test_util::chat_at(80, 24);
    let (tx, _rx) = tokio::sync::oneshot::channel();
    chat.asks
        .send((
            PermissionRequest::new("Allow running Bash", "cargo build", vec!["Allow".into()]),
            tx,
        ))
        .unwrap();
    assert!(chat.drain_asks());
    chat.handle(UiEvent::TurnStart);
    age_the_turn(&mut chat);
    chat.handle(UiEvent::TurnEnd);
    chat.handle(UiEvent::Error {
        code: "TIMEOUT",
        msg: "long turn interrupted".into(),
        level: crate::error::ErrorLevel::Full,
        context: crate::error::ErrorContext::LongTurn,
    });
    assert!(chat.notify.take().is_empty());
}

/// Chrome rows above/below the transcript, as plain text.
fn chrome_text(chat: &Chat) -> String {
    crate::tui::el::render(crate::tui::chrome::chrome(chat, 100, false))
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n")
}

/// D80: the layer stack is one ordered list, and Esc peels it one entry per
/// press. The busy interrupt is an entry like any other, so everything stacked
/// above it closes first and the turn survives.
#[test]
fn esc_peels_one_layer_per_press_before_it_reaches_the_turn() {
    let mut chat = test_chat();
    chat.busy = true;
    chat.help_visible = true;
    chat.push_slash_info("session status".to_string());
    chat.set_input("/");
    assert!(!chat.slash_suggestions.is_empty(), "dropdown open");

    let t0 = std::time::Instant::now();
    let mut order = Vec::new();
    while let Some(layer) = chat.esc_layer() {
        order.push(layer);
        assert!(chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0));
        if layer == EscLayer::Interrupt {
            break;
        }
        assert!(
            !chat.interrupted,
            "a layer above the interrupt closed instead of the turn: {layer:?}"
        );
        assert!(chat.busy, "the turn kept running through {layer:?}");
    }
    assert_eq!(
        order,
        vec![
            EscLayer::SlashDropdown,
            EscLayer::InfoLines,
            EscLayer::HelpPanel,
            EscLayer::Interrupt,
        ],
        "the stack is walked top-down, one entry per press"
    );
    assert!(chat.interrupted, "the last press reached the turn");
}

/// The dropdown closes and the turn keeps running; the status row says so
/// while the layer is up, and goes back to promising the interrupt after.
#[test]
fn esc_over_a_busy_turn_closes_the_dropdown_and_says_so() {
    let mut chat = test_chat();
    chat.busy = true;
    chat.set_input("/");
    assert_eq!(chat.esc_busy_hint(), "esc to close");

    let t0 = std::time::Instant::now();
    assert!(chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0));
    assert!(chat.slash_suggestions.is_empty(), "the dropdown closed");
    assert!(chat.busy && !chat.interrupted, "the turn is still running");
    assert_eq!(chat.esc_busy_hint(), "esc to interrupt");

    assert!(chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0));
    assert!(chat.interrupted, "with nothing left open, Esc interrupts");
}

/// Info lines are reading the user asked for; Esc dismisses the reading, not
/// the work.
#[test]
fn esc_clears_info_lines_without_ending_the_turn() {
    let mut chat = test_chat();
    chat.busy = true;
    chat.push_slash_info("context: 12k/200k".to_string());

    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(chat.slash_info_lines.is_empty(), "the info block cleared");
    assert!(chat.busy && !chat.interrupted, "the turn is still running");
}

/// Ctrl+C is the one key the layers do not shield: it interrupts with the
/// dialog still on screen.
#[test]
fn an_interrupt_settles_the_dialog_it_leaves_behind() {
    let mut chat = test_chat();
    let (tx, mut rx) = tokio::sync::oneshot::channel();
    chat.asks
        .send((
            PermissionRequest::new("Allow running Bash", "rm -rf /", vec!["Allow".into()]),
            tx,
        ))
        .unwrap();
    assert!(chat.drain_asks());
    chat.busy = true;
    assert!(chrome_text(&chat).contains("Waiting for permission…"));

    assert!(chat.on_key(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(chat.interrupted, "ctrl+c still interrupts through a dialog");
    assert!(chat.pending_ask.is_none(), "the dialog went with the turn");
    assert_eq!(
        rx.try_recv(),
        Ok(DialogAction::Cancel),
        "the waiting side is told, not left hanging"
    );
    assert!(
        !chrome_text(&chat).contains("Waiting for permission…"),
        "the footer stops claiming a question is open"
    );
    let flow = visible(&mut chat, 100, 40);
    assert!(
        flow.contains(crate::tui::chat::ASK_CANCELLED_TEXT),
        "the block settles as cancelled: {flow}"
    );

    // The keys that answered the dialog are ordinary composer keys again, and
    // the dropped receiver is never sent to a second time.
    drop(rx);
    assert!(chat.on_key(KeyCode::Char('1'), KeyModifiers::empty()));
    assert_eq!(chat.input, "1", "the digit is text now, not an answer");
    assert!(chat.on_key(KeyCode::Enter, KeyModifiers::empty()));
    assert!(chat.pending_ask.is_none(), "nothing came back to life");
    assert_eq!(
        chat.queued.last().map(|q| q.text.as_str()),
        Some("1"),
        "Enter queued a message instead of confirming an option"
    );
}

/// A turn that died on its own takes its dialog and everything queued behind it.
/// The receiver being gone is what marks a request as the dead turn's — a
/// background agent's question is still live, and stays.
#[test]
fn turn_end_settles_the_asks_whose_turn_is_gone() {
    let mut chat = test_chat();
    let (dead_tx, dead_rx) = tokio::sync::oneshot::channel();
    let (live_tx, _live_rx) = tokio::sync::oneshot::channel();
    chat.asks
        .send((
            PermissionRequest::new("Allow running Bash", "cargo test", vec!["Allow".into()]),
            dead_tx,
        ))
        .unwrap();
    chat.asks
        .send((
            PermissionRequest::new("Allow running Edit", "src/main.rs", vec!["Allow".into()]),
            live_tx,
        ))
        .unwrap();
    assert!(chat.drain_asks(), "the first request is on screen");
    chat.busy = true;
    drop(dead_rx); // the turn awaiting the answer is gone

    chat.handle(UiEvent::TurnEnd);

    assert!(chat.pending_ask.is_none(), "the dead dialog is settled");
    let flow = visible(&mut chat, 100, 40);
    assert!(
        flow.contains(crate::tui::chat::ASK_CANCELLED_TEXT),
        "the block settles as cancelled: {flow}"
    );
    assert!(
        chat.drain_asks(),
        "the request still being waited on stays in the queue"
    );
    assert!(
        chat.pending_ask
            .as_ref()
            .is_some_and(|(r, _)| r.title == "Allow running Edit"),
        "and surfaces next"
    );
}

/// The `!` prefix is sticky, so a running bash command always sits under an
/// empty bash-mode composer: Esc has to reach the command, not the prefix.
#[test]
fn esc_stops_a_running_bash_command_before_it_leaves_bash_mode() {
    let mut chat = test_chat();
    chat.bash_mode = true;
    chat.busy = true;

    assert_eq!(chat.esc_layer(), Some(EscLayer::Interrupt));
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(chat.interrupted, "the command stops first");
    assert!(chat.bash_mode, "and the prompt is still a shell prompt");

    chat.busy = false;
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(!chat.bash_mode, "idle, the same key leaves bash mode");
}

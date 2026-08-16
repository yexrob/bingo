//! Chat state-machine tests, part three: the collapse fold's own contract.
//!
//! `chat_tests_a` / `chat_tests_b` split by size alone (the 4000-line file cap); this
//! file continues them.

use super::chat_tail::EscLayer;
use super::tests_a::*;
use super::*;
use serde_json::json;

/// Register an instance on the chat's own session, the way a spawn would.
fn seed_agent(chat: &Chat, name: &str) {
    chat.session.agents.insert(
        name,
        crate::agents::AgentKind::Hire,
        None,
        "test instance".to_string(),
        chat.session.clone(),
    );
}

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

    assert!(chat.expand_all_folds(), "ctrl+o opens the fold");
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

    assert!(chat.expand_all_folds(), "ctrl+o opens the fold");
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

/// D85: the `@` dropdown joined the stack in the slash dropdown's stratum, so
/// the same walk holds with a mention on screen instead of a command query.
/// The two can never be open together — `update_slash_suggestions` hands the
/// composer to exactly one of them — which is why this is a second walk rather
/// than a longer one.
#[test]
fn esc_peels_the_mention_dropdown_in_the_slash_dropdowns_place() {
    assert_eq!(
        EscLayer::ORDER
            .iter()
            .position(|layer| *layer == EscLayer::MentionDropdown),
        EscLayer::ORDER
            .iter()
            .position(|layer| *layer == EscLayer::SlashDropdown)
            .map(|i| i + 1),
        "the two completion surfaces are adjacent"
    );

    // Its own empty project dir: the file source has a bounded, known answer
    // instead of whatever the shared temp directory happens to hold.
    let root = std::env::temp_dir().join(format!("bingo-d85-{}-peel", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let _ = std::fs::create_dir_all(&root);
    let mut chat = test_chat_home(root.clone());
    chat.busy = true;
    chat.help_visible = true;
    chat.push_slash_info("session status".to_string());
    chat.set_input("read @");
    assert!(
        chat.mention.is_some(),
        "an empty project still opens the layer"
    );

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
            EscLayer::MentionDropdown,
            EscLayer::InfoLines,
            EscLayer::HelpPanel,
            EscLayer::Interrupt,
        ],
        "the mention peels where the slash dropdown would have"
    );
    assert!(chat.interrupted, "the last press reached the turn");
    let _ = std::fs::remove_dir_all(&root);
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

// D81 — the approval dialog, in CC's three-option shape.

use crate::query::AskOutcome;
use crate::tui::chat::{
    ASK_RECEIPT_NO, ASK_RECEIPT_NO_PREFIX, ASK_RECEIPT_SESSION, ASK_RECEIPT_YES,
};
use crate::ui::{ASK_NO, ASK_YES, ASK_YES_SESSION};

/// Drive a real permission prompt through `modal_ask`: what the tests read is
/// the dialog the gate's own hook builds, not one a test assembled by hand.
/// The request is on the queue as soon as this returns; the future settles when
/// the dialog does.
fn gate_asks(
    chat: &Chat,
    tool: &str,
    reason: &str,
    input: &serde_json::Value,
    scope: Option<&str>,
    diff: Option<&str>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = AskOutcome> + Send>> {
    let ask = crate::ui::modal_ask(chat.asks.clone());
    ask(&crate::query::AskContext {
        tool,
        reason,
        input,
        cwd: &std::env::temp_dir(),
        scope,
        diff,
    })
}

/// A dialog opened `now`, with the type-ahead guard already behind it.
fn past_guard(chat: &mut Chat) -> std::time::Instant {
    let opened = std::time::Instant::now();
    chat.ask_opened_at = Some(opened);
    opened + crate::tui::chat::ask::ASK_CONFIRM_GUARD + std::time::Duration::from_millis(1)
}

/// The approval prompt is CC's, word for word, and it shows the command before
/// it asks about it.
#[tokio::test]
async fn the_approval_prompt_offers_the_three_options_verbatim() {
    let mut chat = test_chat();
    let input = json!({ "command": "cargo test --locked" });
    let verdict = gate_asks(
        &chat,
        "Bash",
        "Bash needs permission",
        &input,
        Some("Bash(cargo:*)"),
        None,
    );
    assert!(chat.drain_asks());
    let flow = visible(&mut chat, 100, 40);
    assert!(flow.contains("Allow running Bash"), "{flow}");
    assert!(flow.contains(&format!("1. {ASK_YES}")), "{flow}");
    assert!(flow.contains(&format!("2. {ASK_YES_SESSION}")), "{flow}");
    assert!(flow.contains(&format!("3. {ASK_NO}")), "{flow}");
    assert!(
        flow.contains("$ cargo test --locked"),
        "the command is on screen before it is approved: {flow}"
    );

    let now = past_guard(&mut chat);
    assert!(chat.on_key_at(KeyCode::Char('1'), KeyModifiers::empty(), now));
    assert_eq!(verdict.await, AskOutcome::Allow);
    assert!(
        visible(&mut chat, 100, 40).contains(ASK_RECEIPT_YES),
        "the choice stays in the flow after the dialog goes"
    );
}

/// Type-ahead must not answer a question that was not on screen when the key
/// was pressed.
#[tokio::test]
async fn enter_inside_the_guard_window_answers_nothing() {
    let mut chat = test_chat();
    let input = json!({ "command": "rm -rf build" });
    let verdict = gate_asks(&chat, "Bash", "Bash needs permission", &input, None, None);
    assert!(chat.drain_asks());

    let opened = std::time::Instant::now();
    chat.ask_opened_at = Some(opened);
    assert!(
        chat.on_key_at(KeyCode::Enter, KeyModifiers::empty(), opened),
        "the key is swallowed by the dialog"
    );
    assert!(
        chat.pending_ask.is_some(),
        "but it answers nothing: the question is still open"
    );

    let now =
        opened + crate::tui::chat::ask::ASK_CONFIRM_GUARD + std::time::Duration::from_millis(1);
    assert!(chat.on_key_at(KeyCode::Enter, KeyModifiers::empty(), now));
    assert_eq!(
        verdict.await,
        AskOutcome::Allow,
        "past the window it confirms"
    );
}

/// shift+tab reaches the session option from anywhere in the dialog — and,
/// without one offered, the dialog swallows the key rather than cycling the
/// permission mode behind an unanswered question.
#[tokio::test]
async fn shift_tab_takes_the_session_option() {
    let mut chat = test_chat();
    let input = json!({ "command": "cargo build" });
    let verdict = gate_asks(
        &chat,
        "Bash",
        "Bash needs permission",
        &input,
        Some("Bash(cargo:*)"),
        None,
    );
    assert!(chat.drain_asks());
    chat.ask_focus = 2;
    assert!(chat.on_key(KeyCode::BackTab, KeyModifiers::empty()));
    assert_eq!(verdict.await, AskOutcome::AllowSession);
    assert!(visible(&mut chat, 100, 40).contains(ASK_RECEIPT_SESSION));

    let mode = chat.session.permission_mode;
    let verdict = gate_asks(&chat, "Bash", "Bash needs permission", &input, None, None);
    assert!(chat.drain_asks());
    let flow = visible(&mut chat, 100, 40);
    assert!(
        !flow.contains(ASK_YES_SESSION),
        "no session option when no rule could keep the promise: {flow}"
    );
    assert!(chat.on_key(KeyCode::BackTab, KeyModifiers::empty()));
    assert!(chat.pending_ask.is_some(), "the dialog is still open");
    assert_eq!(
        chat.session.permission_mode, mode,
        "and the permission mode behind it did not cycle"
    );
    drop(verdict);
}

/// The refusal option collects what to do instead, and the model reads it.
#[tokio::test]
async fn a_refusal_collects_feedback_for_the_model() {
    let mut chat = test_chat();
    let input = json!({ "command": "git push --force" });
    let verdict = gate_asks(&chat, "Bash", "Bash needs permission", &input, None, None);
    assert!(chat.drain_asks());

    let now = past_guard(&mut chat);
    assert!(chat.on_key_at(KeyCode::Char('2'), KeyModifiers::empty(), now));
    assert!(
        chat.pending_ask.is_some(),
        "the refusal opens a feedback row instead of resolving"
    );
    for c in "open a PR".chars() {
        assert!(chat.on_key_at(KeyCode::Char(c), KeyModifiers::empty(), now));
    }
    assert!(chat.on_key_at(KeyCode::Enter, KeyModifiers::empty(), now));
    assert_eq!(
        verdict.await,
        AskOutcome::Deny {
            feedback: Some("open a PR".to_string())
        }
    );
    assert!(
        visible(&mut chat, 100, 40).contains(&format!("{ASK_RECEIPT_NO_PREFIX}open a PR")),
        "the transcript records what was asked for instead"
    );
}

/// Every way of saying no without saying more is the same plain deny.
#[tokio::test]
async fn esc_and_an_empty_feedback_submit_are_both_a_plain_deny() {
    let mut chat = test_chat();
    let input = json!({ "command": "rm -rf /" });
    let verdict = gate_asks(&chat, "Bash", "Bash needs permission", &input, None, None);
    assert!(chat.drain_asks());
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert_eq!(verdict.await, AskOutcome::Deny { feedback: None });
    assert!(visible(&mut chat, 100, 40).contains(ASK_RECEIPT_NO));

    // Empty submit from the feedback row.
    let verdict = gate_asks(&chat, "Bash", "Bash needs permission", &input, None, None);
    assert!(chat.drain_asks());
    let now = past_guard(&mut chat);
    assert!(chat.on_key_at(KeyCode::Char('2'), KeyModifiers::empty(), now));
    assert!(chat.on_key_at(KeyCode::Enter, KeyModifiers::empty(), now));
    assert_eq!(verdict.await, AskOutcome::Deny { feedback: None });

    // Esc from the feedback row.
    let verdict = gate_asks(&chat, "Bash", "Bash needs permission", &input, None, None);
    assert!(chat.drain_asks());
    let now = past_guard(&mut chat);
    assert!(chat.on_key_at(KeyCode::Char('2'), KeyModifiers::empty(), now));
    assert!(chat.on_key_at(KeyCode::Char('x'), KeyModifiers::empty(), now));
    assert!(chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), now));
    assert_eq!(verdict.await, AskOutcome::Deny { feedback: None });
}

/// An edit is approved against the change it would make, computed without
/// touching the file.
#[tokio::test]
async fn an_edit_prompt_shows_the_change_it_would_make_without_making_it() {
    use crate::tool::Tool;
    let root = std::env::temp_dir().join(format!("bingo-ask-preview-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("preview.txt");
    let before = "alpha\nbeta\ngamma\n";
    std::fs::write(&file, before).unwrap();

    let input = json!({
        "file_path": file.to_string_lossy(),
        "old_string": "beta",
        "new_string": "delta",
    });
    let diff = crate::tool::edit::EditTool
        .preview_diff(&input, &root)
        .expect("a matching edit previews as a diff");
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        before,
        "the dry run did not write"
    );

    let mut chat = test_chat();
    let verdict = gate_asks(
        &chat,
        "Edit",
        "Edit needs permission (destructive)",
        &input,
        Some("Edit(/tmp/)"),
        Some(&diff),
    );
    assert!(chat.drain_asks());
    let flow = visible(&mut chat, 100, 40);
    assert!(flow.contains("-beta"), "the removal is shown: {flow}");
    assert!(flow.contains("+delta"), "the addition is shown: {flow}");

    let now = past_guard(&mut chat);
    assert!(chat.on_key_at(KeyCode::Enter, KeyModifiers::empty(), now));
    assert_eq!(verdict.await, AskOutcome::Allow);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        before,
        "and approving is not what writes either — the tool still has to run"
    );
}

/// A preview long enough to bury the question is bounded, and ctrl+e is the way
/// to see the rest.
#[tokio::test]
async fn ctrl_e_toggles_the_bounded_preview() {
    let mut chat = test_chat();
    let command = (1..=9)
        .map(|n| format!("echo line{n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let input = json!({ "command": command });
    let verdict = gate_asks(
        &chat,
        "Bash",
        "Bash needs permission",
        &input,
        Some("Bash(echo:*)"),
        None,
    );
    assert!(chat.drain_asks());

    let flow = visible(&mut chat, 100, 40);
    assert!(flow.contains("$ echo line6"), "{flow}");
    assert!(!flow.contains("$ echo line7"), "bounded collapsed: {flow}");
    assert!(flow.contains("… 3 more lines"), "{flow}");
    assert!(flow.contains("ctrl+e to expand"), "{flow}");

    assert!(chat.on_key(KeyCode::Char('e'), KeyModifiers::CONTROL));
    let flow = visible(&mut chat, 100, 40);
    assert!(flow.contains("$ echo line9"), "expanded shows all: {flow}");
    assert!(
        flow.contains("session rule: Bash(echo:*)"),
        "and spells out the promise option 2 makes: {flow}"
    );
    assert!(flow.contains("ctrl+e to collapse"), "{flow}");

    assert!(chat.on_key(KeyCode::Char('e'), KeyModifiers::CONTROL));
    assert!(
        !visible(&mut chat, 100, 40).contains("$ echo line9"),
        "and folds back"
    );
    drop(verdict);
}

/// AskUserQuestion shares the queue and the keys, and none of D81 reaches it:
/// its own options stand, its Other row is numbered, and no guard delays it.
#[tokio::test]
async fn ask_user_question_keeps_its_own_shape() {
    let mut chat = test_chat();
    let (events_tx, _events_rx) = tokio::sync::mpsc::unbounded_channel();
    let ui = crate::ui::tui_hooks(
        events_tx,
        chat.asks.clone(),
        chat.steer.clone(),
        chat.live.clone(),
    );
    let answer = (ui.ask_question)(
        "Tech stack".to_string(),
        "Which library?".to_string(),
        vec![("A".to_string(), None), ("B".to_string(), None)],
    );
    assert!(chat.drain_asks());
    let flow = visible(&mut chat, 100, 40);
    assert!(flow.contains("1. A") && flow.contains("2. B"), "{flow}");
    assert!(
        flow.contains("3. Other"),
        "the Other row keeps its number: {flow}"
    );
    assert!(!flow.contains(ASK_YES), "{flow}");

    // No guard: the answer lands on the first press, as it always did.
    assert!(chat.on_key(KeyCode::Char('2'), KeyModifiers::empty()));
    assert_eq!(answer.await, Some(crate::query::AskAnswer::Option(1)));
    let flow = visible(&mut chat, 100, 40);
    assert!(flow.contains("· Which library? → B"), "{flow}");
    assert!(
        !flow.contains(ASK_RECEIPT_YES),
        "no permission receipt: {flow}"
    );
}

/// D83: what the composer offers the running turn. A plain message can change what the
/// turn does next; a slash command runs on this side and has nothing to say to it.
#[test]
fn only_plain_messages_are_offered_to_the_running_turn() {
    let mut chat = chat_with_history("steer-offer");
    chat.busy = true;
    chat.set_input("use tabs");
    chat.submit();
    assert_eq!(
        chat.steer.take(),
        vec![crate::steer::SteerItem {
            id: 0,
            text: "use tabs".into()
        }],
        "a plain message is on offer at the next barrier"
    );

    let mut chat = chat_with_history("steer-offer-slash");
    chat.busy = true;
    chat.set_input("/clear");
    chat.submit();
    assert!(
        chat.steer.is_empty(),
        "a slash command stays for TurnEnd: it is dispatched here, not by the turn"
    );
    assert_eq!(chat.queued.len(), 1, "and it is still queued");

    // Order survives: a plain message typed behind a slash command must not overtake it.
    chat.set_input("and then this");
    chat.submit();
    assert!(
        chat.steer.is_empty(),
        "a message queued behind a slash command waits with it"
    );
}

/// The absorbed message leaves the queue, lands in the flow under `↪`, and `↑` cannot
/// bring it back — it is in the request already, and typing it again would send it twice.
#[test]
fn an_absorbed_message_moves_from_the_queue_into_the_flow() {
    let mut chat = chat_with_history("steer-absorb");
    chat.busy = true;
    chat.set_input("first");
    chat.submit();
    chat.set_input("second");
    chat.submit();
    assert_eq!(chat.queued.len(), 2);

    // The barrier takes what is on offer, exactly as `tui_hooks` does.
    let taken = chat.steer.take();
    assert_eq!(taken.len(), 2);
    let _ = chat.events.send(UiEvent::Steered { items: taken });
    chat.drain_events();

    assert!(
        chat.queued.is_empty(),
        "absorbed messages stop being pending"
    );
    assert!(
        chat.queue_lines().is_empty(),
        "and stop being rendered as such"
    );
    let flow = visible(&mut chat, 100, 40);
    assert!(
        flow.contains("↪ first") && flow.contains("↪ second"),
        "both land in the flow under the steering marker: {flow}"
    );
    assert!(
        !flow.contains("❯ first"),
        "not as a `❯` bubble — it landed inside a turn, not at the prompt: {flow}"
    );

    // ↑ on an empty input now finds nothing to pull back.
    press(&mut chat, KeyCode::Up);
    assert_ne!(
        chat.input, "second",
        "an absorbed message is not resurrected"
    );
}

/// The race: the turn took the message between the composer offering it and the user
/// pressing ↑. The turn wins; the pull-back is a no-op and the absorption event, still
/// in flight, is what takes it out of the queue.
#[test]
fn a_pull_back_that_lost_the_race_does_nothing() {
    let mut chat = chat_with_history("steer-race");
    chat.busy = true;
    chat.set_input("too late");
    chat.submit();
    let taken = chat.steer.take();
    assert_eq!(taken.len(), 1);

    press(&mut chat, KeyCode::Up);
    assert_eq!(chat.input, "", "the composer is left alone");
    assert_eq!(chat.queued.len(), 1, "the queue waits for the event");

    let _ = chat.events.send(UiEvent::Steered { items: taken });
    chat.drain_events();
    assert!(chat.queued.is_empty(), "which then removes it");
}

/// CC's wording, and only while it is true: with no turn running the queue is about to
/// submit itself, and there is no window in which editing it would mean anything.
#[test]
fn the_queue_hint_shows_exactly_while_a_busy_turn_holds_a_queue() {
    let mut chat = chat_with_history("steer-hint");
    assert_eq!(chat.queue_hint(), None, "idle and empty");
    chat.busy = true;
    assert_eq!(chat.queue_hint(), None, "busy with nothing queued");
    chat.set_input("later");
    chat.submit();
    assert_eq!(chat.queue_hint(), Some("Press up to edit queued messages"));
    chat.busy = false;
    assert_eq!(
        chat.queue_hint(),
        None,
        "the turn ended: the queue is going out"
    );
}

/// The channel belongs to one turn. A message the ended turn never took must not be
/// folded into the next one behind the user's back — it goes out as a turn of its own.
#[tokio::test]
async fn the_channel_is_re_armed_for_the_turn_that_actually_runs() {
    let mut chat = chat_with_history("steer-rearm");
    chat.busy = true;
    chat.set_input("one");
    chat.submit();
    chat.set_input("two");
    chat.submit();
    assert_eq!(chat.steer.take().len(), 2);

    // TurnEnd: "one" starts the next turn, and only what is still queued is on offer.
    chat.busy = false;
    chat.submit_queued();
    chat.rearm_steer();
    assert!(chat.busy, "the first queued message opened a turn");
    assert_eq!(
        chat.steer.take(),
        vec![crate::steer::SteerItem {
            id: 1,
            text: "two".into()
        }],
        "the message that opened the turn is not also steered into it"
    );
}

/// A Bash tool call in flight, exactly as the query layer announces one.
/// `standalone`: the `!` shell mode's own call, which never joins a fold group.
fn running_bash(chat: &mut Chat, command: &str, standalone: bool) {
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    let _ = chat.events.send(UiEvent::ToolStart {
        name: "Bash".into(),
    });
    chat.drain_events();
    let _ = chat.events.send(UiEvent::ToolReady {
        tool_call_id: "bash-1".into(),
        name: "Bash".into(),
        input: json!({ "command": command }),
        standalone,
    });
    chat.drain_events();
}

fn tail(chat: &mut Chat, lines: &[&str], total_lines: usize) {
    let _ = chat.events.send(UiEvent::BashTail(crate::live::LiveTail {
        lines: lines.iter().map(|l| (*l).to_string()).collect(),
        total_lines,
    }));
    chat.drain_events();
}

/// D84: a folded Bash call gets one row — the command — and used to say nothing
/// else until it exited. Its output now hangs under that row while it runs, and
/// leaves with it.
#[test]
fn a_folded_command_shows_its_output_while_it_runs() {
    let mut chat = test_chat();
    running_bash(&mut chat, "cargo build --release", false);
    assert!(
        visible(&mut chat, 120, 40).contains("cargo build --release"),
        "the folded row names the command"
    );

    tail(
        &mut chat,
        &["Compiling bingo v0.4.0", "Compiling serde v1"],
        2,
    );
    let screen = visible(&mut chat, 120, 40);
    assert!(screen.contains("Compiling bingo v0.4.0"), "{screen}");
    assert!(screen.contains("Compiling serde v1"), "{screen}");
    assert!(
        !screen.contains("lines"),
        "nothing is being left out yet: {screen}"
    );

    // A later sample replaces the rows rather than adding to them, and says how
    // much of the output it is not showing.
    tail(
        &mut chat,
        &["Compiling a", "Compiling b", "Compiling c", "Compiling d"],
        128,
    );
    let screen = visible(&mut chat, 120, 40);
    assert!(
        !screen.contains("Compiling bingo v0.4.0"),
        "the tail is replaced, not appended: {screen}"
    );
    assert!(screen.contains("Compiling d"), "{screen}");
    assert!(screen.contains("… 128 lines"), "{screen}");

    let _ = chat
        .events
        .send(UiEvent::ToolDone(crate::query::ToolCallDone {
            tool_call_id: "bash-1".into(),
            name: "Bash".into(),
            summary: "cargo build --release".into(),
            output: "Finished release".into(),
            status: crate::query::ToolCallStatus::Done,
            duration_ms: 10,
            diff: None,
        }));
    chat.drain_events();
    let screen = visible(&mut chat, 120, 40);
    assert!(
        !screen.contains("Compiling d") && !screen.contains("… 128 lines"),
        "the finished call's own result row takes over: {screen}"
    );
}

/// The `!` shell command is standalone (no fold group), and its running row is
/// the activity's own — the tail has to find that one too.
#[test]
fn a_standalone_command_shows_its_output_under_its_own_row() {
    let mut chat = test_chat();
    running_bash(&mut chat, "pytest -q", true);
    tail(&mut chat, &["collected 40 items", "test_one PASSED"], 2);
    let screen = visible(&mut chat, 120, 40);
    assert!(screen.contains("Running…"), "{screen}");
    assert!(screen.contains("test_one PASSED"), "{screen}");
}

/// Long output belongs to the terminal width, not to the command: a tail row
/// never wraps into a second row (the tail region's height would drift).
#[test]
fn a_long_tail_line_is_clipped_to_the_width() {
    let mut chat = test_chat();
    running_bash(&mut chat, "cargo test", false);
    tail(&mut chat, &["x".repeat(400).as_str()], 1);
    for row in chat.bash_tail_rows(60) {
        assert!(
            crate::tui::line::text_width(&row.plain_text()) <= 60,
            "row overflows the width: {}",
            row.plain_text()
        );
    }
}

/// D84: ctrl+b reads the situation. A command running in the foreground is what
/// it backgrounds; with none running it keeps opening the dialog (D80, D107).
#[test]
fn ctrl_b_backgrounds_the_running_command_before_it_opens_the_dialog() {
    let mut chat = test_chat();
    running_bash(&mut chat, "cargo build", false);
    tail(&mut chat, &["Compiling bingo"], 1);
    let (run, mut promote) = chat.live.arm();
    assert!(chat.live.running());

    assert!(chat.on_key(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert!(
        *promote.borrow_and_update(),
        "the running command was told to go to the background"
    );
    assert!(
        chat.dialog.is_none(),
        "the dialog stays closed while a command owns the key"
    );
    assert!(
        chat.bash_tail.is_none(),
        "the tail leaves with the row it hung under"
    );
    assert!(!chat.live.running(), "a second press means something else");
    drop(run);

    assert!(chat.on_key(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert!(
        chat.dialog.is_some(),
        "with nothing running, ctrl+b is the background dialog"
    );
}

/// The offer is only made while it is true.
#[test]
fn the_status_hint_offers_ctrl_b_only_while_a_command_runs() {
    let chat = test_chat();
    assert_eq!(chat.busy_hint(), "esc to interrupt");
    let (run, _promote) = chat.live.arm();
    assert_eq!(
        chat.busy_hint(),
        "esc to interrupt · ctrl+b to run in background"
    );
    drop(run);
    assert_eq!(chat.busy_hint(), "esc to interrupt");
}

// ---------------------------------------------------------------------------
// D87 — the motion layer's two wired surfaces that are not the status row:
// the terminal title's working animation, and the completion blink.
// ---------------------------------------------------------------------------

/// While a turn runs the title marker cycles, slowly and on a throttle: about
/// one write per 960ms, and never a repeat of what the tab already says.
#[test]
fn the_busy_title_animates_once_a_second_and_repeats_nothing() {
    let mut chat = chat_with_bell();
    chat.handle(UiEvent::TurnStart);
    assert_eq!(
        emitted(&mut chat),
        "\x1b]2;✳ bingo — working…\x07",
        "the turn opens on the resting marker"
    );

    // Four seconds of frames produce four title changes, not a hundred and
    // twenty: the tab is a surface for a user looking at another window.
    let mut writes = Vec::new();
    let four_seconds = 4_000 / crate::tui::motion::TICK_MS;
    for _ in 0..four_seconds {
        chat.tick();
        let out = emitted(&mut chat);
        if !out.is_empty() {
            writes.push(out);
        }
    }
    assert_eq!(
        writes,
        vec![
            "\x1b]2;⠂ bingo — working…\x07".to_string(),
            "\x1b]2;✳ bingo — working…\x07".to_string(),
            "\x1b]2;⠐ bingo — working…\x07".to_string(),
            "\x1b]2;✳ bingo — working…\x07".to_string(),
        ],
        "one write per frame change, and the marker comes back between them"
    );

    // Motion off: the same title, held still, for as long as the turn runs.
    let mut still = chat_with_bell();
    still.motion = crate::tui::motion::Motion::new(false);
    still.handle(UiEvent::TurnStart);
    assert_eq!(emitted(&mut still), "\x1b]2;✳ bingo — working…\x07");
    for _ in 0..200 {
        still.tick();
    }
    assert!(
        emitted(&mut still).is_empty(),
        "a still title writes nothing after the first"
    );
}

/// A permission prompt outranks the animation: the title says what the session
/// is waiting for until it is answered, and no frame overwrites it.
#[test]
fn a_pending_prompt_keeps_the_title_it_needs() {
    let mut chat = chat_with_bell();
    chat.handle(UiEvent::TurnStart);
    let _ = emitted(&mut chat);
    let (tx, _rx) = tokio::sync::oneshot::channel();
    chat.asks
        .send((
            PermissionRequest::new("Allow running Bash", "cargo build", vec!["Allow".into()]),
            tx,
        ))
        .unwrap();
    assert!(chat.drain_asks());
    let _ = emitted(&mut chat);
    for _ in 0..200 {
        chat.tick();
    }
    assert!(
        emitted(&mut chat).is_empty(),
        "the waiting title is not animated over"
    );
}

/// D103: ctrl+k is readline's kill again. D90 spent it on the conversation
/// switcher and moved the kill to alt+k; the switcher retired with the
/// conversations it switched between, so the key comes back — and alt+k stays
/// an alias, because taking a binding back twice is worse than keeping two.
#[test]
fn both_kill_keys_kill_to_the_end_of_the_line() {
    for key in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        let mut chat = test_chat();
        chat.set_input("alpha beta");
        chat.cursor = 6;
        assert!(chat.on_key(KeyCode::Char('k'), key));
        assert_eq!(chat.input, "alpha ", "{key:?} kills to the end of the line");
        assert!(chat.on_key(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(chat.input, "alpha beta", "and the kill fed the ring");
    }
}

/// D103: steering offers the running turn what the composer submitted. A
/// direct send is addressed to a subagent, so it must not reach the model's
/// turn — neither as a steer nor as a queued item behind it.
#[tokio::test]
async fn a_direct_send_never_steers_mains_turn() {
    let mut chat = chat_with_history("steer-direct");
    chat.session.agents.insert(
        "scout",
        crate::agents::AgentKind::Hire,
        None,
        "research".into(),
        chat.session.clone(),
    );
    chat.refresh_conversations();
    chat.busy = true;

    chat.set_input("@scout use tabs");
    chat.submit();

    assert!(
        chat.steer.is_empty(),
        "a direct send is not the turn's to read"
    );
    assert!(chat.queued.is_empty(), "and it is not waiting for TurnEnd");
    assert!(chat.busy, "the turn is untouched");
    assert!(
        chat.session
            .agents
            .pending_of("scout")
            .iter()
            .any(|(from, text)| from == crate::channels::USER_NAME && text == "use tabs")
            || !chat.session.agents.take_running("scout", 0).is_empty(),
        "it went to the instance instead"
    );

    // The same text without the envelope is on offer at the next barrier.
    chat.set_input("use tabs");
    chat.submit();
    assert_eq!(
        chat.steer.take(),
        vec![crate::steer::SteerItem {
            id: 0,
            text: "use tabs".into()
        }],
        "main's composer still steers"
    );
}

/// D91: the rewind selector is in the stack's Menu stratum, and Esc peels its
/// two stages one press at a time before anything under it moves — the turn
/// keeps running throughout.
#[test]
fn esc_peels_the_rewind_selector_one_stage_at_a_time() {
    use crate::tui::chat::rewind_ui::Rewind;

    assert_eq!(
        EscLayer::ORDER
            .iter()
            .position(|layer| *layer == EscLayer::Rewind),
        EscLayer::ORDER
            .iter()
            .position(|layer| *layer == EscLayer::Menu)
            .map(|i| i + 1),
        "rewind sits just under the pickers, in the same stratum"
    );

    let mut chat = test_chat();
    chat.busy = true;
    chat.help_visible = true;
    chat.rewind = Some(Rewind {
        points: vec![crate::rewind::Checkpoint {
            line: 1,
            index: 0,
            label: "a question".to_string(),
            text: "a question".to_string(),
            at: 1_700_000_000,
            coverage: Default::default(),
        }],
        selected: 0,
        action: Some(0),
    });

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
            EscLayer::Rewind,
            EscLayer::Rewind,
            EscLayer::HelpPanel,
            EscLayer::Interrupt,
        ],
        "the action list returns to the turn list before the selector closes"
    );
}

/// D95's slot, D104's occupant: the ctrl+t cycle's second stop joined the stack
/// directly above the task panel it cycles with, as its own layer rather than a
/// second meaning for the task panel's, so `ORDER` — the single source of Esc's
/// priority — can still answer which of the two a press closes.
///
/// Rewritten for D104: the second stop is the agent tree, and the assertion it
/// makes is the one it always made (adjacency, and that every variant is
/// reachable) plus the tree's own two-stage peel, which the directory never had.
#[test]
fn esc_peels_the_agent_tree_in_the_slot_above_the_task_panel() {
    let at = |wanted: EscLayer| {
        EscLayer::ORDER
            .iter()
            .position(|layer| *layer == wanted)
            .unwrap_or_else(|| panic!("{wanted:?} is in the stack"))
    };
    assert_eq!(
        at(EscLayer::Roster) + 1,
        at(EscLayer::TaskPanel),
        "the roster's cursor peels before the task panel"
    );
    assert!(
        at(EscLayer::BackgroundDialog) < at(EscLayer::Roster),
        "the modal is dismissed before the furniture it is drawn over"
    );
    assert_eq!(
        EscLayer::ORDER.len(),
        17,
        "every variant is in ORDER — one missing is a layer Esc can never reach"
    );
    assert!(
        at(EscLayer::AwayStop) < at(EscLayer::Interrupt),
        "the page's run is stopped before main's would even be considered"
    );
    assert_eq!(
        at(EscLayer::AwayHome),
        EscLayer::ORDER.len() - 1,
        "leaving the page is the last thing Esc does (v6)"
    );

    let mut chat = test_chat();
    seed_agent(&chat, "scout");
    chat.busy = true;
    chat.help_visible = true;
    assert!(chat.roster_enter_selection(), "a cursor on the roster");

    let t0 = std::time::Instant::now();
    let mut order = Vec::new();
    while let Some(layer) = chat.esc_layer() {
        order.push(layer);
        assert!(chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0));
        if layer == EscLayer::Interrupt {
            break;
        }
        if layer == EscLayer::Roster {
            assert!(chat.roster_selection().is_none(), "the cursor left");
        }
        assert!(
            !chat.interrupted,
            "a layer above the interrupt closed instead of the turn: {layer:?}"
        );
        assert!(chat.busy, "the turn kept running through {layer:?}");
    }
    assert_eq!(
        order,
        vec![EscLayer::HelpPanel, EscLayer::Roster, EscLayer::Interrupt,],
        "the stack is walked top-down, one entry per press"
    );

    // The roster's rows are constant furniture (v6): the one state Esc can
    // take is the cursor on them, and the rows stay.
    let mut chat = test_chat();
    seed_agent(&chat, "scout");
    assert!(chat.roster_enter_selection());
    assert!(chat.roster_selection().is_some());
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(
        chat.roster_selection().is_none(),
        "the press cleared the cursor"
    );
    assert!(
        chat.roster_len() > 0,
        "and the rows stay — they are furniture"
    );
}

/// D95's key, D104's cycle, D115's toggle: ctrl+t belongs to the task panel
/// and to nothing else (user ruling: "ctrl+t 只和 task 展示有关"). The roster
/// is constant furniture (v6) and neither opens nor closes with it.
#[test]
fn ctrl_t_toggles_the_task_panel_alone() {
    let mut chat = test_chat();
    seed_agent(&chat, "scout");
    let ctrl_t = |chat: &mut Chat| chat.on_key(KeyCode::Char('t'), KeyModifiers::CONTROL);

    assert!(ctrl_t(&mut chat));
    assert!(chat.tasks_visible, "on: the task panel");
    assert!(!chat.tasks_auto, "opened by hand, so it stays open");
    assert!(
        chat.dialog.is_none(),
        "and no modal: the dialog is ctrl+b's (D107)"
    );

    assert!(ctrl_t(&mut chat));
    assert!(!chat.tasks_visible, "off: back to the transcript");
    assert!(chat.roster_len() > 0, "the roster never moved");

    // Esc closes the open panel rather than toggling anything.
    assert!(ctrl_t(&mut chat));
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!chat.tasks_visible);
}

/// The dialog is modal for the keys it uses and transparent to the chords it
/// does not: `x` stops a row instead of typing a letter, while a chord still
/// reaches the application underneath.
///
/// Rewritten for D107 from the directory's version of the same claim — the
/// surface changed and the letter it swallows changed with it; the property is
/// the one the directory was built to have.
#[test]
fn the_dialog_swallows_its_own_keys_and_passes_the_chords_through() {
    let mut chat = test_chat();
    chat.open_background_dialog();

    chat.set_input("draft");
    assert!(chat.on_key(KeyCode::Char('x'), KeyModifiers::NONE));
    assert_eq!(chat.input, "draft", "a bare key never reached the composer");

    chat.cursor = 2;
    assert!(chat.on_key(KeyCode::Char('k'), KeyModifiers::CONTROL));
    assert_eq!(chat.input, "dr", "but a chord did");
    assert!(chat.dialog.is_some(), "and the dialog stayed open");

    // One chord is the exception: the key that opened it closes it, which is
    // the ctrl+t panels' rule and beats leaving ctrl+b dead while it is up.
    assert!(chat.on_key(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert!(chat.dialog.is_none());
    assert_eq!(chat.input, "dr", "and it did not edit the draft either");
}

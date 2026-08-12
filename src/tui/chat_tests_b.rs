use super::tests_a::*;
use super::*;
use base64::Engine;
use serde_json::json;
#[test]
fn interleaved_group_keeps_text_position() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    let _ = chat.events.send(UiEvent::TextDelta("let me read".into()));
    chat.drain_events();
    let _ = chat.events.send(UiEvent::ToolStart {
        name: "Read".into(),
    });
    chat.drain_events();
    let _ = chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Read".into(),
        input: json!({"file_path": "a.md"}),
        standalone: false,
    });
    chat.drain_events();
    let joined = visible(&mut chat, 120, 20);
    let text_pos = joined.find("let me read").expect("text");
    let group_pos = joined.find("Reading 1 file").expect("group line");
    assert!(text_pos < group_pos, "text before group: {joined}");
}

#[test]
fn ctrl_o_round_trip_collapses_group_back() {
    let mut chat = test_chat();
    start_group_done(&mut chat);
    finish_turn(&mut chat);
    assert!(
        visible(&mut chat, 120, 40).contains("Read 2 files"),
        "collapsed first"
    );
    assert!(chat.toggle_transcript());
    let expanded = visible(&mut chat, 120, 40);
    assert!(expanded.contains("Read a.md"), "expanded: {expanded}");
    assert!(
        !expanded.contains("Read 2 files"),
        "no collapse line: {expanded}"
    );
    assert!(chat.toggle_transcript());
    let collapsed = visible(&mut chat, 120, 40);
    assert!(
        collapsed.contains("Read 2 files"),
        "collapsed again: {collapsed}"
    );
    assert!(
        !collapsed.contains("Read a.md"),
        "tools hidden: {collapsed}"
    );
}

#[test]
fn click_group_then_ctrl_o_collapses() {
    let mut chat = test_chat();
    start_group_done(&mut chat);
    finish_turn(&mut chat);
    chat.build_rows(120);
    // Clicking the group fold row expands
    let row = chat
        .doc
        .click_ranges
        .iter()
        .find(|r| matches!(r.target, ClickTarget::Group { .. }))
        .map(|r| r.start)
        .expect("group fold row");
    assert!(chat.doc_click(row), "click expands group");
    let expanded = visible(&mut chat, 120, 40);
    assert!(expanded.contains("Read a.md"), "click expanded: {expanded}");
    // ctrl+o collapses back
    assert!(chat.toggle_transcript());
    let collapsed = visible(&mut chat, 120, 40);
    assert!(
        collapsed.contains("Read 2 files"),
        "ctrl+o collapsed: {collapsed}"
    );
}

#[test]
fn running_tool_shows_input_summary_after_ready() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    let _ = chat.events.send(UiEvent::ToolStart {
        name: "Skill".into(),
    });
    chat.drain_events();
    let _ = chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Skill".into(),
        input: json!({"skill": "pdf", "args": "doc.md"}),
        standalone: false,
    });
    chat.drain_events();
    let joined = visible(&mut chat, 120, 30);
    assert!(
        joined.contains("pdf doc.md"),
        "running header shows input summary: {joined}"
    );
    // After completion, duration uses the real value
    let _ = chat
        .events
        .send(UiEvent::ToolDone(crate::query::ToolCallDone {
            tool_call_id: "test-tool".into(),
            name: "Skill".into(),
            summary: "pdf doc.md".into(),
            output: "✦ pdf — read /tmp/skills/SKILL.md".into(),
            status: crate::query::ToolCallStatus::Done,
            diff: None,
            duration_ms: 3210,
        }));
    chat.drain_events();
    let joined = visible(&mut chat, 120, 30);
    // CC two-line form: elapsed time merges into the result row, and only slow commands (>2s) show it.
    // Skill uses the ✦ icon (category icons: ⏺ built-in / ◆ MCP / ✦ Skill).
    assert!(
        joined.contains("✦ Skill(pdf doc.md)"),
        "header row: {joined}"
    );
    assert!(
        joined.contains("✦ pdf"),
        "the result row shows only the ✦ skill name: {joined}"
    );
    assert!(
        !joined.contains("read /tmp/skills/SKILL.md"),
        "the pointer path never enters the TUI result row: {joined}"
    );
    assert!(
        joined.contains("Ran in 3.2s"),
        "the result row carries the duration: {joined}"
    );
    assert!(
        !joined.contains("3210ms"),
        "milliseconds no longer enter the header row: {joined}"
    );
}

/// Agent aligns with Task renderToolUseMessage=null: ToolStart creates no tool activity row,
/// the message area is carried solely by the Watch progress row (the only display, updated in place).
#[test]
fn agent_tool_start_creates_no_tool_activity() {
    assert!(is_hidden_tool("Agent"), "Agent is a hidden tool");
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    let _ = chat.events.send(UiEvent::ToolStart {
        name: "Agent".into(),
    });
    chat.drain_events();
    assert!(
        chat.messages[0]
            .activities
            .iter()
            .all(|a| !matches!(a.kind, ActivityKind::Tool(_))),
        "Agent creates no Tool activity: {:?}",
        chat.messages[0]
            .activities
            .iter()
            .map(|a| format!("{:?}", a.kind))
            .collect::<Vec<_>>()
    );

    // The Watch activity row is created normally (the only Agent display).
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "Agent: listing desktop dir contents".into(),
        kind: crate::watch::WatchKind::Agent,
        status: WatchState::Running,
        detail: Some("produced 0 chars".into()),
        duration_ms: 0,
        payload: None,
        signal: None,
    });
    chat.drain_events();
    let watch_rows = chat.messages[0]
        .activities
        .iter()
        .filter(|a| matches!(a.kind, ActivityKind::Watch(_)))
        .count();
    assert_eq!(watch_rows, 1, "a single Watch row");

    // Later events with the same label update in place, creating no new row.
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "Agent: listing desktop dir contents".into(),
        kind: crate::watch::WatchKind::Agent,
        status: WatchState::Running,
        detail: Some("produced 43 chars".into()),
        duration_ms: 0,
        payload: None,
        signal: None,
    });
    chat.drain_events();
    let watch_rows = chat.messages[0]
        .activities
        .iter()
        .filter(|a| matches!(a.kind, ActivityKind::Watch(_)))
        .count();
    assert_eq!(watch_rows, 1, "same-label events do not create new rows");
    let detail = chat.messages[0]
        .activities
        .iter()
        .find_map(|a| match &a.kind {
            ActivityKind::Watch(w) => w.detail.clone(),
            _ => None,
        });
    assert_eq!(
        detail.as_deref(),
        Some("produced 43 chars"),
        "the detail updates in place"
    );
}

#[tokio::test]
async fn terminal_watch_event_triggers_auto_turn_when_idle() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "Agent: long task".into(),
        kind: crate::watch::WatchKind::Agent,
        status: WatchState::Running,
        detail: None,
        duration_ms: 0,
        payload: None,
        signal: None,
    });
    chat.drain_events();
    assert!(!chat.busy);
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "Agent: long task".into(),
        kind: crate::watch::WatchKind::Agent,
        status: WatchState::Done,
        detail: Some("done".into()),
        duration_ms: 30000,
        payload: Some(serde_json::json!("result")),
        signal: None,
    });
    chat.drain_events();
    tokio::task::yield_now().await;
    chat.drain_events();
    assert!(chat.busy, "auto turn started");
    assert_eq!(chat.messages.len(), 2, "new message for auto turn");
}

#[tokio::test]
async fn signal_triggers_auto_turn_even_while_typing() {
    let mut chat = test_chat();
    chat.input = "still typing".to_string();
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "tail -f app.log".into(),
        kind: crate::watch::WatchKind::Command,
        status: WatchState::Running,
        detail: None,
        duration_ms: 0,
        payload: None,
        signal: None,
    });
    chat.drain_events();
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "tail -f app.log".into(),
        kind: crate::watch::WatchKind::Command,
        status: WatchState::Running,
        detail: Some("found 1 error".into()),
        duration_ms: 12000,
        payload: None,
        signal: Some("found error: ERROR boom".into()),
    });
    chat.drain_events();
    tokio::task::yield_now().await;
    chat.drain_events();
    assert!(chat.busy, "signal wakes despite typing");
    assert_eq!(chat.input, "still typing", "input preserved");
}

/// Test watchable: state always Running.
struct FakeWatchable;

impl crate::watch::Watchable for FakeWatchable {
    fn label(&self) -> String {
        "fake".to_string()
    }
    fn poll(&self) -> crate::watch::WatchPoll {
        crate::watch::WatchPoll {
            state: crate::watch::WatchState::Running,
            detail: None,
            payload: None,
            signal: None,
        }
    }
    fn check_interval(&self) -> Option<std::time::Duration> {
        None
    }
}

#[tokio::test]
async fn turn_end_triggers_auto_turn_when_wake_notification_pending() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    chat.busy = true;
    let watch = chat.session.watch.clone();
    let id = watch.register_with_conditions(Box::new(FakeWatchable), Vec::new(), None);
    watch.set_state(
        id,
        crate::watch::WatchState::Done,
        Some("done".into()),
        None,
    );
    assert!(watch.has_wake_notifications(None), "notification queued");
    chat.drain_events();
    assert!(chat.busy, "still busy, no auto turn mid-turn");
    let _ = chat.events.send(UiEvent::TurnEnd);
    chat.drain_events();
    tokio::task::yield_now().await;
    chat.drain_events();
    assert!(chat.busy, "auto turn started after TurnEnd");
    assert_eq!(chat.messages.len(), 2, "new message for wake turn");
}

#[tokio::test]
async fn draw_with_long_cjk_stream_and_activities_does_not_panic() {
    let mut chat = test_chat();
    chat.apply_turn_start();
    let big = "the clippy baseline is running in the background (task 2). Here is the summary and optimization list.\n\n---\n\n## Project overview (subagent summary)\n\n**bingo** is a local agent CLI implemented in Rust.\n\n- **Two run modes**: interactive TUI and headless `--print`\n- **9 built-in tools** + MCP (stdio) adapters; five permission-gate modes\n- **Core layering**: `api/`, `tool/`, `query.rs`, `tui.rs`\n- **watch mechanism**: background command / subagent state machines\n";
    for chunk in big.chars().collect::<Vec<_>>().chunks(120) {
        let t: String = chunk.iter().collect();
        let _ = chat.events.send(UiEvent::TextDelta(t));
        chat.drain_events();
    }
    let _ = chat.events.send(UiEvent::ToolStart {
        name: "Bash".into(),
    });
    chat.drain_events();
    let _ = chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Bash".into(),
        input: json!({"command": "cargo clippy"}),
        standalone: false,
    });
    chat.drain_events();
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "Agent: review".into(),
        kind: crate::watch::WatchKind::Agent,
        status: WatchState::Running,
        detail: Some("produced 100 chars".into()),
        duration_ms: 5000,
        payload: None,
        signal: None,
    });
    chat.drain_events();
    let _ = chat.events.send(UiEvent::TextDelta(
        "more body text, with more CJK, continuing.".into(),
    ));
    chat.drain_events();
    let _ = chat
        .events
        .send(UiEvent::ToolDone(crate::query::ToolCallDone {
            tool_call_id: "test-tool".into(),
            name: "Bash".into(),
            summary: "$ cargo clippy".into(),
            output: "ok".into(),
            status: crate::query::ToolCallStatus::Done,
            diff: None,
            duration_ms: 3000,
        }));
    chat.drain_events();
    let _ = chat.events.send(UiEvent::TurnEnd);
    chat.drain_events();
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "Agent: review".into(),
        kind: crate::watch::WatchKind::Agent,
        status: WatchState::Done,
        detail: Some("done".into()),
        duration_ms: 30000,
        payload: None,
        signal: None,
    });
    chat.drain_events();
    visible(&mut chat, 120, 40);
    assert_eq!(chat.messages.len(), 1, "single message rendered");
}

#[test]
fn watch_event_updates_across_messages_in_place() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "Agent: explore".into(),
        kind: crate::watch::WatchKind::Agent,
        status: WatchState::Running,
        detail: None,
        duration_ms: 0,
        payload: None,
        signal: None,
    });
    chat.drain_events();
    assert_eq!(chat.messages[0].activities.len(), 1);
    let _ = chat.events.send(UiEvent::TurnEnd);
    chat.drain_events();
    chat.stream_msg = None;
    chat.messages.push(msg(Role::Assistant, ""));
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "Agent: explore".into(),
        kind: crate::watch::WatchKind::Agent,
        status: WatchState::Done,
        detail: Some("done".into()),
        duration_ms: 40000,
        payload: None,
        signal: None,
    });
    chat.drain_events();
    assert_eq!(chat.messages[0].activities.len(), 1, "updated in place");
    assert_eq!(chat.messages[1].activities.len(), 0, "no new row at bottom");
    let w = match &chat.messages[0].activities[0].kind {
        ActivityKind::Watch(w) => w,
        _ => unreachable!(),
    };
    assert_eq!(w.status, WatchState::Done, "in-place status change");
}

#[test]
fn idle_round_notification_does_not_trigger_auto_turn() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "watch ls".into(),
        kind: crate::watch::WatchKind::Command,
        status: WatchState::Idle,
        detail: Some("round 1".into()),
        duration_ms: 5000,
        payload: None,
        signal: None,
    });
    chat.drain_events();
    assert!(!chat.busy, "idle round does not wake");
    assert_eq!(chat.messages.len(), 1);
}

#[test]
fn watch_event_renders_inline_and_updates() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "watch -n 2 ls".into(),
        kind: crate::watch::WatchKind::Command,
        status: WatchState::Running,
        detail: None,
        duration_ms: 0,
        payload: None,
        signal: None,
    });
    chat.drain_events();
    assert_eq!(chat.messages[0].activities.len(), 1);
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "watch -n 2 ls".into(),
        kind: crate::watch::WatchKind::Command,
        status: WatchState::Idle,
        detail: Some("round 2".into()),
        duration_ms: 4000,
        payload: None,
        signal: None,
    });
    let _ = chat.events.send(UiEvent::WatchEvent {
        label: "watch -n 2 ls".into(),
        kind: crate::watch::WatchKind::Command,
        status: WatchState::Done,
        detail: None,
        duration_ms: 9000,
        payload: Some(serde_json::json!("done output")),
        signal: None,
    });
    chat.drain_events();
    assert_eq!(chat.messages[0].activities.len(), 1, "updates in place");
    let joined = visible(&mut chat, 120, 30);
    assert!(joined.contains("⏺ watch -n 2 ls"), "header: {joined}");
    assert!(joined.contains("  ⎿  round 2"), "result row: {joined}");
    assert!(chat.toggle_transcript());
    let joined = visible(&mut chat, 120, 30);
    assert!(joined.contains("done output"), "expanded: {joined}");
}

#[test]
fn bash_folds_into_group_with_count() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    for (name, input) in [
        ("Bash", json!({"command": "cargo test"})),
        ("Read", json!({"file_path": "a.md"})),
        ("Bash", json!({"command": "npm run build"})),
    ] {
        let _ = chat.events.send(UiEvent::ToolStart { name: name.into() });
        chat.drain_events();
        let _ = chat.events.send(UiEvent::ToolReady {
            tool_call_id: "test-tool".into(),
            name: name.into(),
            input,
            standalone: false,
        });
        chat.drain_events();
    }
    assert_eq!(chat.messages[0].groups.len(), 1, "all fold into one group");
    let g = &chat.messages[0].groups[0];
    assert_eq!(g.bash, 2);
    assert_eq!(g.read_ops, 0);
    assert_eq!(g.read_paths, vec!["a.md".to_string()]);
    assert_eq!(
        collapse_summary(g, false),
        "Read 1 file, ran 2 bash commands"
    );
    assert_eq!(
        collapse_summary(g, true),
        "Reading 1 file, running 2 bash commands…"
    );
    for (summary, out) in [
        ("Bash $ cargo test", "ok"),
        ("Read a.md", "l1"),
        ("Bash $ npm run build", "done"),
    ] {
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
                tool_call_id: "test-tool".into(),
                name: summary.split(' ').next().unwrap().into(),
                summary: summary.into(),
                output: out.into(),
                status: crate::query::ToolCallStatus::Done,
                diff: None,
                duration_ms: 1,
            }));
        chat.drain_events();
    }
    let joined = visible(&mut chat, 120, 30);
    assert!(
        joined.contains("Read 1 file, ran 2 bash commands"),
        "final summary: {joined}"
    );
}

#[test]
fn running_group_shows_hint_line_then_hides_when_done() {
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
        input: json!({"file_path": "package.json"}),
        standalone: false,
    });
    chat.drain_events();
    let joined = visible(&mut chat, 120, 30);
    assert!(
        joined.contains("⎿") && joined.contains("package.json"),
        "running group shows hint: {joined}"
    );
    let _ = chat
        .events
        .send(UiEvent::ToolDone(crate::query::ToolCallDone {
            tool_call_id: "test-tool".into(),
            name: "Read".into(),
            summary: "Read package.json".into(),
            output: "l1".into(),
            status: crate::query::ToolCallStatus::Done,
            diff: None,
            duration_ms: 3,
        }));
    chat.drain_events();
    let joined = visible(&mut chat, 120, 30);
    assert!(joined.contains("Read 1 file"), "past tense: {joined}");
    assert!(
        !joined.contains("⎿"),
        "hint hidden when group done: {joined}"
    );
}

/// Collapse groups are bounded by text: neither RoundEnd (model rounds) nor thinking splits them,
/// tools across rounds merge into one group; only text (TextDelta) opens a new one.
#[test]
fn group_survives_rounds_and_thinking_until_text() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    let _ = chat.events.send(UiEvent::ToolStart {
        name: "Grep".into(),
    });
    chat.drain_events();
    let _ = chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Grep".into(),
        input: json!({"pattern": "nomatch"}),
        standalone: false,
    });
    chat.drain_events();
    assert_eq!(chat.messages[0].groups.len(), 1, "round 1 group");
    let _ = chat.events.send(UiEvent::RoundEnd);
    chat.drain_events();
    let _ = chat.events.send(UiEvent::ThinkingDelta("hmm".into()));
    chat.drain_events();
    let _ = chat.events.send(UiEvent::ToolStart {
        name: "Grep".into(),
    });
    chat.drain_events();
    let _ = chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Grep".into(),
        input: json!({"pattern": "another"}),
        standalone: false,
    });
    chat.drain_events();
    assert_eq!(chat.messages[0].groups.len(), 1, "round 2 joins same group");
    let idx = chat.messages[0].activities.len() - 1;
    assert_eq!(chat.messages[0].group_of[idx], Some(0));
    let _ = chat.events.send(UiEvent::ToolStart {
        name: "Read".into(),
    });
    chat.drain_events();
    let _ = chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Read".into(),
        input: json!({"file_path": "a.md"}),
        standalone: false,
    });
    chat.drain_events();
    assert_eq!(
        chat.messages[0].groups.len(),
        1,
        "same-group Read joins group"
    );
    // Text appears: the group closes and later tools open a new one.
    let _ = chat.events.send(UiEvent::TextDelta("conclusion…".into()));
    chat.drain_events();
    let _ = chat.events.send(UiEvent::ToolStart {
        name: "Grep".into(),
    });
    chat.drain_events();
    let _ = chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Grep".into(),
        input: json!({"pattern": "post-text"}),
        standalone: false,
    });
    chat.drain_events();
    assert_eq!(chat.messages[0].groups.len(), 2, "text opens new group");
    let idx = chat.messages[0].activities.len() - 1;
    assert_eq!(chat.messages[0].group_of[idx], Some(1));
}

#[test]
fn expand_running_then_complete_then_collapse_back() {
    let mut chat = test_chat();
    start_group(&mut chat);
    assert!(
        visible(&mut chat, 120, 40).contains("Reading 2 files"),
        "running fold"
    );
    assert!(chat.toggle_transcript());
    assert!(
        !visible(&mut chat, 120, 40).contains("Reading 2 files"),
        "expanded"
    );
    for (summary, out) in [("Read a.md", "l1\nl2\nl3"), ("Read b.md", "x\ny")] {
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
                tool_call_id: "test-tool".into(),
                name: "Read".into(),
                summary: summary.into(),
                output: out.into(),
                status: crate::query::ToolCallStatus::Done,
                duration_ms: 0,
                diff: None,
            }));
    }
    chat.drain_events();
    finish_turn(&mut chat);
    assert!(chat.toggle_transcript());
    let collapsed = visible(&mut chat, 120, 40);
    assert!(
        collapsed.contains("Read 2 files"),
        "collapsed after turn: {collapsed}"
    );
}

#[test]
fn click_expanded_group_head_collapses_back() {
    let mut chat = test_chat();
    start_group(&mut chat);
    chat.build_rows(120);
    let fold_row = chat
        .doc
        .click_ranges
        .iter()
        .find(|r| matches!(r.target, ClickTarget::Group { .. }))
        .map(|r| r.start)
        .expect("group fold row");
    assert!(chat.doc_click(fold_row), "click expands");
    chat.build_rows(120);
    let head_row = chat
        .doc
        .click_ranges
        .iter()
        .find(|r| matches!(r.target, ClickTarget::Group { .. }))
        .map(|r| r.start)
        .expect("group head row");
    assert!(head_row >= fold_row, "head row after fold row");
    assert!(chat.doc_click(head_row), "click head collapses");
    let collapsed = visible(&mut chat, 120, 40);
    assert!(
        collapsed.contains("Reading 2 files"),
        "collapsed again: {collapsed}"
    );
}

#[test]
fn collapse_after_expand_then_expand_again() {
    let mut chat = test_chat();
    start_group(&mut chat);
    chat.stream_msg = Some(0);
    for (summary, out) in [("Read a.md", "l1"), ("Read b.md", "x")] {
        let _ = chat
            .events
            .send(UiEvent::ToolDone(crate::query::ToolCallDone {
                tool_call_id: "test-tool".into(),
                name: "Read".into(),
                summary: summary.into(),
                output: out.into(),
                status: crate::query::ToolCallStatus::Done,
                duration_ms: 0,
                diff: None,
            }));
    }
    chat.drain_events();
    chat.stream_msg = None;
    for _ in 0..3 {
        assert!(chat.toggle_transcript());
        assert!(
            !visible(&mut chat, 120, 40).contains("Read 2 files"),
            "expanded state"
        );
        assert!(chat.toggle_transcript());
        assert!(
            visible(&mut chat, 120, 40).contains("Read 2 files"),
            "collapsed state"
        );
    }
}

#[test]
fn user_message_has_bubble_background() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::User, "hello"));
    chat.build_rows(100);
    let row = chat
        .doc
        .rows
        .iter()
        .find(|r| r.line.plain_text().starts_with("❯"));
    assert!(row.is_some(), "user row rendered");
    assert_eq!(row.unwrap().bg, Some(chat.theme.user_message_bg));
}

/// User messages with newlines (multi-line pastes) must split into single-line Rows: a Row always
/// occupies one line; mixing in newlines would detach the row model from the actual viewport height.
#[test]
fn multiline_user_message_wraps_into_single_line_rows() {
    let mut chat = test_chat();
    chat.messages
        .push(msg(Role::User, "first line\nsecond line\nthird"));
    chat.build_rows(40);
    let bubbles: Vec<&Row> = chat.doc.rows.iter().filter(|r| r.bg.is_some()).collect();
    assert_eq!(bubbles.len(), 3, "one bubble Row per line");
    for row in &bubbles {
        for seg in &row.line.segs {
            assert!(
                !seg.text.contains(['\n', '\r']),
                "a Row must be a single line: {:?}",
                seg.text
            );
        }
    }
    assert!(bubbles[0].line.plain_text().starts_with("❯ first line"));
    // Continuation lines align with indentation, never repeating the prefix.
    assert!(bubbles[1].line.plain_text().starts_with("  second line"));
}

/// Overlong (newline-free) user messages wrap to the terminal width instead of spilling off screen.
#[test]
fn long_user_message_wraps_to_width() {
    let mut chat = test_chat();
    let text = "word ".repeat(40);
    chat.messages.push(msg(Role::User, text.trim()));
    chat.build_rows(30);
    let bubbles: Vec<&Row> = chat.doc.rows.iter().filter(|r| r.bg.is_some()).collect();
    assert!(bubbles.len() > 1, "a long message wraps into multiple rows");
    for row in bubbles {
        // 2 prefix columns + body ≤ width-1 (1 column of right padding inside the bubble).
        assert!(
            text_width(&row.line.plain_text()) <= 29,
            "row width exceeded: {:?}",
            row.line.plain_text()
        );
    }
}

/// A collapse group's `⎿ hint` row may hold a multi-line bash command: it must be single-lined + truncated.
#[test]
fn multiline_hint_stays_one_row() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, ""));
    chat.stream_msg = Some(0);
    let _ = chat.events.send(UiEvent::ToolStart {
        name: "Bash".into(),
    });
    chat.drain_events();
    let _ = chat.events.send(UiEvent::ToolReady {
        tool_call_id: "test-tool".into(),
        name: "Bash".into(),
        input: json!({"command": "grep -rn foo \\\n  --include='*.rs' .\nls -la"}),
        standalone: false,
    });
    chat.drain_events();
    chat.build_rows(60);
    let hint = chat
        .doc
        .rows
        .iter()
        .find(|r| r.line.plain_text().contains('⎿'))
        .expect("hint row rendered");
    assert!(
        !hint.line.plain_text().contains('\n'),
        "the hint is single-lined"
    );
    assert!(
        text_width(&hint.line.plain_text()) <= 60,
        "the hint truncates by width"
    );
}

/// The flush cursor counts by message boundary: re-layout after a width change (all row numbers change) never re-flushes.
#[test]
fn flush_cursor_survives_width_change() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::User, "first message"));
    chat.messages.push(msg(Role::Assistant, "reply body"));
    chat.build_rows(100);
    assert_eq!(
        chat.doc.settled,
        chat.doc.rows.len(),
        "everything settles when idle"
    );
    assert_eq!(
        settled_segments(&chat),
        3,
        "welcome card + 2 messages = 3 segments"
    );
    chat.advance_flushed();
    assert_eq!(chat.flushed_segments, 3);
    assert_eq!(chat.tail_start, chat.doc.rows.len());

    // Rebuild after a width change: already-flushed segments no longer appear in the doc.
    chat.build_rows(40);
    assert_eq!(
        chat.tail_start, 0,
        "the tail restarts from zero after a rebuild"
    );
    assert!(chat.doc.rows.is_empty(), "flushed content is not rebuilt");
    let text: String = chat.doc.rows.iter().map(|r| r.line.plain_text()).collect();
    assert!(!text.contains("first message"), "not printed again");

    // A new message only builds its own segment.
    chat.messages.push(msg(Role::User, "second message"));
    chat.build_rows(40);
    assert!(
        chat.doc
            .rows
            .iter()
            .any(|r| r.line.plain_text().contains("second message")),
        "the new message enters the document"
    );
    assert_eq!(settled_segments(&chat), 1, "only 1 new segment");
}

/// Streaming (unsettled) content is not flushed: a full markdown re-parse rewrites earlier rows,
/// which would be frozen in scrollback as an unchangeable intermediate state.
#[test]
fn streaming_content_is_not_flushed_until_settled() {
    let mut chat = test_chat();
    chat.build_rows(80);
    chat.advance_flushed();
    let welcome_segments = chat.flushed_segments;
    assert_eq!(welcome_segments, 1, "the welcome card is segment 0");

    chat.handle(UiEvent::TurnStart);
    chat.handle(UiEvent::TextDelta("| a | b |".into()));
    chat.build_rows(80);
    assert_eq!(chat.doc.settled, 0, "streaming content is not settled");
    assert!(
        !chat.doc.rows.is_empty(),
        "but still renders in the dynamic tail"
    );
    chat.advance_flushed();
    assert_eq!(
        chat.flushed_segments, welcome_segments,
        "the cursor does not move"
    );

    chat.handle(UiEvent::TurnEnd);
    chat.build_rows(80);
    assert_eq!(
        chat.doc.settled,
        chat.doc.rows.len(),
        "everything settles after the turn ends"
    );
    chat.advance_flushed();
    assert_eq!(
        chat.flushed_segments,
        welcome_segments + 1,
        "the message is flushed"
    );
}

/// `/clear` (and `/resume`) replace the message set wholesale → segment numbers become invalid, so the flush
/// cursor must reset, otherwise the new session's doc is skipped wholesale (blank screen).
#[test]
fn clear_resets_flush_cursor() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::User, "hi"));
    chat.build_rows(80);
    chat.advance_flushed();
    assert!(chat.flushed_segments > 0);
    chat.input = "/clear".to_string();
    chat.submit();
    assert_eq!(chat.flushed_segments, 0, "the cursor resets");
    assert!(chat.dirty, "a rebuild after the reset");
    chat.build_rows(80);
    assert!(
        chat.doc
            .rows
            .iter()
            .any(|r| r.line.plain_text().contains("bingo")),
        "the welcome card reappears"
    );
}

/// An AskUserQuestion answer is an ordinary user message: it enters the message flow, settles like a normal message,
/// and flushes (the segment count advances) — no longer a transient block rendered above the input box.
#[test]
fn ask_answer_message_flushes_like_normal_message() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::User, "hi"));
    chat.build_rows(80);
    chat.advance_flushed();
    assert_eq!(chat.flushed_segments, 2, "welcome card + the user's input");

    // Answer one question (through the real event path).
    let (tx, _rx) = oneshot::channel();
    let mut request =
        PermissionRequest::new("Tech stack", "Which library?", vec!["A".into(), "B".into()]);
    request.free_text = true;
    chat.pending_ask = Some((request, tx));
    chat.ask_focus = 0;
    assert!(
        chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
        "Enter selects A"
    );
    assert!(chat.pending_ask.is_none(), "the dialog closed");

    // The answer enters the message flow as a user message.
    let answer = chat
        .messages
        .last()
        .expect("the answer message entered the flow");
    assert_eq!(answer.role, Role::User, "the answer is a user message");
    assert!(
        answer.text.contains("User answered the questions:"),
        "{}",
        answer.text
    );
    assert!(
        answer.text.contains("· Which library? → A"),
        "{}",
        answer.text
    );
    // Settles and flushes like a normal message: the cursor advances by message segment.
    chat.build_rows(80);
    assert_eq!(
        chat.doc.settled,
        chat.doc.rows.len(),
        "the answer message is settled"
    );
    chat.advance_flushed();
    assert_eq!(
        chat.flushed_segments, 3,
        "the welcome card + hi + the answer message all flush"
    );
}

/// Answer messages persist with the session: TurnEnd no longer clears them (they used to be in-turn transient blocks,
/// vanishing at the turn end; now they are part of the message flow).
#[test]
fn ask_answer_message_persists_across_turn_end() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::User, "hi"));
    let (tx, _rx) = oneshot::channel();
    let mut request =
        PermissionRequest::new("Tech stack", "Which library?", vec!["A".into(), "B".into()]);
    request.free_text = true;
    chat.pending_ask = Some((request, tx));
    chat.ask_focus = 1;
    assert!(
        chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
        "Enter selects B"
    );

    chat.handle(UiEvent::TurnEnd);
    let answer = chat
        .messages
        .last()
        .expect("the answer message is still there");
    assert_eq!(
        answer.role,
        Role::User,
        "the turn end does not clear the answer message"
    );
    assert!(
        answer.text.contains("· Which library? → B"),
        "{}",
        answer.text
    );
    chat.build_rows(80);
    let joined: String = chat
        .doc
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("User answered the questions:"),
        "the answer still renders in the message flow: {joined}"
    );
}

/// Answering mid-turn ends the assistant message and opens a fresh one: everything the model
/// does next belongs *below* what the user just said. Before this, `stream_msg` kept pointing
/// at the message above the answer, so the continuation rendered on top of it and the answer
/// sat pinned at the bottom of the transcript until the turn ended (#28).
#[test]
fn answer_mid_turn_opens_a_new_message_for_the_continuation() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::User, "hi"));
    chat.handle(UiEvent::TurnStart);
    chat.handle(UiEvent::TextDelta("before".into()));
    answer_pending_ask(&mut chat);
    assert_eq!(
        chat.messages.len(),
        4,
        "hi + what the model said before asking + the answer + the continuation"
    );
    assert_eq!(
        chat.stream_msg,
        Some(3),
        "the stream moved below the answer"
    );

    chat.handle(UiEvent::TextDelta("after".into()));
    assert_eq!(chat.messages[1].text, "before");
    assert_eq!(chat.messages[2].role, Role::User, "the answer");
    assert_eq!(
        chat.messages[3].text, "after",
        "the continuation lands under the answer, not above it"
    );
}

/// The message the answer closed has to be able to settle. AskUserQuestion is a hidden tool,
/// so `ToolStart` never closed the placeholder thinking block TurnStart opened — left running
/// it would hold the settle prefix (and every flush after it) for the rest of the session.
#[test]
fn the_message_an_answer_closed_settles_without_waiting_for_the_turn() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::User, "hi"));
    chat.handle(UiEvent::TurnStart);
    chat.handle(UiEvent::ThinkingDelta("weighing it up".into()));
    answer_pending_ask(&mut chat);

    chat.build_rows(80);
    assert!(chat.message_settled(0), "the leading user message");
    assert!(
        chat.message_settled(1),
        "the pre-answer message is finished: nothing more can be added to it"
    );
    assert!(chat.message_settled(2), "the answer");
    assert!(
        !chat.message_settled(3),
        "the continuation is still streaming"
    );

    chat.handle(UiEvent::TurnEnd);
    chat.build_rows(80);
    assert_eq!(
        chat.doc.settled,
        chat.doc.rows.len(),
        "everything settles after the turn ends"
    );
}

/// The turn ends right after the answer: the continuation message never received anything,
/// and an empty assistant block would render as a stray gap.
#[test]
fn an_unused_continuation_message_is_dropped_at_turn_end() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::User, "hi"));
    chat.handle(UiEvent::TurnStart);
    chat.handle(UiEvent::TextDelta("before".into()));
    answer_pending_ask(&mut chat);
    assert_eq!(chat.messages.len(), 4);

    chat.handle(UiEvent::TurnEnd);
    assert_eq!(
        chat.messages.len(),
        3,
        "the empty continuation is dropped: hi + the model's text + the answer"
    );
    assert_eq!(chat.messages[2].role, Role::User, "the answer is last");
    chat.build_rows(80);
    chat.advance_flushed();
    assert_eq!(
        chat.flushed_segments, 4,
        "welcome card + hi + the model's text + the answer all flush"
    );
}

/// A tool call still in flight owns activity indices in the current message
/// (`pending_tools`), so its rows must keep landing there: the split waits.
#[test]
fn a_tool_in_flight_pins_the_stream_to_its_own_message() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::User, "hi"));
    chat.handle(UiEvent::TurnStart);
    chat.handle(UiEvent::ToolStart {
        name: "Read".into(),
    });
    answer_pending_ask(&mut chat);
    assert_eq!(chat.stream_msg, Some(1), "the stream stays with the tool");
    assert_eq!(chat.messages.len(), 3, "hi + assistant + answer");
}

/// Answers a pending free-text question by confirming its first option.
fn answer_pending_ask(chat: &mut Chat) {
    let (tx, _rx) = oneshot::channel();
    let mut request = PermissionRequest::new("Tech stack", "Which library?", vec!["A".into()]);
    request.free_text = true;
    chat.pending_ask = Some((request, tx));
    chat.ask_focus = 0;
    assert!(
        chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
        "select A"
    );
}

/// The error path never reaches TurnEnd (start_turn's `Err(e)` only emits UiEvent::Error):
/// the answer message stays in the message flow — the old transient block was never cleaned up on the error path and hung until
/// /clear (the regression path of a pre-24ba4d9 bug); an ordinary message has no state to clear, so it is fixed by design.
#[test]
fn ask_answer_message_survives_error_path() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::User, "hi"));
    let (tx, _rx) = oneshot::channel();
    let mut request =
        PermissionRequest::new("Tech stack", "Which library?", vec!["A".into(), "B".into()]);
    request.free_text = true;
    chat.pending_ask = Some((request, tx));
    chat.ask_focus = 0;
    assert!(
        chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
        "select A"
    );

    chat.handle(UiEvent::Error {
        code: "SERVER_ERROR",
        msg: "turn failed".to_string(),
        level: crate::error::ErrorLevel::Full,
        context: crate::error::ErrorContext::LongTurn,
    });
    // The answer message stays in the flow and renders as usual.
    let answer = chat
        .messages
        .last()
        .expect("the answer message is still there");
    assert_eq!(answer.role, Role::User);
    assert!(
        answer.text.contains("· Which library? → A"),
        "{}",
        answer.text
    );
    chat.build_rows(80);
    let joined: String = chat
        .doc
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("User answered the questions:"),
        "the answer still renders after the error: {joined}"
    );
}

/// The ordering guard must be linear: build_rows' settling decision under full settling (hundreds of messages)
/// must not blow up exponentially (regression: a per-prefix recursive evaluation froze at ~40 messages).
#[test]
fn message_settled_guard_is_linear_for_large_settled_sessions() {
    let mut chat = test_chat();
    for _ in 0..400 {
        chat.messages.push(msg(Role::User, "hi"));
        chat.messages.push(msg(Role::Assistant, "ok"));
    }
    // Fully static settling: build_rows decides settling for every message.
    chat.build_rows(80);
    assert_eq!(chat.doc.settled, chat.doc.rows.len(), "everything settles");
    for i in 0..chat.messages.len() {
        assert!(chat.message_settled(i), "message {i} settles");
    }
}

/// Simulates the inline component's flush loop: rebuild → flush the settled prefix → advance the cursor.
fn flush_frame(chat: &mut Chat, width: usize, printed: &mut Vec<String>) {
    chat.build_rows(width);
    if chat.doc.settled > chat.tail_start {
        for row in &chat.doc.rows[chat.tail_start..chat.doc.settled] {
            printed.push(row.line.plain_text());
        }
        chat.advance_flushed();
    }
}

/// Full-flow regression: streaming + mid-turn resize + settling — no row in the scrollback
/// is ever repeated (the old row-number cursor re-printed everything after a resize re-layout).
#[test]
fn streaming_with_resize_never_prints_a_row_twice() {
    let mut chat = test_chat();
    let mut printed = Vec::new();
    flush_frame(&mut chat, 100, &mut printed);
    let welcome = printed.len();
    assert!(welcome > 0, "the welcome card flushes");

    chat.messages
        .push(msg(Role::User, "please explain this code"));
    flush_frame(&mut chat, 100, &mut printed);
    chat.handle(UiEvent::TurnStart);
    for chunk in [
        "First paragraph.\n\n",
        "## Heading\n\n",
        "- item one\n",
        "- item two\n",
    ] {
        chat.handle(UiEvent::TextDelta(chunk.into()));
        flush_frame(&mut chat, 100, &mut printed);
    }
    // Mid-turn resize: all row numbers change after re-layout.
    flush_frame(&mut chat, 60, &mut printed);
    chat.handle(UiEvent::TextDelta("ending.".into()));
    chat.handle(UiEvent::TurnEnd);
    flush_frame(&mut chat, 60, &mut printed);
    // Idling a few frames must print nothing more.
    let after = printed.len();
    for _ in 0..3 {
        flush_frame(&mut chat, 60, &mut printed);
    }
    assert_eq!(printed.len(), after, "no new flushes");

    // The welcome card itself has repeated padded rows; deduping by content would false-positive — check only the message part.
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for line in &printed[welcome..] {
        if line.trim().is_empty() {
            continue;
        }
        *seen.entry(line.as_str()).or_default() += 1;
    }
    for (line, count) in &seen {
        assert_eq!(*count, 1, "row flushed {count} times: {line:?}");
    }
    let joined = printed.join("\n");
    assert!(
        joined.contains("please explain this code"),
        "the user message is flushed"
    );
    assert!(joined.contains("ending."), "settled body flushes");
    assert!(
        chat.doc.rows.is_empty(),
        "the tail is empty after everything flushes"
    );
}

/// inline ctrl+o replay: a no-op with nothing new; with flushed content or expandable items,
/// it expands everything, rewinds the cursor, and requests a full freeze.
#[test]
fn expand_transcript_rewinds_and_expands_everything() {
    let mut chat = test_chat();
    // Empty session, everything on screen → no-op (the replay adds no information).
    assert!(!chat.expand_transcript());
    assert!(!chat.dump_transcript);
    assert!(!chat.force_redraw);

    // After a message flushed → replay: the cursor rewinds and the rebuilt doc contains all segments;
    // clear the screen first, then write (top-aligned, same as resize).
    chat.messages.push(msg(Role::Assistant, "reply"));
    chat.build_rows(80);
    chat.advance_flushed();
    chat.build_rows(80);
    assert!(
        chat.doc.rows.is_empty(),
        "the tail is empty after everything flushes"
    );
    assert!(chat.expand_transcript());
    assert!(chat.dump_transcript);
    assert!(
        chat.force_redraw,
        "the replay frame first clears the visible screen"
    );
    chat.build_rows(80);
    let text: String = chat
        .doc
        .rows
        .iter()
        .map(|row| row.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("reply"),
        "the replay document includes flushed messages: {text}"
    );

    // Historical messages with collapse groups → everything expands before the replay.
    chat.dump_transcript = false;
    start_group(&mut chat);
    let _ = chat
        .events
        .send(UiEvent::ToolDone(crate::query::ToolCallDone {
            tool_call_id: "test-tool".into(),
            name: "Read".into(),
            summary: "Read a.md".into(),
            output: "l1\nl2\nl3".into(),
            status: crate::query::ToolCallStatus::Done,
            duration_ms: 0,
            diff: None,
        }));
    chat.drain_events();
    assert!(chat.expand_transcript());
    assert!(chat.dump_transcript);
    assert!(
        chat.messages
            .iter()
            .flat_map(|m| &m.groups)
            .all(|g| g.expanded || g.activities.is_empty()),
        "all fold groups expanded"
    );

    // Fully expanded → the second press goes the collapse direction: back to aggregates (the app layer
    // handles the clear-redraw + rehydration to close it up).
    assert!(chat.transcript_fully_expanded());
    assert!(chat.collapse_transcript());
    assert!(
        chat.messages
            .iter()
            .flat_map(|m| &m.groups)
            .all(|g| !g.expanded),
        "all fold groups closed"
    );
    assert!(
        !chat.transcript_fully_expanded(),
        "after closing, it returns to the expand direction"
    );
    assert!(
        !chat.collapse_transcript(),
        "already fully closed; closing again changes nothing"
    );
}

/// The tick does not set dirty when idle (no doc rebuild); it does when dynamic elements exist.
#[test]
fn tick_marks_dirty_only_when_dynamic() {
    let mut chat = test_chat();
    chat.dirty = false;
    chat.tick();
    assert!(!chat.dirty, "no rebuild when idle");
    assert!(!chat.needs_tick(), "idle does not wake components");
    chat.busy = true;
    chat.tick();
    assert!(chat.dirty, "rebuilds while busy (spinner/duration row)");
    assert!(chat.needs_tick());
    // Pending events must also wake it up (otherwise they would never drain).
    chat.busy = false;
    chat.dirty = false;
    let _ = chat.events.send(UiEvent::Warning("w".into()));
    assert!(chat.needs_tick(), "pending events need a wake-up");

    let mut slash_error = test_chat();
    slash_error.push_slash_error("unknown command; type /help.".to_string());
    assert!(
        slash_error.needs_tick(),
        "even when idle, a slash error must drive the tick so the error TTL auto-expires it"
    );
    slash_error.slash_error_at = Some(
        std::time::Instant::now() - SLASH_OUTPUT_ERROR_TTL - std::time::Duration::from_millis(1),
    );
    slash_error.tick();
    assert!(
        slash_error.slash_error_lines.is_empty(),
        "cleared after the error TTL expires"
    );
    assert!(
        !slash_error.needs_tick(),
        "after clearing, the host returns to true idle"
    );
}

#[test]
fn settled_tracks_streaming_message() {
    let mut chat = test_chat();
    chat.build_rows(100);
    let welcome = chat.doc.settled;
    assert!(welcome > 0, "welcome card rows are settled");
    assert_eq!(
        chat.doc.settled,
        chat.doc.rows.len(),
        "empty session fully settled"
    );
    // Turn start: streaming message + placeholder thinking → must not settle.
    chat.handle(UiEvent::TurnStart);
    chat.build_rows(100);
    assert_eq!(chat.doc.settled, welcome, "streaming message not settled");
    assert!(chat.doc.rows.len() > welcome, "streaming message rendered");
    // Turn end: the message settles, all rows enter settled.
    chat.handle(UiEvent::TurnEnd);
    chat.build_rows(100);
    assert_eq!(
        chat.doc.settled,
        chat.doc.rows.len(),
        "all rows settled after turn"
    );
    // Settling is one-way: a second message (streaming) does not move existing boundaries.
    let after_turn = chat.doc.settled;
    chat.handle(UiEvent::TurnStart);
    chat.build_rows(100);
    assert_eq!(
        chat.doc.settled, after_turn,
        "new turn keeps prior settled boundary"
    );
}

#[test]
fn settled_stops_at_running_activity() {
    let mut chat = test_chat();
    chat.build_rows(100);
    let welcome = chat.doc.settled;
    // A message with a running tool.
    let mut m = msg(Role::Assistant, "");
    m.activities.push(tool_activity());
    chat.messages.push(m);
    chat.build_rows(100);
    assert_eq!(
        chat.doc.settled, welcome,
        "running tool keeps message dynamic"
    );
    // Tool done → settles.
    let a = &mut chat.messages[0].activities[0];
    match &mut a.kind {
        ActivityKind::Tool(t) => t.status = ToolStatus::Done,
        _ => panic!("tool activity expected"),
    }
    chat.build_rows(100);
    assert_eq!(
        chat.doc.settled,
        chat.doc.rows.len(),
        "settled after tool done"
    );
}

#[test]
fn settled_stops_before_permission_block() {
    let mut chat = test_chat();
    chat.build_rows(100);
    let welcome = chat.doc.settled;
    // Streaming turn (dynamic message).
    chat.handle(UiEvent::TurnStart);
    chat.build_rows(100);
    assert_eq!(chat.doc.settled, welcome, "streaming message dynamic");
    // A permission block appears → the boundary stays put (ask blocks never settle).
    let (tx, _rx) = tokio::sync::oneshot::channel();
    chat.pending_ask = Some((
        PermissionRequest::new("Allow running Bash", "cargo build", vec!["Allow".into()]),
        tx,
    ));
    chat.build_rows(100);
    assert_eq!(chat.doc.settled, welcome, "ask block not settled");
    // Turn end + request resolved → everything settles.
    chat.pending_ask = None;
    chat.handle(UiEvent::TurnEnd);
    chat.build_rows(100);
    assert_eq!(
        chat.doc.settled,
        chat.doc.rows.len(),
        "all settled after ask done"
    );
}

#[test]
fn permission_request_renders_with_clickable_options() {
    let mut chat = test_chat();
    let (tx, _rx) = oneshot::channel();
    chat.pending_ask = Some((
        PermissionRequest::new(
            "Allow running Bash",
            "cargo build",
            vec!["Allow".into(), "Deny".into()],
        ),
        tx,
    ));
    chat.build_rows(100);
    let joined: String = chat
        .doc
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("Allow running Bash"), "title: {joined}");
    assert!(
        joined.contains("❯ 1. Allow"),
        "focused first option: {joined}"
    );
    assert!(joined.contains("2. Deny"), "option row: {joined}");
    assert!(
        joined.contains("enter to select · ↑/↓ to navigate · esc to cancel"),
        "hint: {joined}"
    );
    let ask_rows: Vec<(usize, usize)> = chat
        .doc
        .click_ranges
        .iter()
        .filter_map(|r| match r.target {
            ClickTarget::AskOption(i) => Some((r.start, i)),
            _ => None,
        })
        .collect();
    assert_eq!(ask_rows.len(), 2, "two clickable options");
}

#[test]
fn ask_question_renders_other_and_answers_free_text() {
    let mut chat = test_chat();
    let (tx, mut rx) = oneshot::channel();
    let mut request =
        PermissionRequest::new("Tech stack", "Which library?", vec!["A".into(), "B".into()]);
    request.free_text = true;
    request.descriptions = vec![None, Some("faster".to_string())];
    chat.pending_ask = Some((request, tx));
    chat.build_rows(100);
    let joined: String = chat
        .doc
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("1. A"), "option: {joined}");
    assert!(joined.contains("2. B"), "option: {joined}");
    assert!(joined.contains("  faster"), "desc dim row: {joined}");
    assert!(joined.contains("3. Other"), "other option: {joined}");
    assert!(joined.contains("Type something."), "placeholder: {joined}");
    assert!(
        chat.ask_key(KeyCode::Char('3'), KeyModifiers::empty()),
        "digit 3 → Other"
    );
    chat.build_rows(100);
    let joined: String = chat
        .doc
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("❯ 3. Other"), "other focused: {joined}");
    assert!(
        joined.contains("enter to submit · esc to cancel"),
        "input hint: {joined}"
    );
    for c in ['s', 'e', 'r', 'd', 'e'] {
        assert!(
            chat.ask_key(KeyCode::Char(c), KeyModifiers::empty()),
            "type {c}"
        );
    }
    assert!(
        chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
        "submit"
    );
    assert!(chat.pending_ask.is_none(), "dialog closed");
    assert_eq!(rx.try_recv(), Ok(DialogAction::Answer("serde".to_string())));
    // The answer enters the message flow: an ordinary user message (Q&A echo).
    let answer = chat
        .messages
        .last()
        .expect("the answer message entered the flow");
    assert_eq!(answer.role, Role::User);
    assert_eq!(
        answer.text,
        "User answered the questions:\n  · Which library? → serde"
    );
    chat.build_rows(100);
    let joined: String = chat
        .doc
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("User answered the questions:"),
        "result header: {joined}"
    );
    assert!(
        joined.contains("· Which library? → serde"),
        "result line: {joined}"
    );
    assert!(
        joined.contains("❯ "),
        "the answer renders as a user bubble: {joined}"
    );
}

#[test]
fn ask_other_empty_submit_cancels() {
    let mut chat = test_chat();
    let (tx, mut rx) = oneshot::channel();
    let mut request =
        PermissionRequest::new("Tech stack", "Which library?", vec!["A".into(), "B".into()]);
    request.free_text = true;
    chat.pending_ask = Some((request, tx));
    chat.ask_focus = 2;
    assert!(
        chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
        "empty Other submit"
    );
    assert!(chat.pending_ask.is_none());
    assert_eq!(rx.try_recv(), Ok(DialogAction::Cancel));
    // A decline also enters the message flow (an ordinary user message).
    let declined = chat
        .messages
        .last()
        .expect("the decline message entered the flow");
    assert_eq!(declined.role, Role::User);
    assert_eq!(declined.text, ASK_DECLINED_TEXT);
    chat.build_rows(100);
    let joined: String = chat
        .doc
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("User declined to answer questions"),
        "{joined}"
    );
}

#[test]
fn ask_arrow_keys_move_focus() {
    let mut chat = test_chat();
    let (tx, mut rx) = oneshot::channel();
    let mut request =
        PermissionRequest::new("Tech stack", "Which library?", vec!["A".into(), "B".into()]);
    request.free_text = true;
    chat.pending_ask = Some((request, tx));
    assert!(chat.ask_key(KeyCode::Down, KeyModifiers::empty()), "↓ to B");
    assert_eq!(chat.ask_focus, 1);
    assert!(
        chat.ask_key(KeyCode::Down, KeyModifiers::empty()),
        "↓ to Other"
    );
    assert_eq!(chat.ask_focus, 2);
    assert!(
        chat.ask_key(KeyCode::Down, KeyModifiers::empty()),
        "↓ at the bottom stops moving"
    );
    assert_eq!(chat.ask_focus, 2);
    assert!(
        chat.ask_key(KeyCode::Up, KeyModifiers::empty()),
        "↑ back to B"
    );
    assert_eq!(chat.ask_focus, 1);
    assert!(
        chat.ask_key(KeyCode::Enter, KeyModifiers::empty()),
        "Enter selects B"
    );
    assert_eq!(rx.try_recv(), Ok(DialogAction::Confirm(1)));
    let answer = chat
        .messages
        .last()
        .expect("the answer message entered the flow");
    assert_eq!(answer.role, Role::User);
    assert!(
        answer.text.contains("· Which library? → B"),
        "the option text is the answer: {}",
        answer.text
    );
}

/// Esc (while busy) sets the interrupt flag: background-task completion no longer auto-starts a turn;
/// a new turn (start_turn) resets it.
#[test]
fn esc_sets_interrupted_and_start_turn_resets() {
    let mut chat = test_chat();
    chat.busy = true;
    assert!(
        chat.on_key(KeyCode::Esc, KeyModifiers::empty()),
        "busy Esc interrupts"
    );
    assert!(chat.interrupted, "Esc sets interrupted");
    assert!(
        *chat.cancel_tx.borrow(),
        "the interrupt signal was sent (send_replace applies unconditionally)"
    );
    chat.busy = false;
    chat.interrupted = false;
    chat.busy = true;
    let _ = chat.cancel_tx.send_replace(true);
    let cancel_rx = chat.cancel_tx.subscribe();
    chat.cancel_tx.send_replace(false);
    assert!(
        !*cancel_rx.borrow(),
        "reset before a new turn starts: the receiver reads false"
    );
    drop(cancel_rx);
}

/// start_turn's reset order: subscribe first, then send_replace — after the previous turn's receivers are all
/// dropped (send does not update with no receivers), the new turn still sees false.
#[test]
fn cancel_reset_works_after_all_receivers_dropped() {
    let chat = test_chat();
    chat.cancel_tx.send_replace(true);
    drop(chat.cancel_tx.subscribe());
    let cancel_rx = chat.cancel_tx.subscribe();
    chat.cancel_tx.send_replace(false);
    assert!(
        !*cancel_rx.borrow(),
        "after all receivers drop, send_replace still resets (send would fail)"
    );
}

#[test]
fn image_ready_updates_cache_and_invalidates_render_cache() {
    let mut chat = test_chat();
    chat.reply_cache
        .insert("x".to_string(), vec![Line::plain("old")]);
    let meta = ImageMeta {
        cols: 5,
        rows: 3,
        bytes: vec![1, 2, 3],
    };
    chat.handle(UiEvent::ImageReady {
        url: "a.png".to_string(),
        meta: Some(meta.clone()),
    });
    assert!(
        chat.images.contains_key("a.png"),
        "a successful load lands in the cache"
    );
    assert_eq!(chat.images["a.png"].cols, 5);
    assert_eq!(
        chat.images_version, 2,
        "the version increments (starts at 1)"
    );
    assert!(chat.reply_cache.is_empty(), "reply_cache invalidated");

    chat.handle(UiEvent::ImageReady {
        url: "a.png".to_string(),
        meta: None,
    });
    assert!(
        !chat.images.contains_key("a.png"),
        "a failure removes the cache entry"
    );
    assert!(
        chat.warnings.iter().any(|(_, w)| w.contains("a.png")),
        "warning hint"
    );
}

#[test]
fn turn_end_without_capability_skips_image_loading() {
    let mut chat = test_chat();
    chat.apply_turn_start();
    chat.handle(UiEvent::TextDelta(
        "![img](https://example.com/i.png)".to_string(),
    ));
    chat.handle(UiEvent::TurnEnd);
    assert!(chat.images.is_empty(), "no capability → not loaded");
    assert!(chat.images_pending.is_empty());
}

/// TurnEnd → asynchronously load the data-URL image → ImageReady reply → the image block appears in the doc.
#[tokio::test]
async fn turn_end_loads_images_and_renders_image_block() {
    let mut chat = test_chat();
    chat.image_cap = Some(ImageCap::default_cells());
    let png = tiny_png();
    let url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&png)
    );
    chat.apply_turn_start();
    chat.handle(UiEvent::TextDelta(format!("![img]({url})")));
    chat.handle(UiEvent::TurnEnd);
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(5);
    while !chat.images.contains_key(&url) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "image load timed out"
        );
        chat.drain_all();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        chat.images_pending.is_empty(),
        "the in-flight set is cleared"
    );
    chat.build_rows(100);
    let image_rows = chat
        .doc
        .rows
        .iter()
        .filter(|r| r.line.image.is_some())
        .count();
    assert!(image_rows > 0, "image-block rows appear in the document");
    let meta = &chat.images[&url];
    assert_eq!(image_rows, meta.rows, "block rows = meta.rows");
}

/// A message with images still loading never settles — otherwise the `#[image]` fallback rows would flush
/// into scrollback, and since the kitty sequence is only emitted at flush time, the picture could never appear.
#[test]
fn message_waits_for_pending_images_before_settling() {
    let mut chat = test_chat();
    chat.image_cap = Some(ImageCap::default_cells());
    let url = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(tiny_png())
    );
    chat.messages
        .push(msg(Role::Assistant, &format!("![img]({url})")));
    // Load in flight (the effect of load_message_images).
    chat.images_pending.insert(url.clone());
    chat.build_rows(100);
    assert_eq!(
        settled_segments(&chat),
        1,
        "only the welcome card settles; a message with in-flight images does not"
    );

    // Load succeeds → the message settles, and flushed rows carry an ImageRef (the block head emits the kitty sequence).
    let meta = ImageMeta {
        cols: 4,
        rows: 2,
        bytes: tiny_png(),
    };
    chat.handle(UiEvent::ImageReady {
        url: url.clone(),
        meta: Some(meta),
    });
    chat.build_rows(100);
    assert_eq!(
        settled_segments(&chat),
        2,
        "the message settles once the image is ready"
    );
    let image_rows: Vec<&Row> = chat
        .doc
        .rows
        .iter()
        .take(chat.doc.settled)
        .filter(|r| r.line.image.is_some())
        .collect();
    assert!(!image_rows.is_empty(), "settled rows contain image blocks");
}

/// A failed load (including None from a timeout) also releases the block:
/// it settles with a failure-marked placeholder, distinguishable from a
/// still-loading one.
#[test]
fn failed_image_load_settles_with_placeholder() {
    let mut chat = test_chat();
    chat.image_cap = Some(ImageCap::default_cells());
    chat.messages
        .push(msg(Role::Assistant, "![img](missing.png)"));
    chat.images_pending.insert("missing.png".to_string());
    chat.build_rows(100);
    assert_eq!(
        settled_segments(&chat),
        1,
        "does not settle while in flight"
    );
    chat.handle(UiEvent::ImageReady {
        url: "missing.png".to_string(),
        meta: None,
    });
    chat.build_rows(100);
    assert_eq!(
        settled_segments(&chat),
        2,
        "settles normally after a failure"
    );
    let text: String = chat
        .doc
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("#[image ✗ load failed]"),
        "the failure marker lands in the settled text: {text}"
    );
}

/// Without image capability, nothing enters the in-flight set and messages settle immediately (unchanged behavior).
#[test]
fn without_image_capability_messages_settle_immediately() {
    let mut chat = test_chat();
    chat.messages.push(msg(Role::Assistant, "![img](a.png)"));
    chat.build_rows(100);
    assert!(chat.images_pending.is_empty());
    assert_eq!(
        settled_segments(&chat),
        2,
        "no capability → does not wait for images"
    );
}

/// Caret editing: ←/→ move, ctrl+a/e line start/end, alt+b/f word movement,
/// insertion lands at the caret, not the line end.
#[test]
fn cursor_moves_and_inserts_at_position() {
    let mut chat = chat_with_history("cursor");
    type_text(&mut chat, "hello world");
    assert_eq!(chat.cursor, chat.input.len());
    assert!(ctrl(&mut chat, 'a'));
    assert_eq!(chat.cursor, 0, "ctrl+a to the line start");
    assert!(press(&mut chat, KeyCode::Right));
    press(&mut chat, KeyCode::Char('i'));
    assert_eq!(chat.input, "hiello world", "inserts at the cursor");
    assert!(ctrl(&mut chat, 'e'));
    assert_eq!(chat.cursor, chat.input.len(), "ctrl+e to the line end");
    assert!(alt(&mut chat, 'b'));
    assert_eq!(chat.cursor, "hiello ".len(), "alt+b back one word");
    assert!(alt(&mut chat, 'f'));
    assert_eq!(chat.cursor, chat.input.len(), "alt+f forward one word");
    // CJK moves by character and renders by display width.
    chat.set_input("ＡＢ");
    press(&mut chat, KeyCode::Left);
    assert_eq!(chat.cursor, 3, "one char back at a time (3 bytes)");
}

/// ctrl+k/u/w delete into the kill buffer, ctrl+y pastes back; ctrl+d deletes after the caret.
#[test]
fn kill_ring_round_trip() {
    let mut chat = chat_with_history("kill");
    type_text(&mut chat, "alpha beta");
    assert!(ctrl(&mut chat, 'w'));
    assert_eq!(chat.input, "alpha ");
    assert!(ctrl(&mut chat, 'y'));
    assert_eq!(chat.input, "alpha beta", "ctrl+y pastes back");
    assert!(ctrl(&mut chat, 'a'));
    assert!(ctrl(&mut chat, 'k'));
    assert_eq!(chat.input, "", "ctrl+k deletes to the line end");
    assert!(ctrl(&mut chat, 'y'));
    assert_eq!(chat.input, "alpha beta");
    assert!(ctrl(&mut chat, 'u'));
    assert_eq!(chat.input, "", "ctrl+u deletes to the line start");
    chat.set_input("abc");
    chat.cursor = 1;
    assert!(ctrl(&mut chat, 'd'));
    assert_eq!(chat.input, "ac", "ctrl+d deletes the char after the cursor");
}

/// History: submitted entries go into history and persist; ↑/↓ navigate, back at the bottom restores the draft;
/// consecutive identical prompts are recorded once.
#[test]
fn prompt_history_persists_and_navigates() {
    let mut chat = chat_with_history("history");
    chat.record_history("first");
    chat.record_history("second");
    chat.record_history("second");
    assert_eq!(
        chat.history.entries(),
        ["first", "second"],
        "consecutive repeats record once"
    );
    // Persisted: a new session with the same home + cwd can read it.
    let reloaded = crate::tui::history::load(&chat.session.home, std::path::Path::new(&chat.cwd));
    assert_eq!(reloaded, vec!["first".to_string(), "second".to_string()]);

    chat.set_input("draft");
    press(&mut chat, KeyCode::Up);
    assert_eq!(chat.input, "second");
    press(&mut chat, KeyCode::Up);
    assert_eq!(chat.input, "first");
    press(&mut chat, KeyCode::Down);
    assert_eq!(chat.input, "second");
    press(&mut chat, KeyCode::Down);
    assert_eq!(chat.input, "draft", "back at the bottom, draft is restored");
    let _ = std::fs::remove_dir_all(&chat.session.home);
}

/// Multi-line input: `\`+Enter and ctrl+j insert newlines, Enter submits the whole;
/// rendered as multiple rows (each height=1, not a single row stuffed with \n).
#[test]
fn multiline_input_renders_as_multiple_rows() {
    let mut chat = chat_with_history("multiline");
    chat.width = 80;
    type_text(&mut chat, "first\\");
    assert!(press(&mut chat, KeyCode::Enter), "\\+Enter newline");
    type_text(&mut chat, "second");
    assert!(ctrl(&mut chat, 'j'), "ctrl+j newline");
    type_text(&mut chat, "third");
    assert_eq!(chat.input, "first\nsecond\nthird");
    let rows = chat.prompt_lines();
    assert_eq!(rows.len(), 3, "three lines of input = three Rows");
    for row in &rows {
        assert!(!row.plain_text().contains('\n'), "rows contain no newline");
    }
    assert!(
        rows[2].plain_text().contains('▋'),
        "the caret is drawn on the last row"
    );
    // ↑ moves along visual rows within a multi-line input before switching history.
    chat.record_history("older");
    press(&mut chat, KeyCode::Up);
    assert_eq!(
        chat.input, "first\nsecond\nthird",
        "in-line movement does not touch the text"
    );
    press(&mut chat, KeyCode::Up);
    press(&mut chat, KeyCode::Up);
    assert_eq!(
        chat.input, "older",
        "history switches only at the first line"
    );
    let _ = std::fs::remove_dir_all(&chat.session.home);
}

/// The input area has a row cap: long input only shows the screen around the caret.
#[test]
fn prompt_rows_are_capped() {
    let mut chat = chat_with_history("caprows");
    chat.width = 40;
    chat.set_input(
        (0..30)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n"),
    );
    assert_eq!(chat.prompt_lines().len(), INPUT_ROWS_MAX);
}

/// Ctrl+C (CC semantics): interrupts when busy; with text, clears it first (into history);
/// on empty input, first press hints and a second within the window quits; the counter resets on timeout.
#[test]
fn ctrl_c_interrupt_clear_then_exit() {
    let mut chat = chat_with_history("ctrlc");
    let t0 = std::time::Instant::now();
    chat.busy = true;
    chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
    assert!(chat.interrupted, "busy → interrupt");
    assert!(!chat.exit);

    chat.busy = false;
    chat.set_input("draft");
    chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
    assert_eq!(chat.input, "", "non-empty input clears first");
    assert!(!chat.exit, "clearing does not exit");
    assert_eq!(
        chat.history.entries().last().map(String::as_str),
        Some("draft")
    );

    chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
    assert_eq!(chat.notice, Some("Press ctrl-c again to exit"));
    assert!(!chat.exit, "the first time only hints");
    chat.on_key_at(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        t0 + CTRL_C_WINDOW,
    );
    assert!(chat.exit, "the second press inside the window exits");

    // The counter restarts after the window expires.
    let mut chat = chat_with_history("ctrlc2");
    chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
    chat.on_key_at(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        t0 + CTRL_C_WINDOW + std::time::Duration::from_millis(1),
    );
    assert!(!chat.exit, "outside the window: no exit, just a new hint");
    assert_eq!(chat.notice, Some("Press ctrl-c again to exit"));
    let _ = std::fs::remove_dir_all(&chat.session.home);
}

/// A turn whose task died never clears `busy`, and every quit route is gated on `busy` —
/// the session used to answer only to `kill`. An interrupt left unhonoured past
/// [`INTERRUPT_GRACE`] gives Ctrl+C its exit meaning back.
#[test]
fn ctrl_c_force_quits_a_turn_that_never_stops() {
    let mut chat = chat_with_history("wedged");
    let t0 = std::time::Instant::now();
    chat.busy = true;

    chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
    assert!(chat.interrupted, "the first press asks the turn to stop");
    assert!(
        !chat.exit,
        "a turn that may still be stopping is not killed"
    );

    chat.on_key_at(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        t0 + INTERRUPT_GRACE - std::time::Duration::from_millis(1),
    );
    assert!(!chat.exit, "inside the grace it stays an interrupt");

    chat.on_key_at(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        t0 + INTERRUPT_GRACE,
    );
    assert!(chat.exit, "still busy past the grace → exit");
    let _ = std::fs::remove_dir_all(&chat.session.home);
}

/// The escape hatch must not fire on a healthy interrupt: the next turn clears the
/// stamp, so Ctrl+C during it hints and interrupts exactly as before.
#[test]
fn a_new_turn_rearms_the_ordinary_interrupt() {
    let mut chat = chat_with_history("rearm");
    let t0 = std::time::Instant::now();
    chat.busy = true;
    chat.on_key_at(KeyCode::Char('c'), KeyModifiers::CONTROL, t0);
    assert!(chat.interrupt_at.is_some());

    chat.apply_event(UiEvent::TurnStart);
    assert_eq!(
        chat.interrupt_at, None,
        "a fresh turn is owed a fresh grace"
    );

    chat.on_key_at(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
        t0 + INTERRUPT_GRACE * 10,
    );
    assert!(!chat.exit, "the new turn's first press only interrupts");
    let _ = std::fs::remove_dir_all(&chat.session.home);
}

/// A panic inside the turn task is swallowed by tokio (nothing joins the handle) and the
/// terminal repaints over its message, so the only visible symptom was a session stuck
/// on "Working…" forever. The supervisor turns it into the ordinary long-turn error.
#[tokio::test]
async fn a_lost_turn_reports_itself_instead_of_latching_busy() {
    let (events, mut rx) = mpsc::unbounded_channel();
    // The panic below prints its own backtrace line; that noise is the point — in the
    // TUI it lands on the alternate screen and is repainted away within a frame.
    let handle = tokio::spawn(async { panic!("turn task died") });

    Chat::supervise_turn(events, handle);

    let event = rx.recv().await;
    assert!(
        matches!(
            event,
            Some(UiEvent::Error { code, context, .. })
                if code == crate::error::TURN_LOST
                    && context == crate::error::ErrorContext::LongTurn
        ),
        "a lost turn reports itself: {event:?}"
    );
}

/// Esc: interrupts when busy; closes suggestions/panels layer by layer; double-press with text clears and saves to history.
#[test]
fn esc_closes_layers_then_clears_input() {
    let mut chat = chat_with_history("esc");
    let t0 = std::time::Instant::now();
    chat.busy = true;
    chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
    assert!(chat.interrupted, "busy → interrupt");

    chat.busy = false;
    chat.set_input("/");
    assert!(!chat.slash_suggestions.is_empty());
    chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
    assert!(
        chat.slash_suggestions.is_empty(),
        "the dropdown closes first"
    );
    assert_eq!(
        chat.input, "",
        "the slash query clears with the dropdown (no more leftover //)"
    );

    chat.set_input("hello");
    chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
    assert_eq!(chat.input, "hello", "the first press only arms it");
    assert_eq!(chat.notice, Some("Press esc again to clear"));
    chat.on_key_at(KeyCode::Esc, KeyModifiers::empty(), t0);
    assert_eq!(chat.input, "", "double-press clears");
    assert_eq!(
        chat.history.entries().last().map(String::as_str),
        Some("hello")
    );
    let _ = std::fs::remove_dir_all(&chat.session.home);
}

/// Shift+Tab cycles the permission mode, and it really applies to the next turn's Session.
#[test]
fn shift_tab_cycles_permission_mode() {
    let mut chat = chat_with_history("mode");
    assert_eq!(chat.permission_mode, PermissionMode::Default);
    press(&mut chat, KeyCode::BackTab);
    assert_eq!(chat.permission_mode, PermissionMode::AcceptEdits);
    assert_eq!(
        chat.permission_mode_label(),
        "acceptEdits",
        "the footer badge shares a source"
    );
    press(&mut chat, KeyCode::BackTab);
    assert_eq!(chat.permission_mode, PermissionMode::Plan);
    press(&mut chat, KeyCode::BackTab);
    assert_eq!(
        chat.permission_mode,
        PermissionMode::Default,
        "cycles back to default"
    );
    // The turn's Session carries the current mode (Session is immutable in Arc → derive a copy).
    press(&mut chat, KeyCode::BackTab);
    assert_eq!(
        chat.session_for_turn().permission_mode,
        PermissionMode::AcceptEdits
    );
    assert_eq!(
        chat.session.permission_mode,
        PermissionMode::Default,
        "the original Session is unchanged"
    );

    // A session started in bypass only toggles between bypass ↔ default (never introduces a new dangerous mode).
    let mut chat = chat_with_history("mode-bypass");
    chat.permission_mode = PermissionMode::BypassPermissions;
    let mut session = (*chat.session).clone();
    session.permission_mode = PermissionMode::BypassPermissions;
    chat.session = Arc::new(session);
    press(&mut chat, KeyCode::BackTab);
    assert_eq!(chat.permission_mode, PermissionMode::Default);
    press(&mut chat, KeyCode::BackTab);
    assert_eq!(chat.permission_mode, PermissionMode::BypassPermissions);
}

/// Enter while busy is no longer a no-op: messages queue and show below the input; ↑ pulls back the last one.
#[test]
fn messages_queue_while_busy() {
    let mut chat = chat_with_history("queue");
    chat.busy = true;
    chat.set_input("first queued");
    chat.submit();
    assert_eq!(
        chat.queued,
        vec![QueuedInput {
            text: "first queued".into(),
            is_slash: false
        }]
    );
    assert_eq!(chat.input, "", "the input clears after enqueueing");
    chat.set_input("second queued");
    chat.submit();
    assert_eq!(chat.queued.len(), 2);
    let lines = chat.queue_lines();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].starts_with("> first queued"), "{lines:?}");
    // While busy, ↑ pulls back the last queued message for further editing.
    press(&mut chat, KeyCode::Up);
    assert_eq!(chat.input, "second queued");
    assert_eq!(chat.queued.len(), 1);
}

/// Busy dispatch (contract §4.2): instant commands run immediately and never reset
/// `busy`; other slash commands queue with the slash marker; plain messages queue.
#[test]
fn busy_dispatch_runs_instant_and_queues_the_rest() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-busy", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.join("home"));
    chat.cwd = tmp.display().to_string();
    chat.busy = true;

    // Instant: /think xhigh applies now, not queued; busy stays true.
    chat.set_input("/think xhigh");
    chat.submit();
    assert_eq!(
        chat.session.runtime.thinking.borrow().as_deref(),
        Some("xhigh"),
        "whitelisted commands apply immediately while busy"
    );
    assert!(chat.busy, "the whitelist path does not reset busy");
    assert!(chat.queued.is_empty(), "whitelisted commands do not queue");
    let out = chat.slash_lines.join("\n");
    assert!(out.contains("✓ thinking level set: xhigh"), "{out}");

    chat.set_input("/gc");
    chat.submit();
    assert!(chat.busy, "refused cleanup does not reset the active turn");
    assert!(chat.queued.is_empty(), "refused cleanup is never queued");
    assert!(
        chat.slash_error_lines
            .join("\n")
            .contains("cannot clean session data mid-turn")
    );

    chat.set_input("/model deepseek-v4");
    chat.submit();
    assert_eq!(*chat.session.runtime.model.borrow(), "test-model");
    assert!(
        chat.slash_error_lines
            .join("\n")
            .contains("cannot switch models mid-turn")
    );

    // Non-instant slash: queued with the slash marker (never sent as a prompt).
    chat.set_input("/clear");
    chat.submit();
    assert_eq!(
        chat.queued,
        vec![QueuedInput {
            text: "/clear".into(),
            is_slash: true
        }],
        "non-whitelisted slash commands queue with a marker"
    );

    // Plain message: queued without the marker.
    chat.set_input("hello");
    chat.submit();
    assert_eq!(chat.queued.len(), 2);
    assert!(!chat.queued[1].is_slash);
    let _ = std::fs::remove_dir_all(&tmp);
}

/// After TurnEnd, queued slash commands drain through `run_slash` (not `start_turn`),
/// in order, until a plain message starts the next turn.
#[tokio::test]
async fn queued_slashes_drain_through_run_slash() {
    let tmp = std::env::temp_dir().join(format!("bingo-slash-{}-drain", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let mut chat = test_chat_home(tmp.join("home"));
    chat.cwd = tmp.display().to_string();
    chat.queued = vec![
        QueuedInput {
            text: "/think low".into(),
            is_slash: true,
        },
        QueuedInput {
            text: "/nope".into(),
            is_slash: true,
        },
        QueuedInput {
            text: "the message".into(),
            is_slash: false,
        },
    ];
    chat.submit_queued();
    // Both slash commands ran (think applied + unknown guidance), then the message started a turn.
    assert_eq!(
        chat.session.runtime.thinking.borrow().as_deref(),
        Some("low"),
        "queued slash commands run as commands"
    );
    let out = all_slash_text(&chat);
    assert!(
        out.contains("unknown command: /nope") && out.contains("code=UNKNOWN_COMMAND"),
        "unknown commands get guidance instead of the model: {out}"
    );
    assert!(chat.busy, "the last plain message starts a new turn");
    assert_eq!(chat.messages.last().map(|m| m.role), Some(Role::User));
    assert!(
        chat.messages
            .last()
            .is_some_and(|m| m.text == "the message"),
        "plain messages reach the model via start_turn"
    );
    let _ = std::fs::remove_dir_all(&tmp);
}

/// Bottom entity area lists only running entities with their engine; Ctrl+G
/// opens the full workspace directly instead of focusing an inline selector.
#[test]
fn entity_area_filters_idle_agents_and_ctrl_g_opens_workspace() {
    let mut chat = test_chat();
    chat.width = 100;
    assert!(chat.entity_rows(100).is_empty());

    let running = chat.session.clone();
    let _ = running.runtime.model_tx.send("gpt-5.6-sol".to_string());
    let _ = running.runtime.thinking_tx.send(Some("max".to_string()));
    chat.session.agents.insert(
        "scout",
        crate::agents::AgentKind::Hire,
        None,
        "research".into(),
        running,
    );
    chat.session.agents.insert(
        "reviewer",
        crate::agents::AgentKind::Hire,
        None,
        "review".into(),
        chat.session.clone(),
    );
    let _ = chat.session.agents.finish("reviewer", Vec::new(), 0);
    chat.session
        .channels
        .create("table", vec![], crate::channels::ChannelMode::Serial)
        .unwrap_or_else(|e| panic!("{e}"));

    chat.refresh_entities();
    assert_eq!(chat.entities.len(), 2, "running agent plus channel");
    assert!(
        chat.entities
            .iter()
            .all(|e| !matches!(e, EntityRow::Agent { name, .. } if name == "reviewer")),
        "idle agents stay out of the compact entity area"
    );
    let summary = chat.entity_rows(100)[0].plain_text();
    assert!(
        summary.contains("◉ scout · gpt-5.6-sol · max · running"),
        "{summary}"
    );
    assert!(summary.contains("◇ #table(0)"), "{summary}");

    assert!(chat.on_key(KeyCode::Char('g'), KeyModifiers::CONTROL));
    assert_eq!(chat.open_entity, Some(EntityOpen::Workspace));
}

/// Running agents can be selected from the entity area and Enter opens that exact DM.
#[test]
fn entity_area_selects_running_agent_and_enter_opens_dm() {
    let mut chat = test_chat();
    chat.session.agents.insert(
        "scout",
        crate::agents::AgentKind::Hire,
        None,
        "research".into(),
        chat.session.clone(),
    );
    chat.session.agents.insert(
        "reviewer",
        crate::agents::AgentKind::Hire,
        None,
        "review".into(),
        chat.session.clone(),
    );
    chat.refresh_entities();

    assert!(chat.on_key(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(chat.entity_focus, Some(0));
    assert!(
        chat.entity_rows(100)[0]
            .plain_text()
            .contains("❯ ◉ reviewer")
    );
    assert!(chat.on_key(KeyCode::Down, KeyModifiers::NONE));
    assert_eq!(chat.entity_focus, Some(1));
    assert!(chat.on_key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(chat.open_entity, Some(EntityOpen::Agent("scout".into())));
    assert_eq!(chat.entity_focus, None);
}

/// Ctrl+B owns list/detail navigation and x stops the selected running agent.
#[test]
fn agent_manager_lists_opens_details_and_stops_agents() {
    let mut chat = test_chat();
    chat.session.agents.insert(
        "alpha",
        crate::agents::AgentKind::Hire,
        None,
        "first agent".into(),
        chat.session.clone(),
    );
    chat.session.agents.insert(
        "scout",
        crate::agents::AgentKind::Hire,
        None,
        "inspect the code".into(),
        chat.session.clone(),
    );
    chat.session
        .agents
        .set_prompt("scout", "Find the rendering seam".into());
    chat.session.agents.set_progress_snapshot(
        "scout",
        crate::agents::AgentProgress {
            started_at: Some(std::time::Instant::now()),
            output_tokens: 123,
            tool_uses: 2,
            recent_activity: vec!["⏺Read(src/tui/chat.rs)".into()],
        },
    );

    assert!(chat.on_key(KeyCode::Char('b'), KeyModifiers::CONTROL));
    let list = chat
        .agent_manager_rows(100)
        .iter()
        .map(|row| row.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(list.contains("Background agents · 2 running"), "{list}");
    assert!(list.contains("scout · inspect the code"), "{list}");
    assert!(list.contains("123 tokens · 2 tools"), "{list}");
    assert!(list.contains("Read(src/tui/chat.rs)"), "{list}");
    assert!(chat.on_key(KeyCode::Down, KeyModifiers::NONE));
    assert!(chat.on_key(KeyCode::Char('x'), KeyModifiers::NONE));
    let statuses = chat.session.agents.list();
    assert_eq!(
        statuses
            .iter()
            .find(|status| status.name == "scout")
            .map(|status| status.state),
        Some(crate::agents::AgentState::Stopped),
        "x stops the selected row rather than the first row"
    );
    assert_eq!(
        statuses
            .iter()
            .find(|status| status.name == "alpha")
            .map(|status| status.state),
        Some(crate::agents::AgentState::Running)
    );
    assert!(
        chat.agent_manager_rows(100).len() <= AGENT_MANAGER_ROWS_MAX + 4,
        "manager list stays bounded"
    );

    chat.session.agents.insert(
        "scout",
        crate::agents::AgentKind::Hire,
        None,
        "inspect the code".into(),
        chat.session.clone(),
    );
    chat.session
        .agents
        .set_prompt("scout", "Find the rendering seam".into());
    chat.session.agents.set_progress_snapshot(
        "scout",
        crate::agents::AgentProgress {
            started_at: Some(std::time::Instant::now()),
            output_tokens: 123,
            tool_uses: 2,
            recent_activity: vec!["⏺Read(src/tui/chat.rs)".into()],
        },
    );
    assert!(chat.on_key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        chat.agent_manager,
        Some(AgentManager::Detail {
            name: "scout".into()
        })
    );
    let detail = chat
        .agent_manager_rows(100)
        .iter()
        .map(|row| row.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(detail.contains("scout › inspect the code"), "{detail}");
    assert!(detail.contains("Prompt"), "{detail}");
    assert!(detail.contains("Find the rendering seam"), "{detail}");
    assert!(detail.contains("Progress"), "{detail}");
    assert!(
        chat.agent_manager_rows(100).len() <= AGENT_PROMPT_ROWS_MAX + 12,
        "detail prompt is bounded"
    );
    assert!(
        chat.has_dynamic_rows(),
        "an open running detail keeps elapsed live"
    );

    assert!(chat.on_key(KeyCode::Char('x'), KeyModifiers::NONE));
    assert!(chat.agent_manager.is_none());
    assert_eq!(
        chat.session
            .agents
            .list()
            .iter()
            .find(|status| status.name == "scout")
            .map(|status| status.state),
        Some(crate::agents::AgentState::Stopped)
    );
}

/// Queues beyond the cap fold into one row (row count feeds chrome, so it must be bounded).
#[test]
fn queue_lines_are_capped() {
    let mut chat = chat_with_history("queuecap");
    chat.queued = (0..10)
        .map(|i| QueuedInput {
            text: format!("m{i}"),
            is_slash: false,
        })
        .collect();
    assert_eq!(chat.queue_lines().len(), QUEUE_ROWS_MAX + 1);
    assert!(
        chat.queue_lines()
            .last()
            .is_some_and(|l| l.contains("more queued"))
    );
}

/// `?`: toggles the panel on empty input; an ordinary character otherwise.
#[test]
fn question_mark_toggles_help_panel() {
    let mut chat = chat_with_history("help");
    chat.width = 100;
    chat.height = 40;
    press(&mut chat, KeyCode::Char('?'));
    assert!(chat.help_visible);
    assert!(!chat.help_lines().is_empty(), "the panel has content");
    assert!(chat.input.is_empty(), "? does not enter the input");
    press(&mut chat, KeyCode::Char('?'));
    assert!(!chat.help_visible, "pressed again, closes");
    assert!(chat.help_lines().is_empty());
    type_text(&mut chat, "why");
    press(&mut chat, KeyCode::Char('?'));
    assert_eq!(chat.input, "why?", "with text present it is a plain char");
    assert!(!chat.help_visible);
}

/// Help panel rows are bounded by the terminal height (the canvas must never exceed it).
#[test]
fn help_panel_shrinks_on_short_terminals() {
    let mut chat = chat_with_history("helpshort");
    chat.width = 100;
    chat.help_visible = true;
    chat.height = 40;
    let tall = chat.help_lines().len();
    chat.height = 14;
    let short = chat.help_lines().len();
    assert!(
        short < tall,
        "short terminals get a shorter panel: {short} vs {tall}"
    );
    assert!(
        short + 9 <= 14,
        "the panel + remaining chrome fit within the terminal height"
    );
    chat.height = 6;
    assert!(
        chat.help_lines().is_empty(),
        "very short terminals show no panel"
    );
}

/// ctrl+s stash/restore (with the caret), ctrl+_ undo, ctrl+t task area, ctrl+l repaint.
#[test]
fn stash_undo_tasks_and_redraw() {
    let mut chat = chat_with_history("t2");
    type_text(&mut chat, "stashed");
    chat.cursor = 3;
    assert!(ctrl(&mut chat, 's'));
    assert_eq!(chat.input, "", "ctrl+s stashes and clears");
    assert!(ctrl(&mut chat, 's'));
    assert_eq!(
        (chat.input.as_str(), chat.cursor),
        ("stashed", 3),
        "the restore includes the caret"
    );

    // Undo: a bulk edit (kill) steps back one.
    chat.set_input("undo me");
    chat.cursor = chat.input.len();
    assert!(ctrl(&mut chat, 'w'));
    assert_eq!(chat.input, "undo ");
    assert!(ctrl(&mut chat, '7'), "ctrl+_ arrives as ctrl+7");
    assert_eq!(chat.input, "undo me", "undo returns to before the deletion");

    assert!(!chat.tasks_visible);
    assert!(ctrl(&mut chat, 't'));
    assert!(chat.tasks_visible, "ctrl+t shows the task area");
    assert!(ctrl(&mut chat, 't'));
    assert!(!chat.tasks_visible);

    assert!(ctrl(&mut chat, 'l'));
    assert!(chat.force_redraw, "ctrl+l requests a full-screen redraw");
}

/// bash mode: empty-input Esc/backspace/ctrl+u exit; Tab completes from this session's `!` history.
#[test]
fn bash_mode_exits_and_completes() {
    let mut chat = chat_with_history("bash");
    chat.bash_history.push("cargo test --all".to_string());
    press(&mut chat, KeyCode::Char('!'));
    assert!(chat.bash_mode);
    press(&mut chat, KeyCode::Esc);
    assert!(!chat.bash_mode, "empty input + Esc exits shell mode");
    press(&mut chat, KeyCode::Char('!'));
    assert!(ctrl(&mut chat, 'u'));
    assert!(!chat.bash_mode, "empty input + ctrl+u exits");
    press(&mut chat, KeyCode::Char('!'));
    type_text(&mut chat, "cargo");
    press(&mut chat, KeyCode::Tab);
    assert_eq!(chat.input, "cargo test --all", "Tab prefix completion");
}

/// Paste burst: Enter inside a burst is a newline, not send; ≥10 lines fold into a placeholder,
/// with the real content expanded at submit time.
#[test]
fn paste_burst_inserts_newlines_and_collapses() {
    let mut chat = chat_with_history("paste");
    let mut now = std::time::Instant::now();
    let fast = std::time::Duration::from_millis(1);
    // "Paste" 12 lines character by character.
    for i in 0..12 {
        for c in format!("line{i}").chars() {
            now += fast;
            chat.on_key_at(KeyCode::Char(c), KeyModifiers::empty(), now);
        }
        now += fast;
        chat.on_key_at(KeyCode::Enter, KeyModifiers::empty(), now);
    }
    assert!(!chat.busy, "Enter during a paste does not send");
    assert!(
        chat.input.starts_with("[Pasted text #1 +"),
        "placeholder: {}",
        chat.input
    );
    assert_eq!(chat.pastes.len(), 1);
    assert!(
        chat.expand_pastes(&chat.input).contains("line11"),
        "the real content expands on submit"
    );

    // Normal typing (wide intervals): Enter submits as usual instead of inserting a newline.
    let mut chat = chat_with_history("paste2");
    chat.busy = true; // queueing path: no tokio runtime needed
    let slow = std::time::Duration::from_millis(50);
    let mut now = std::time::Instant::now();
    for c in "hi".chars() {
        now += slow;
        chat.on_key_at(KeyCode::Char(c), KeyModifiers::empty(), now);
    }
    now += slow;
    chat.on_key_at(KeyCode::Enter, KeyModifiers::empty(), now);
    assert_eq!(chat.input, "", "Enter submits instead of a newline");
    assert_eq!(
        chat.queued,
        vec![QueuedInput {
            text: "hi".into(),
            is_slash: false
        }]
    );
}

/// Bracketed paste: the whole chunk inserts at the caret as one undo step; ≥10 lines fold into a placeholder,
/// with the real content expanded at submit time. CR newlines (what terminals paste) are normalized first.
#[test]
fn bracketed_paste_inserts_and_collapses() {
    let mut chat = chat_with_history("paste-real");
    chat.set_input("ab");
    chat.cursor = 1;
    chat.on_paste("X");
    assert_eq!(chat.input, "aXb", "inserts at the cursor");
    assert_eq!(chat.cursor, 2);
    chat.undo_edit();
    assert_eq!(chat.input, "ab", "one paste = one undo step");

    // Short chunks do not fold (below the threshold).
    let mut chat = chat_with_history("paste-short");
    chat.on_paste("line1\nline2");
    assert_eq!(chat.input, "line1\nline2");
    assert!(chat.pastes.is_empty(), "below the threshold, no folding");

    // ≥ PASTE_COLLAPSE_LINES lines fold; CR and CRLF both count as newlines.
    let mut chat = chat_with_history("paste-fold");
    let body: String = (0..PASTE_COLLAPSE_LINES)
        .map(|i| format!("line{i}\r"))
        .collect();
    chat.on_paste(&body);
    assert!(
        chat.input.starts_with("[Pasted text #1 +"),
        "placeholder: {}",
        chat.input
    );
    assert_eq!(chat.cursor, chat.input.len());
    assert!(
        chat.expand_pastes(&chat.input).contains("line9"),
        "the real content expands on submit"
    );
    assert!(
        !chat.expand_pastes(&chat.input).contains('\r'),
        "CR is normalized"
    );

    // An empty paste does nothing (no undo-stack write).
    let mut chat = chat_with_history("paste-empty");
    chat.on_paste("");
    assert!(chat.input.is_empty());
    assert!(chat.undo.is_empty());
}

/// Generates a test PNG and returns its path.
fn test_png_path(dir: &std::path::Path, name: &str, w: u32, h: u32) -> std::path::PathBuf {
    let path = dir.join(name);
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255u8, 0, 0, 255]));
    image::DynamicImage::ImageRgba8(img)
        .write_to(
            &mut std::fs::File::create(&path).unwrap(),
            image::ImageFormat::Png,
        )
        .unwrap();
    path
}

/// A standalone image path line at submit time → register the attachment + `#[image N]` placeholder (text kept).
#[test]
fn image_path_line_becomes_marker_on_submit() {
    let mut chat = chat_with_history("img-path");
    let dir = std::env::temp_dir().join(format!("bingo-img-dir-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let png = test_png_path(&dir, "a.png", 8, 8);
    chat.set_input(format!("take a look at this image\n{}", png.display()));
    chat.busy = true; // take the queue path: no tokio runtime needed
    chat.submit();
    assert_eq!(chat.queued.len(), 1);
    assert_eq!(
        chat.queued[0].text,
        format!("take a look at this image\n#[image 1]"),
        "the path line becomes a placeholder: {}",
        chat.queued[0].text
    );
    assert_eq!(chat.session.attachments.len(), 1);
    assert_eq!(
        chat.session.attachments.get(1).unwrap().media_type,
        "image/png"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A whole `![alt](path)` line is recognized too; non-image paths/missing files stay as-is.
#[test]
fn markdown_image_syntax_and_non_image_lines() {
    let mut chat = chat_with_history("img-md");
    let dir = std::env::temp_dir().join(format!("bingo-img-md-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let png = test_png_path(&dir, "b.png", 4, 4);
    let txt = dir.join("note.txt");
    std::fs::write(&txt, "hi").unwrap();
    chat.set_input(format!("![img]({})\n{}", png.display(), txt.display()));
    chat.busy = true;
    chat.submit();
    assert_eq!(
        chat.queued[0].text,
        format!("#[image 1]\n{}", txt.display())
    );
    assert_eq!(chat.session.attachments.len(), 1, "txt is not registered");
    let _ = std::fs::remove_dir_all(&dir);
}

/// resolve_images: picks attachments by placeholder number (deduped, out-of-range ignored).
#[test]
fn resolve_images_extracts_attachments_in_order() {
    let mut chat = chat_with_history("img-resolve");
    let dir = std::env::temp_dir().join(format!("bingo-img-rs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = test_png_path(&dir, "a.png", 4, 4);
    let b = test_png_path(&dir, "b.png", 6, 6);
    let id1 = chat.register_image_file(&a).unwrap();
    let id2 = chat.register_image_file(&b).unwrap();
    let text =
        format!("look at #[image {id1}] and #[image {id2}], then #[image {id1}] and #[image 99]");
    let imgs = chat.resolve_images(&text);
    assert_eq!(imgs.len(), 2, "dedup + out-of-range ignored");
    assert_eq!(
        imgs[0].data,
        chat.session.attachments.get(id1).unwrap().data
    );
    assert_eq!(
        imgs[1].data,
        chat.session.attachments.get(id2).unwrap().data
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// ctrl+r reverse search: filter hits, press again for older, Tab adopts and keeps editing,
/// ctrl+c cancels and restores.
#[test]
fn reverse_search_walks_history() {
    let mut chat = chat_with_history("search");
    for entry in ["cargo test", "git status", "cargo build"] {
        chat.record_history(entry);
    }
    chat.set_input("keep");
    assert!(ctrl(&mut chat, 'r'));
    assert!(chat.search.is_some(), "enters search mode");
    let line = chat.search_line().expect("search row");
    assert!(
        line.starts_with("(reverse-i-search)`': cargo build"),
        "{line}"
    );
    assert!(line.contains("enter submit"), "key hints visible: {line}");
    type_text(&mut chat, "cargo");
    assert_eq!(
        chat.search.as_ref().and_then(|s| s.hit.clone()).as_deref(),
        Some("cargo build")
    );
    assert!(ctrl(&mut chat, 'r'), "pressing again finds an older match");
    assert_eq!(
        chat.search.as_ref().and_then(|s| s.hit.clone()).as_deref(),
        Some("cargo test")
    );
    // In search mode, the input row shows the hit.
    assert_eq!(chat.prompt_lines()[0].plain_text(), "cargo test");
    press(&mut chat, KeyCode::Tab);
    assert!(chat.search.is_none(), "Tab accepts and exits search");
    assert_eq!(chat.input, "cargo test");

    // ctrl+c cancels: the input restores to its pre-search content.
    chat.set_input("keep");
    ctrl(&mut chat, 'r');
    ctrl(&mut chat, 'c');
    assert!(chat.search.is_none(), "ctrl+c exits search");
    assert_eq!(chat.input, "keep", "cancelling does not change the input");
    let _ = std::fs::remove_dir_all(&chat.session.home);
}

/// Alt+T thinking toggle: off ↔ the previous level.
#[test]
fn alt_t_toggles_thinking() {
    let mut chat = chat_with_history("think");
    let _ = chat
        .session
        .runtime
        .thinking_tx
        .send(Some("high".to_string()));
    alt(&mut chat, 't');
    assert_eq!(
        *chat.session.runtime.thinking.borrow(),
        None,
        "thinking turned off"
    );
    alt(&mut chat, 't');
    assert_eq!(
        chat.session.runtime.thinking.borrow().as_deref(),
        Some("high"),
        "restores the last level"
    );
}

/// Task area (CC glyphs): `☐`/`☒`, completed items dimmed + strikethrough semantics.
#[test]
fn task_lines_use_checkbox_glyphs() {
    let mut chat = chat_with_history("todo");
    chat.tasks_visible = true;
    chat.tasks_cache = vec![
        TodoItem {
            text: "done one".into(),
            status: TodoStatus::Done,
        },
        TodoItem {
            text: "doing".into(),
            status: TodoStatus::InProgress,
        },
        TodoItem {
            text: "later".into(),
            status: TodoStatus::Pending,
        },
    ];
    let lines = chat.task_lines();
    let joined: Vec<String> = lines.iter().map(|l| l.plain_text()).collect();
    assert!(joined[0].contains("todo · 1/3 tasks"), "{joined:?}");
    assert!(joined.iter().any(|l| l == "☒ done one"), "{joined:?}");
    assert!(joined.iter().any(|l| l == "☐ doing"), "{joined:?}");
    assert!(joined.iter().any(|l| l == "☐ later"), "{joined:?}");
    assert!(
        !joined
            .iter()
            .any(|l| l.contains("[x]") || l.contains("[ ]"))
    );
    let done_text = lines
        .iter()
        .find(|l| l.plain_text() == "☒ done one")
        .and_then(|l| l.segs.last())
        .expect("done seg");
    assert!(
        done_text.style.strikethrough,
        "done items carry strikethrough semantics"
    );
    assert_eq!(
        done_text.style.fg,
        Some(chat.theme.inactive),
        "and render dimmed"
    );
}

/// Empty-input placeholder hint (CC placeholder); gone once there is input.
#[test]
fn empty_prompt_shows_placeholder() {
    let mut chat = chat_with_history("placeholder");
    let lines = chat.prompt_lines();
    assert_eq!(lines.len(), 1);
    let text = lines[0].plain_text();
    // Caret sits ON the first placeholder cell: `▋` replaces the first
    // char instead of being glued in front of the full hint.
    let mut rest = crate::tui::keys::INPUT_PLACEHOLDER.chars();
    rest.next();
    assert_eq!(text, format!("▋{}", rest.as_str()), "{text}");
    chat.set_input("x");
    let text = chat.prompt_lines()[0].plain_text();
    assert_eq!(text, "x▋", "with input there is no placeholder");
}

/// A 4×2 solid-color PNG (for tests).
fn tiny_png() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(4, 2, image::Rgba([255u8, 0, 0, 255]));
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

// ---- #18 presentation-layer minimal implementation: error-row highlight + full-screen state + retry/back ----

/// #18 full-flow full-screen error state: inject a Full-level fixture → `last_error` recorded →
/// `Frame::assemble` produces the full-screen error rows (title/stable code/actions) → Esc returns and clears the error state
/// (AC-26/53: the way back is not a dead end).
#[test]
fn full_error_shows_full_screen_and_esc_returns() {
    use crate::error::ErrorLevel;
    use crate::tui::app::Frame;
    use crate::tui::test_util::error_fixtures;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::layout::Size;
    let mut chat = test_chat();
    let fx = error_fixtures()
        .into_iter()
        .find(|f| f.code == "AUTH_REQUIRED")
        .expect("FX-04 is in the fixture list");
    fx.inject(&chat.events);
    chat.drain_events();
    let err = chat
        .last_error
        .as_ref()
        .expect("the error state was recorded");
    assert_eq!(err.code, "AUTH_REQUIRED");
    assert_eq!(err.level, ErrorLevel::Full);
    let frame = Frame::assemble(&chat, Size::new(80, 24));
    let joined: String = frame
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("something went wrong"),
        "fullscreen error-state title: {joined}"
    );
    assert!(
        joined.contains("code=AUTH_REQUIRED"),
        "stable code visible: {joined}"
    );
    assert!(joined.contains("retries"), "primary-action hint: {joined}");
    assert!(
        frame.cursor.is_none(),
        "the fullscreen state hides the input caret"
    );
    // Esc returns: not a dead end.
    chat.on_key(KeyCode::Esc, KeyModifiers::empty());
    assert!(chat.last_error.is_none(), "Esc back clears the error state");
}

/// #18 page-level error-row highlight: inject a Page-level fixture → the `[error]` row uses the error color
/// (A zone; theme.error = (255,107,128) color baseline).
#[test]
fn page_error_row_is_highlighted_with_error_color() {
    use crate::error::ErrorLevel;
    use crate::tui::app::Frame;
    use crate::tui::test_util::{ErrorContext, error_fixtures};
    use ratatui::layout::Size;
    use ratatui::style::Color;
    let mut chat = test_chat();
    let fx = error_fixtures()
        .into_iter()
        .find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::ShortSync)
        .expect("FX-01 is in the fixture list");
    fx.inject(&chat.events);
    chat.drain_events();
    assert_eq!(chat.last_error.as_ref().unwrap().level, ErrorLevel::Page);
    let frame = Frame::assemble(&chat, Size::new(80, 24));
    let error_row = frame
        .rows
        .iter()
        .find(|r| r.line.plain_text().starts_with("[error]"))
        .expect("the error row exists");
    assert!(
        error_row
            .line
            .segs
            .iter()
            .any(|s| s.style.fg == Some(Color::Rgb(255, 107, 128))),
        "the error row highlights with the error color (255,107,128): {:?}",
        error_row.line.segs
    );
}

/// #18 full-screen state: Enter retries the last input (AC-15/53 retry-path skeleton).
#[tokio::test]
async fn full_error_enter_retries_last_prompt() {
    use crate::error::ErrorLevel;
    use crate::tui::test_util::error_fixtures;
    use crossterm::event::{KeyCode, KeyModifiers};
    let mut chat = test_chat();
    chat.last_prompt = "why is the sky blue".into();
    let fx = error_fixtures()
        .into_iter()
        .find(|f| f.code == "PERMISSION_DENIED")
        .expect("FX-05 is in the fixture list");
    fx.inject(&chat.events);
    chat.drain_events();
    assert_eq!(chat.last_error.as_ref().unwrap().level, ErrorLevel::Full);
    chat.on_key(KeyCode::Enter, KeyModifiers::empty());
    assert!(chat.last_error.is_none(), "Enter clears the error state");
    assert!(chat.busy, "Enter retries and starts a new turn");
}

// ---- QA assertion side (delivery 3/3): AC-53 / AC-29 / presentation styling ----

/// AC-53 long-turn failure escalates: FX-11 (TIMEOUT + LongTurn) → full-flow full-screen state,
/// versus FX-01 (TIMEOUT + ShortSync, page-level): **same code, different level**, distinguished by context.
/// The full-screen state shows the stable code + retry/back paths (AC-53 F3) and hides the caret.
#[test]
fn qa_ac53_long_turn_timeout_escalates_to_full_screen() {
    use crate::error::ErrorContext;
    use crate::error::ErrorLevel;
    use crate::tui::app::Frame;
    use crate::tui::test_util::error_fixtures;
    use ratatui::layout::Size;
    // Long-turn transport timeout → full-flow level.
    let mut chat = test_chat();
    let fx = error_fixtures()
        .into_iter()
        .find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::LongTurn)
        .expect("FX-11 is in the fixture list");
    fx.inject(&chat.events);
    chat.drain_events();
    let err = chat
        .last_error
        .as_ref()
        .expect("the error state was recorded");
    assert_eq!(err.code, "TIMEOUT");
    assert_eq!(
        err.level,
        ErrorLevel::Full,
        "a long-turn TIMEOUT escalates to the full-flow level (AC-53)"
    );
    let frame = Frame::assemble(&chat, Size::new(80, 24));
    let joined: String = frame
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("code=TIMEOUT"),
        "the fullscreen state carries the stable code: {joined}"
    );
    assert!(
        joined.contains("retry") || joined.contains("back"),
        "AC-53 includes the \"retry or back\" path: {joined}"
    );
    assert!(
        frame.cursor.is_none(),
        "the fullscreen state hides the input caret"
    );
    // Same code, short sync (FX-01) → page-level error row, not full-screen — the two TIMEOUT levels are told apart by context.
    let mut short = test_chat();
    let fx_short = error_fixtures()
        .into_iter()
        .find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::ShortSync)
        .expect("FX-01 is in the fixture list");
    fx_short.inject(&short.events);
    short.drain_events();
    let frame_short = Frame::assemble(&short, Size::new(80, 24));
    let joined_short: String = frame_short
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined_short.contains("[error] code=TIMEOUT"),
        "short-sync TIMEOUT = a page-level error row: {joined_short}"
    );
    assert!(
        !joined_short.contains("something went wrong"),
        "short-sync does not go fullscreen: {joined_short}"
    );
}

/// AC-29 per-code matrix: inject all 11 fixtures from error_fixtures(), asserting that
/// "the level is carried explicitly by the producer and the render shape matches it" — Full → full screen,
/// Page/Field → error row. The assertion anchor is the stable code, never the msg text.
#[test]
fn qa_ac29_fixture_matrix_renders_by_level() {
    use crate::error::ErrorLevel;
    use crate::tui::app::Frame;
    use crate::tui::test_util::error_fixtures;
    use ratatui::layout::Size;
    for fx in error_fixtures() {
        let mut chat = test_chat();
        fx.inject(&chat.events);
        chat.drain_events();
        let err = chat
            .last_error
            .as_ref()
            .expect("the error state was recorded");
        assert_eq!(err.code, fx.code, "error code recorded: {}", fx.code);
        assert_eq!(
            err.level, fx.level,
            "the level is carried explicitly by the producer (no copied mapping table): {}",
            fx.code
        );
        let frame = Frame::assemble(&chat, Size::new(80, 24));
        let joined: String = frame
            .rows
            .iter()
            .map(|r| r.line.plain_text())
            .collect::<Vec<_>>()
            .join("\n");
        match fx.level {
            ErrorLevel::Full => {
                assert!(
                    joined.contains("something went wrong"),
                    "full-flow fullscreen-state title: {} / {joined}",
                    fx.code
                );
                assert!(
                    joined.contains(&format!("code={}", fx.code)),
                    "the fullscreen state carries the stable code: {} / {joined}",
                    fx.code
                );
                assert!(
                    frame.cursor.is_none(),
                    "the fullscreen state hides the caret: {}",
                    fx.code
                );
            }
            ErrorLevel::Page | ErrorLevel::Field => {
                assert!(
                    joined.contains(&format!("[error] code={}", fx.code)),
                    "page/field-level error rows carry the stable code: {} / {joined}",
                    fx.code
                );
                assert!(
                    !joined.contains("something went wrong"),
                    "page/field-level does not go fullscreen: {} / {joined}",
                    fx.code
                );
            }
        }
    }
}

/// Presentation styling (A zone): after the page-level error row renders into the Buffer via `render_rows`,
/// **the real cells use the error color (255,107,128)** (not just at the SegStyle layer) — asserting that
/// the "highlight the user sees" lands on the final picture, anchored in both style and text.
#[test]
fn qa_page_error_row_paints_error_color_in_buffer() {
    use crate::error::ErrorContext;
    use crate::tui::app::Frame;
    use crate::tui::test_util::error_fixtures;
    use ratatui::buffer::Buffer;
    use ratatui::layout::{Rect, Size};
    use ratatui::style::Color;
    let mut chat = test_chat();
    let fx = error_fixtures()
        .into_iter()
        .find(|f| f.code == "TIMEOUT" && f.context == ErrorContext::ShortSync)
        .expect("FX-01 is in the fixture list");
    fx.inject(&chat.events);
    chat.drain_events();
    let frame = Frame::assemble(&chat, Size::new(80, 24));
    let mut buf = Buffer::empty(Rect::new(0, 0, 80, 24));
    let area = buf.area;
    crate::tui::view::render_rows(&frame.rows, Color::White, &mut buf, area);
    let err_color = Color::Rgb(255, 107, 128);
    let has_err_color =
        (0..buf.area.height).any(|y| (0..buf.area.width).any(|x| buf[(x, y)].fg == err_color));
    assert!(
        has_err_color,
        "the error row really renders the error color (255,107,128) into the cell"
    );
    // Text anchor (assertions only anchor on the code).
    let joined: String = frame
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("[error] code=TIMEOUT"),
        "the error-row text carries the stable code: {joined}"
    );
}

/// FX-01 **real-path** assertion (main #91 / dev #92 invite): the `/model` level-two menu
/// fetch (`open_model_models`, the production emission source) emits
/// `UiEvent::Error { level: Page, context: ShortSync }` when list_models times out (10s) — no fixture
/// injection, verifying the **production trigger source** wiring (AC-12/13/14 page-level contracts have a real landing).
/// Degraded behavior is preserved: the error row is visible, non-full-screen, non-blocking.
#[tokio::test(start_paused = true)]
async fn qa_fx01_real_path_model_menu_failure_emits_page_error() {
    use crate::api::client::test_hooks;
    use crate::error::ErrorContext;
    use crate::error::ErrorLevel;
    use crate::tui::app::Frame;
    use ratatui::layout::Size;
    let _guard = test_hooks::hang_guard(60_000); // hangs list_models for 60s, > the 10s read timeout
    let mut chat = test_chat();
    // Exercise the real production fetch path (fork "default" — unknown names now error honestly instead of
    // silently falling back to the current endpoint, so that is no longer this test's path).
    chat.open_model_models(
        "default".into(),
        vec!["default".into()],
        vec![String::new()],
        0,
    );
    // Let the spawned task start and register its timeout timer first (under start_paused it only advances when polled).
    tokio::task::yield_now().await;
    // The 10s read timeout fires → emits UiEvent::Error (page-level).
    tokio::time::advance(std::time::Duration::from_secs(11)).await;
    tokio::task::yield_now().await; // let the spawned task finish sending the event
    chat.drain_events();
    let err = chat
        .last_error
        .as_ref()
        .expect("the production emitter recorded the error state");
    assert_eq!(
        err.code, "TIMEOUT",
        "a list_models read timeout lands on TIMEOUT"
    );
    assert_eq!(
        err.level,
        ErrorLevel::Page,
        "short-sync = page level (the real path)"
    );
    assert_eq!(err.context, ErrorContext::ShortSync, "context = short-sync");
    // Render: the page-level error row is visible, non-full-screen (degraded behavior preserved).
    let frame = Frame::assemble(&chat, Size::new(80, 24));
    let joined: String = frame
        .rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("[error] code=TIMEOUT"),
        "the real-path error row is visible: {joined}"
    );
    assert!(
        !joined.contains("something went wrong"),
        "page level does not go fullscreen: {joined}"
    );
}
/// Info tier: /help output persists (no TTL) until the next input or Esc
/// — the old 2s TTL burned it before anyone could read.
#[test]
fn info_output_persists_until_input_or_escape() {
    let mut chat = test_chat();
    chat.input = "/help".to_string();
    chat.submit();
    assert!(
        !chat.slash_info_lines.is_empty(),
        "/help lands in the info bucket"
    );
    chat.tick();
    assert!(
        !chat.slash_info_lines.is_empty(),
        "the tick does not clear info (no TTL)"
    );
    // Typing clears it (read then act).
    chat.on_key(KeyCode::Char('h'), KeyModifiers::empty());
    assert!(chat.slash_info_lines.is_empty(), "input clears info");

    chat.input = "/help".to_string();
    chat.submit();
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(chat.slash_info_lines.is_empty(), "Esc clears info");
}

/// Pinned panels survive ticks and render in the chrome until unpinned.
#[test]
fn pinned_panel_lives_until_unpinned() {
    let mut chat = test_chat();
    chat.pin_panel(
        "login",
        vec![
            "sign in to codex (device authorization)".to_string(),
            "  enter code ABCD-EFGH".to_string(),
        ],
    );
    chat.tick();
    assert_eq!(
        chat.pinned_panels.len(),
        1,
        "the tick does not clear pinned"
    );
    let rows = crate::tui::el::render(crate::tui::chrome::chrome(&chat, 80, false)).rows;
    let joined: String = rows
        .iter()
        .map(|r| r.line.plain_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        joined.contains("ABCD-EFGH"),
        "the panel is visible: {joined}"
    );
    chat.handle(UiEvent::Unpin {
        id: "login".to_string(),
    });
    assert!(chat.pinned_panels.is_empty(), "unpin makes it disappear");
}

/// Batch-2 invariant: switching providers resolves the model atomically —
/// last-used per provider wins, the provider default fills in, and
/// switching back restores what you used there.
#[test]
fn switch_provider_resolves_model_atomically() {
    let mut chat = test_chat();
    let mut settings = crate::settings::Settings {
        api_key: Some("sk-main".into()),
        ..Default::default()
    };
    settings.providers.insert(
        "deepseek".to_string(),
        crate::settings::ProviderConfig {
            api_key: Some("sk-ds".into()),
            api_base_url: "https://api.deepseek.com".into(),
            supports_images: None,
            protocol: None,
            oauth: None,
        },
    );
    Arc::get_mut(&mut chat.session).unwrap().client =
        crate::api::client::Client::from_settings_with(&settings, |_| {
            Err(std::env::VarError::NotPresent)
        })
        .unwrap();
    let _ = chat.session.runtime.model_tx.send("claude-sonnet-5".into());

    // An anthropic-protocol endpoint: default model fallback (claude-sonnet-5 is already in use → unchanged).
    chat.input = "/provider deepseek".to_string();
    chat.submit();
    assert_eq!(*chat.session.runtime.provider.borrow(), "deepseek");
    // Switch models mid-session → away and back restores the model that endpoint last used.
    chat.input = "/model deepseek-v4".to_string();
    chat.submit();
    chat.input = "/provider default".to_string();
    chat.submit();
    assert_eq!(
        *chat.session.runtime.model.borrow(),
        "claude-sonnet-5",
        "back to default restores its last model"
    );
    chat.input = "/provider deepseek".to_string();
    chat.submit();
    assert_eq!(
        *chat.session.runtime.model.borrow(),
        "deepseek-v4",
        "back to deepseek restores its last model"
    );
}

/// Mid-turn provider switches are refused: a cross-protocol swap would
/// send this conversation's thinking blocks to the wrong endpoint.
#[test]
fn switch_provider_refuses_while_busy() {
    let mut chat = test_chat();
    chat.busy = true;
    chat.input = "/provider codex".to_string();
    chat.submit();
    assert_eq!(
        *chat.session.runtime.provider.borrow(),
        "default",
        "not switched"
    );
    assert!(
        chat.slash_error_lines.join("\n").contains("BUSY"),
        "{:?}",
        chat.slash_error_lines
    );
}

/// P0-9 regression: modifier chords in the Other input stay chords —
/// ctrl+c must reach the global interrupt, not become a literal letter.
#[test]
fn ask_other_input_does_not_swallow_modifier_chords() {
    let mut chat = test_chat();
    let (tx, _rx) = tokio::sync::oneshot::channel();
    chat.pending_ask = Some((
        crate::ui::PermissionRequest {
            title: "choose".into(),
            question: "pick one".into(),
            options: vec!["A".into()],
            descriptions: vec![None],
            free_text: true,
        },
        tx,
    ));
    chat.ask_focus = 1; // the Other input slot
    assert!(chat.ask_key(KeyCode::Char('h'), KeyModifiers::empty()));
    assert_eq!(chat.ask_other, "h");
    assert!(
        !chat.ask_key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        "ctrl+c is not swallowed by the dialog"
    );
    assert_eq!(
        chat.ask_other, "h",
        "modified keys do not leak into the input"
    );
}

/// P0-7 regression: a short-sync (Page-level) failure must not end the
/// running turn — busy/stream stay untouched; only LongTurn errors reset.
#[test]
fn short_sync_error_keeps_the_running_turn() {
    let mut chat = test_chat();
    chat.busy = true;
    chat.stream_msg = Some(0);
    chat.handle(UiEvent::Error {
        code: "TIMEOUT",
        msg: "list_models timeout".into(),
        level: crate::error::ErrorLevel::Page,
        context: crate::error::ErrorContext::ShortSync,
    });
    assert!(
        chat.busy,
        "a short-sync failure does not interrupt the turn"
    );
    assert_eq!(chat.stream_msg, Some(0));
    assert!(chat.last_error.is_some(), "the error row is still recorded");

    chat.handle(UiEvent::Error {
        code: "TIMEOUT",
        msg: "turn died".into(),
        level: crate::error::ErrorLevel::Full,
        context: crate::error::ErrorContext::LongTurn,
    });
    assert!(!chat.busy, "a turn-level failure resets as usual");
}

/// Page/Field error rows dismiss with Esc instead of squatting above the
/// prompt until the next turn.
#[test]
fn escape_dismisses_page_level_errors() {
    let mut chat = test_chat();
    chat.handle(UiEvent::Error {
        code: "TIMEOUT",
        msg: "x".into(),
        level: crate::error::ErrorLevel::Page,
        context: crate::error::ErrorContext::ShortSync,
    });
    assert!(chat.on_key(KeyCode::Esc, KeyModifiers::empty()));
    assert!(chat.last_error.is_none(), "Esc clears a page-level error");
}

/// `/theme` with junk reports BAD_ARGUMENT instead of silently switching
/// to auto with a success message (slash-command-ux G13).
#[test]
fn slash_theme_rejects_unknown_names() {
    let mut chat = test_chat();
    chat.input = "/theme bogus".to_string();
    chat.submit();
    let joined = all_slash_text(&chat);
    assert!(joined.contains("BAD_ARGUMENT"), "{joined}");
    assert!(!joined.contains("✓"), "no success receipt shown: {joined}");
}

/// P0-16 regression: a bash-mode turn clears interrupt suppression the
/// same way a model turn does.
#[tokio::test]
async fn bash_turn_resets_interrupted() {
    let mut chat = test_chat();
    chat.interrupted = true;
    chat.bash_mode = true;
    chat.set_input("echo hi");
    chat.submit();
    assert!(
        !chat.interrupted,
        "! suppresses the interrupt on turn reset"
    );
}

/// The bottom notice expires with its window: an expired "press again"
/// promise disappears instead of lying.
#[test]
fn notice_expires_after_its_window() {
    let mut chat = test_chat();
    chat.notice = Some("Press ctrl-c again to exit");
    chat.notice_until = Some(std::time::Instant::now() - std::time::Duration::from_millis(1));
    assert!(
        chat.needs_tick(),
        "with an expiring notice, idling is not allowed"
    );
    chat.tick();
    assert!(chat.notice.is_none(), "cleared once expired");
    assert!(chat.notice_until.is_none());
}

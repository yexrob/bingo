//! Chat state-machine tests, part three: the collapse fold's own contract.
//!
//! `chat_tests_a` / `chat_tests_b` split by size alone (the 4000-line file cap); this
//! file continues them.

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

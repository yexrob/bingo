//! A task list is the session's own state (ADR-0011 §2): the tools write it
//! into the journal, the frames say so on the wire, and a second run that
//! continues the session reads back what the first one wrote.

use super::*;

/// One run: create a task, list the tasks, say something. The fake provider
/// hands its responses out in order.
const CREATE_THEN_LIST: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"TaskCreate","input":{"subject":"write the plan"}}}]},
    {"steps":[{"toolCall":{"name":"TaskList","input":{}}}]},
    {"steps":[{"text":"listed"}]}
]}"#;

const LIST_ONLY: &str = r#"{"responses":[
    {"steps":[{"toolCall":{"name":"TaskList","input":{}}}]},
    {"steps":[{"text":"still there"}]}
]}"#;

/// The text of the last completed call of `name`, as the model read it.
fn tool_result(out: &Output, name: &str) -> String {
    let output = frames_of(out)
        .into_iter()
        .filter_map(|f| match f.event {
            Event::ItemCompleted { item } => match item.body {
                bingo_sdk::ItemBody::ToolCall {
                    name: called,
                    output,
                    ..
                } if called == name => output,
                _ => None,
            },
            _ => None,
        })
        .next_back()
        .unwrap_or_else(|| panic!("the {name} call completed"));
    assert!(!output.is_error, "{name}: {output:?}");
    output
        .parts
        .iter()
        .filter_map(bingo_sdk::ContentPart::as_text)
        .collect()
}

#[test]
fn the_journal_carries_the_list_from_one_run_to_the_next() {
    let home = tempfile::tempdir().unwrap();

    let first = scripted_run(
        home.path(),
        &script(CREATE_THEN_LIST),
        &[],
        "note the plan and list what there is",
    );
    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    let listed = tool_result(&first, "TaskList");
    assert!(listed.contains("#1"), "{listed}");
    assert!(listed.contains("write the plan"), "{listed}");
    assert!(
        frames_of(&first).iter().any(|f| matches!(
            &f.event,
            Event::Extension { plugin, kind, .. } if plugin == "bingo.tasks" && kind == "tasks"
        )),
        "the list was published into the journal"
    );
    assert!(
        !stderr(&first).contains("[notice]"),
        "the prompt block read the journal on every round: {}",
        stderr(&first)
    );

    let second = scripted_run(
        home.path(),
        &script(LIST_ONLY),
        &["--continue"],
        "what is on the list?",
    );
    assert_eq!(second.status.code(), Some(0), "stderr: {}", stderr(&second));
    assert_eq!(
        frames_of(&second)[0].session,
        frames_of(&first)[0].session,
        "the same session"
    );
    let again = tool_result(&second, "TaskList");
    assert!(again.contains("#1"), "{again}");
    assert!(again.contains("write the plan"), "{again}");
}

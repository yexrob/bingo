//! A step that holds more than one call. The executor runs consecutive
//! concurrency-safe allowed calls as one batch; what a host sees off the wire
//! must not depend on that.

use super::*;

/// Two commands the model asked for in one step: the executor runs them as
/// one batch, and what a host sees is unchanged by that — both receipts land,
/// in the order they were asked for, on one round of one turn.
#[test]
fn two_commands_in_one_step_both_come_back() {
    let home = tempfile::tempdir().unwrap();
    let script = script(
        r#"{"responses":[
            {"steps":[
                {"toolCall":{"name":"Bash","input":{"command":"echo first"}}},
                {"toolCall":{"name":"Bash","input":{"command":"echo second"}}}
            ]},
            {"steps":[{"text":"Both ran."}]}
        ]}"#,
    );
    let out = scripted_run(
        home.path(),
        &script,
        &["--dangerously-skip-permissions"],
        "run both",
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let frames = frames_of(&out);
    assert!(
        matches!(
            frames.last().map(|f| &f.event),
            Some(Event::TurnCompleted {
                status: TurnStatus::Completed,
                ..
            })
        ),
        "{}",
        stdout(&out)
    );
    let calls: Vec<(u32, String)> = frames
        .iter()
        .filter_map(|f| match &f.event {
            Event::ItemCompleted { item } => match &item.body {
                bingo_sdk::ItemBody::ToolCall {
                    name,
                    output: Some(output),
                    ..
                } if name == "Bash" => Some((
                    item.round,
                    output
                        .parts
                        .iter()
                        .filter_map(bingo_sdk::ContentPart::as_text)
                        .collect(),
                )),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(calls.len(), 2, "both receipts landed: {}", stdout(&out));
    assert_eq!(calls[0].0, calls[1].0, "one step, not two rounds");
    assert!(calls[0].1.contains("first"), "{calls:?}");
    assert!(calls[1].1.contains("second"), "{calls:?}");
}

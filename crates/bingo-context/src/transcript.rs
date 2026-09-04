//! The journal as prose, for a model that is asked about it rather than
//! continued from it: the summary request and the memory extractor read the
//! same rendering.

use bingo_sdk::{ContentPart, Item, ItemBody, ToolOutput};
use serde_json::Value;

/// Longest tool input echoed. Enough to identify the command or the path that
/// was acted on; short enough that one file write is not the whole prompt.
const INPUT_CHARS: usize = 200;

/// Longest tool result echoed. A result can be produced again by running the
/// tool; prose cannot, so only results are cut here.
const RESULT_CHARS: usize = 2_000;

/// One line per item, oldest first. Reasoning never comes back — a model's
/// notes to itself are not evidence about the work — and items with no wire
/// form (notices, receipts, rewinds) say nothing about it either.
pub fn lines(items: &[Item]) -> Vec<String> {
    items.iter().filter_map(line).collect()
}

fn line(item: &Item) -> Option<String> {
    match &item.body {
        ItemBody::User { parts, .. } => labelled("user", &text_of(parts)),
        ItemBody::Assistant { text } => labelled("assistant", text),
        ItemBody::ToolCall {
            name,
            input,
            output,
            ..
        } => Some(call(name, input, output.as_ref())),
        ItemBody::Shell {
            command,
            output,
            exit,
            ..
        } => Some(shell(command, output, *exit)),
        ItemBody::Compaction { summary, .. } => labelled("earlier summary", summary),
        _ => None,
    }
}

fn labelled(label: &str, text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| format!("{label}: {text}"))
}

/// The call the model made and what came back. A call with no output is one
/// the turn never finished, and saying so is not the same as saying nothing.
fn call(name: &str, input: &Value, output: Option<&ToolOutput>) -> String {
    let head = format!("tool {name}({})", one_line(&input.to_string(), INPUT_CHARS));
    match output {
        None => format!("{head}\nresult: [the call did not complete]"),
        Some(o) => {
            let label = if o.is_error { "error" } else { "result" };
            let body = one_line(&text_of(&o.parts), RESULT_CHARS);
            format!("{head}\n{label}: {body}")
        }
    }
}

/// A shell line the person ran themselves, and what it wrote. Nothing asked
/// for it, so it is labelled by the prompt it was typed at rather than by a
/// caller; the code comes with the result when it was not a clean exit.
fn shell(command: &str, output: &str, exit: Option<i32>) -> String {
    let mut result = one_line(output.trim_end(), RESULT_CHARS);
    if let Some(code) = exit.filter(|code| *code != 0) {
        if !result.is_empty() {
            result.push(' ');
        }
        result.push_str(&format!("[exit {code}]"));
    }
    format!("shell $ {command}\nresult: {result}")
}

fn text_of(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Flattened and capped: every rendered item is one line, so a budget can be
/// spent a line at a time.
fn one_line(text: &str, limit: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= limit {
        return flat;
    }
    flat.chars().take(limit).chain(['…']).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{assistant, tool, user};
    use bingo_sdk::{Item, ItemBody, ItemId, ItemStatus, Level};
    use jiff::Timestamp;

    #[test]
    fn a_turn_renders_as_who_said_what() {
        let items = vec![
            user("u", "add a test"),
            assistant("a", "reading the file"),
            tool("t", "Read", r#"{"path":"/a.rs"}"#, Some("fn main() {}")),
        ];
        assert_eq!(
            lines(&items),
            [
                "user: add a test",
                "assistant: reading the file",
                "tool Read({\"path\":\"/a.rs\"})\nresult: fn main() {}",
            ]
        );
    }

    #[test]
    fn reasoning_and_notices_never_reach_the_prompt() {
        let items = vec![
            reasoning("r", "the user probably wants"),
            notice("n", "context is nearly spent"),
        ];
        assert!(lines(&items).is_empty());
    }

    #[test]
    fn a_long_input_and_a_long_result_are_cut_to_their_caps() {
        let input = format!(r#"{{"text":"{}"}}"#, "x".repeat(500));
        let call = tool("t", "Write", &input, Some(&"y".repeat(5_000)));
        let rendered = lines(&[call]).concat();
        let (head, result) = rendered.split_once('\n').expect("a call and its result");
        assert_eq!(head.chars().count(), "tool Write()".len() + INPUT_CHARS + 1);
        assert_eq!(result.chars().count(), "result: ".len() + RESULT_CHARS + 1);
    }

    #[test]
    fn a_call_that_never_completed_says_so() {
        let call = tool("t", "Bash", r#"{"command":"ls"}"#, None);
        assert!(
            lines(&[call])
                .concat()
                .ends_with("[the call did not complete]")
        );
    }

    /// A line the person ran themselves reads back as what they typed and
    /// what came of it (M65).
    #[test]
    fn a_shell_line_renders_as_the_prompt_and_its_output() {
        let ran = |command: &str, output: &str, exit| {
            at(
                "s",
                ItemBody::Shell {
                    command: command.into(),
                    output: output.into(),
                    exit,
                    cwd: "/tmp/p".into(),
                },
            )
        };
        assert_eq!(
            lines(&[
                ran("echo hi", "hi\n", Some(0)),
                ran("false", "", Some(1)),
                ran("cat a b", "a\nno such file\n", Some(1)),
            ]),
            [
                "shell $ echo hi\nresult: hi",
                "shell $ false\nresult: [exit 1]",
                "shell $ cat a b\nresult: a no such file [exit 1]",
            ]
        );
    }

    #[test]
    fn an_empty_message_is_no_line_at_all() {
        assert!(lines(&[user("u", "   ")]).is_empty());
    }

    fn at(id: &str, body: ItemBody) -> Item {
        Item {
            id: ItemId::from_raw(id),
            turn: None,
            round: 0,
            status: ItemStatus::Completed,
            started_at: Timestamp::UNIX_EPOCH,
            completed_at: None,
            intent: None,
            body,
            meta: serde_json::Map::new(),
        }
    }

    fn reasoning(id: &str, text: &str) -> Item {
        at(
            id,
            ItemBody::Reasoning {
                text: text.into(),
                provider_metadata: Default::default(),
            },
        )
    }

    fn notice(id: &str, text: &str) -> Item {
        at(
            id,
            ItemBody::Notice {
                level: Level::Warn,
                code: "X".into(),
                text: text.into(),
            },
        )
    }
}

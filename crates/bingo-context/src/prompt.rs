//! The request that buys a summary.

use bingo_sdk::{Item, Message, ModelRequest, ProviderMetadata, Role, SystemBlock};

use crate::{estimate, tail, transcript};

/// What the summary may cost. The old ceiling of 1,024 was the real limit on
/// summary quality: no instruction puts a long session's decisions, paths and
/// pending work into that.
const MAX_TOKENS: u32 = 4_096;

/// Slack over the request's own budget: the estimate is an estimate, and being
/// a little under costs nothing next to a summary request that overflows on
/// the way out of an overflow.
const RESERVE: u64 = 2_000;

const COMPACT: &str = "\
You are compacting an agent conversation. Your summary replaces the transcript below, and it is \
all the agent will have of that work — anything you leave out is lost. Write it under these \
headings, skipping any heading with nothing to report:

## Task and current state
What the user asked for, and exactly where the work stands now.

## Decisions and rationale
Choices that were made and why, including approaches that were tried and rejected.

## Files, commands and results
Files read or changed, with their paths. Commands that were executed and what they returned.

## Outstanding work
What is not done yet, in the order it should be tackled.

## Constraints and preferences
Rules, conventions and user preferences that still apply.

Reproduce identifiers, paths, commands and error text exactly; never invent anything the \
transcript does not contain. Let the length follow the content — usually several hundred to a \
thousand words.";

/// The summary request: the headings as the system prompt, the transcript as
/// one user message, trimmed from its oldest line until the whole thing sits
/// `RESERVE` tokens under the window it has to fit through.
pub fn request(model: &str, window: u64, instructions: Option<&str>, old: &[Item]) -> ModelRequest {
    let system = system(instructions);
    let budget = window
        .saturating_sub(RESERVE)
        .saturating_sub(estimate::blocks(&system));
    ModelRequest {
        model: model.to_string(),
        max_tokens: MAX_TOKENS,
        system,
        messages: vec![Message::text(Role::User, body(old, budget))],
        tools: Vec::new(),
        reasoning: None,
        // A side question, not the session's turn (ADR-0035 §3).
        session: None,
        provider_options: ProviderMetadata::new(),
    }
}

/// A manual compaction says what the person wants kept, after the headings so
/// it reads as an amendment to them.
fn system(instructions: Option<&str>) -> Vec<SystemBlock> {
    let text = match instructions.map(str::trim).filter(|i| !i.is_empty()) {
        Some(extra) => format!("{COMPACT}\n\n{extra}"),
        None => COMPACT.to_string(),
    };
    vec![SystemBlock { text, cache: false }]
}

fn body(old: &[Item], budget: u64) -> String {
    let lines = transcript::lines(old);
    // The note is charged from the start, so dropping cannot undershoot and
    // need a second pass; over-reserving by one absent line is free.
    let budget = budget.saturating_sub(estimate::text(&omitted(lines.len())));
    let dropped = tail::first_within(&lines, budget, |l| estimate::text(l) + 1);
    let mut out = String::new();
    if dropped > 0 {
        out.push_str(&omitted(dropped));
        out.push('\n');
    }
    out.push_str(&lines[dropped..].join("\n"));
    out
}

fn omitted(lines: usize) -> String {
    format!("({lines} earlier lines are left out: they did not fit this request.)")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{assistant, tool, user};
    use serde_json::json;

    fn journal() -> Vec<Item> {
        vec![
            user("u1", "add a compaction test"),
            assistant("a1", "reading the compactor"),
            tool(
                "t1",
                "Read",
                r#"{"path":"crates/bingo-context/src/compact.rs"}"#,
                Some("pub struct SummaryCompactor;"),
            ),
            assistant("a2", "the split is the interesting part"),
            user("u2", "keep the tool pairs together"),
        ]
    }

    fn tokens(request: &ModelRequest) -> u64 {
        estimate::blocks(&request.system)
            + request
                .messages
                .iter()
                .map(|m| {
                    m.parts
                        .iter()
                        .filter_map(|p| p.as_text())
                        .map(estimate::text)
                        .sum::<u64>()
                })
                .sum::<u64>()
    }

    #[test]
    fn the_summary_request_is_the_headings_and_the_transcript() {
        let request = request("model-x", 200_000, None, &journal());
        assert_eq!(request.max_tokens, MAX_TOKENS);
        assert!(request.reasoning.is_none());
        assert!(request.tools.is_empty());
        assert!(request.provider_options.is_empty());
        insta::assert_json_snapshot!(json!({
            "system": request.system,
            "messages": request.messages,
        }));
    }

    #[test]
    fn manual_instructions_amend_the_headings() {
        let request = request("model-x", 200_000, Some("keep the SQL"), &journal());
        let system = &request.system[0].text;
        assert!(system.starts_with("You are compacting"));
        assert!(system.ends_with("\n\nkeep the SQL"));
    }

    #[test]
    fn blank_instructions_amend_nothing() {
        let request = request("model-x", 200_000, Some("  "), &journal());
        assert_eq!(request.system[0].text, COMPACT);
    }

    #[test]
    fn a_transcript_too_large_for_the_window_loses_its_oldest_lines() {
        let mut items = journal();
        for i in 0..200 {
            items.insert(0, user(&format!("old{i}"), &"x".repeat(400)));
        }
        let window = 4_000;
        let request = request("model-x", window, None, &items);
        assert!(
            tokens(&request) <= window - RESERVE,
            "{} tokens against a {window} window",
            tokens(&request)
        );
        let body = request.messages[0].parts[0].as_text().unwrap_or_default();
        assert!(
            body.starts_with("("),
            "the cut says what it left out: {body:.60}"
        );
        assert!(
            body.contains("keep the tool pairs together"),
            "the newest line stays"
        );
    }
}

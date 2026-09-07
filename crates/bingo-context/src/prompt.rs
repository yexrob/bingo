//! A summary is a side question on the parent's exact prefix.

use bingo_sdk::{ContentPart, Message, ModelCapabilities, ModelRequest, Role, tokens};

const MAX_TOKENS: u32 = 4_096;
const RESERVE: u64 = 256;
pub const OMITTED: &str = "[Some earlier conversation was omitted from the summary input after a context overflow; this summary may be incomplete.]";
const COMPACT: &str = "\
You are compacting the agent conversation above. Do not call tools. Output only a summary.
The summary replaces the older turns; the newest turns will also remain verbatim, so overlap
is intentional. Anything you leave out of the older work is lost. Write under these headings,
skipping any heading with nothing to report:

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

Reproduce identifiers, paths, commands and error text exactly; never invent anything the
conversation does not contain. Let the length follow the content — usually several hundred
to a thousand words.";

/// Preserve system, tools, session/cache identity, reasoning and provider options.
/// `window` is the full model window, not the core's input-only usage window.
pub fn request(
    parent: &ModelRequest,
    capabilities: &ModelCapabilities,
    instructions: Option<&str>,
) -> Option<ModelRequest> {
    let mut request = parent.clone();
    let instruction = match instructions.map(str::trim).filter(|s| !s.is_empty()) {
        Some(extra) => format!("{COMPACT}\n\n{extra}"),
        None => COMPACT.to_string(),
    };
    request
        .messages
        .push(Message::text(Role::User, instruction));
    request
        .provider_options
        .entry("bingo".into())
        .or_default()
        .insert("purpose".into(), "compaction".into());
    let input = tokens::estimate(&request.system, &request.messages, &request.tools);
    let headroom = capabilities
        .context_window
        .saturating_sub(input)
        // Reserve the possible retry's omission note before any history is cut.
        .saturating_sub(RESERVE + tokens::text(OMITTED));
    request.max_tokens = u64::from(MAX_TOKENS)
        .min(capabilities.max_output)
        .min(headroom) as u32;
    (request.max_tokens > 0).then_some(request)
}

/// Only after an actual overflow: drop a whole oldest exchange, never split
/// tool calls from results or rewrite signed reasoning inside a message.
pub fn shrink(request: &mut ModelRequest) -> bool {
    let end = request.messages.len().saturating_sub(1);
    let boundary = (1..end).find(|&at| {
        request.messages[at].role == Role::User
            && !request.messages[at]
                .parts
                .iter()
                .any(|p| matches!(p, ContentPart::ToolResult { .. }))
            && closed(&request.messages[..at])
    });
    let Some(at) = boundary else {
        return false;
    };
    request.messages.drain(..at);
    if let Some(instruction) = request.messages.last_mut() {
        instruction.parts.push(ContentPart::text(OMITTED));
    }
    true
}

fn closed(messages: &[Message]) -> bool {
    let mut pending = std::collections::BTreeSet::new();
    for part in messages.iter().flat_map(|m| &m.parts) {
        match part {
            ContentPart::ToolUse { id, .. } => {
                pending.insert(id);
            }
            ContentPart::ToolResult { tool_use_id, .. } => {
                pending.remove(tool_use_id);
            }
            _ => {}
        }
    }
    pending.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{Effort, SessionId, SystemBlock};

    pub fn parent() -> ModelRequest {
        ModelRequest {
            model: "model-x".into(),
            max_tokens: 8_000,
            system: vec![SystemBlock {
                text: "stable system".into(),
                cache: true,
            }],
            messages: vec![
                Message::text(Role::User, "old"),
                Message::text(Role::Assistant, "answer"),
                Message::text(Role::User, "new"),
            ],
            tools: vec![bingo_sdk::ToolSpec {
                name: "Read".into(),
                description: "Read source".into(),
                input_schema: serde_json::json!({"type":"object"}),
                meta: Default::default(),
            }],
            reasoning: Some(Effort::High),
            session: Some(SessionId::from_raw("ses_parent")),
            provider_options: serde_json::from_value(
                serde_json::json!({"openai":{"seed":7}, "bingo":{"other":"preserved"}}),
            )
            .unwrap(),
        }
    }

    #[test]
    fn the_parent_prefix_is_unchanged_and_only_the_instruction_is_appended() {
        let parent = parent();
        let caps = ModelCapabilities {
            context_window: 20_000,
            max_output: 8_000,
            images: false,
            reasoning: true,
            count_tokens: false,
            caching: true,
        };
        let summary = request(&parent, &caps, Some("keep SQL")).expect("headroom");
        assert_eq!(summary.system, parent.system);
        assert_eq!(summary.tools, parent.tools);
        assert_eq!(summary.reasoning, parent.reasoning);
        assert_eq!(summary.session, parent.session);
        assert_eq!(&summary.messages[..parent.messages.len()], &parent.messages);
        assert!(
            summary.messages.last().unwrap().parts[0]
                .as_text()
                .unwrap()
                .ends_with("keep SQL")
        );
        assert_eq!(summary.provider_options["bingo"]["purpose"], "compaction");
        assert_eq!(summary.provider_options["bingo"]["other"], "preserved");
        assert_eq!(
            summary.provider_options["openai"],
            parent.provider_options["openai"]
        );
        assert!(
            tokens::estimate(&summary.system, &summary.messages, &summary.tools)
                + u64::from(summary.max_tokens)
                + RESERVE
                <= caps.context_window
        );
    }

    #[test]
    fn overflow_never_leaves_an_orphaned_tool_result() {
        let mut request = parent();
        request.messages = vec![
            Message::text(Role::User, "old task"),
            Message::assistant(vec![ContentPart::ToolUse {
                id: "c".into(),
                name: "Read".into(),
                input: serde_json::json!({}),
            }]),
            Message::user(vec![ContentPart::ToolResult {
                tool_use_id: "c".into(),
                parts: vec![ContentPart::text("result")],
                is_error: false,
            }]),
            Message::text(Role::Assistant, "done"),
            Message::text(Role::User, "next task"),
            Message::text(Role::User, COMPACT),
        ];
        assert!(shrink(&mut request));
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].parts[0].as_text(), Some("next task"));
        assert!(!shrink(&mut request));
    }

    #[test]
    fn a_tiny_window_never_sends_an_oversized_request() {
        let caps = ModelCapabilities {
            context_window: 1,
            max_output: 8_000,
            images: false,
            reasoning: false,
            count_tokens: false,
            caching: false,
        };
        assert!(request(&parent(), &caps, None).is_none());
    }

    #[test]
    fn overflow_shrinks_only_complete_exchanges_and_preserves_purpose() {
        let mut request = parent();
        request.messages.push(Message::text(Role::User, COMPACT));
        request
            .provider_options
            .entry("bingo".into())
            .or_default()
            .insert("purpose".into(), "compaction".into());
        assert!(shrink(&mut request));
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.provider_options["bingo"]["purpose"], "compaction");
        assert!(!shrink(&mut request));
    }
}

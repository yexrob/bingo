//! Microcompact (ADR-0006): stale tool results leave the wire, nothing else
//! changes. A projection over messages — the items and the journal keep
//! every byte.

use bingo_sdk::{ContentPart, Message};

/// `messages` with every tool result older than the last `keep_recent` and
/// longer than `min_chars` replaced by a note of its size; `None` when no
/// result qualifies, so the common case costs a scan and never a clone.
pub fn elide_old_results(
    messages: &[Message],
    keep_recent: usize,
    min_chars: usize,
) -> Option<Vec<Message>> {
    let total = messages.iter().map(count_results).sum::<usize>();
    let stale = total.saturating_sub(keep_recent);
    let mut seen = 0usize;
    let mut changed = false;
    let projected: Vec<Message> = messages
        .iter()
        .map(|m| Message {
            role: m.role,
            parts: m
                .parts
                .iter()
                .map(|p| project(p, &mut seen, stale, min_chars, &mut changed))
                .collect(),
            provider_options: m.provider_options.clone(),
        })
        .collect();
    changed.then_some(projected)
}

fn count_results(message: &Message) -> usize {
    message
        .parts
        .iter()
        .filter(|p| matches!(p, ContentPart::ToolResult { .. }))
        .count()
}

fn project(
    part: &ContentPart,
    seen: &mut usize,
    stale: usize,
    min_chars: usize,
    changed: &mut bool,
) -> ContentPart {
    let ContentPart::ToolResult {
        tool_use_id,
        parts,
        is_error,
    } = part
    else {
        return part.clone();
    };
    let index = *seen;
    *seen += 1;
    let chars = chars_of(parts);
    if index >= stale || chars < min_chars || is_note(parts) {
        return part.clone();
    }
    *changed = true;
    ContentPart::ToolResult {
        tool_use_id: tool_use_id.clone(),
        parts: vec![ContentPart::text(note(chars))],
        is_error: *is_error,
    }
}

fn chars_of(parts: &[ContentPart]) -> usize {
    parts
        .iter()
        .map(|p| match p {
            ContentPart::Text { text } => text.chars().count(),
            ContentPart::Image(image) => image.data.len(),
            ContentPart::ToolResult { parts, .. } => chars_of(parts),
            ContentPart::ToolUse { input, .. } => input.to_string().len(),
            ContentPart::Reasoning { text, .. } => text.chars().count(),
        })
        .sum()
}

/// The note stands in for a result on the wire; the size is what the model
/// gets to know about what it is not seeing.
pub fn note(chars: usize) -> String {
    format!("[tool result elided: {chars} chars]")
}

fn is_note(parts: &[ContentPart]) -> bool {
    matches!(parts, [ContentPart::Text { text }] if text.starts_with("[tool result elided: "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::Role;
    use proptest::prelude::*;

    fn result(id: &str, chars: usize, is_error: bool) -> ContentPart {
        ContentPart::ToolResult {
            tool_use_id: id.into(),
            parts: vec![ContentPart::text("x".repeat(chars))],
            is_error,
        }
    }

    fn results(sizes: &[usize]) -> Vec<Message> {
        sizes
            .iter()
            .enumerate()
            .map(|(n, size)| Message::user(vec![result(&format!("c{n}"), *size, n % 2 == 1)]))
            .collect()
    }

    #[test]
    fn only_old_large_results_are_elided_and_nothing_else_moves() {
        let messages = results(&[5_000, 20, 5_000, 5_000, 5_000]);
        let projected = elide_old_results(&messages, 2, 1_000).expect("changed");
        assert_eq!(
            projected[0].parts,
            vec![ContentPart::ToolResult {
                tool_use_id: "c0".into(),
                parts: vec![ContentPart::text("[tool result elided: 5000 chars]")],
                is_error: false,
            }]
        );
        assert_eq!(projected[1].parts, messages[1].parts, "too small to elide");
        assert!(
            matches!(&projected[2].parts[0], ContentPart::ToolResult { tool_use_id, parts, is_error: false }
                if tool_use_id == "c2" && parts == &[ContentPart::text("[tool result elided: 5000 chars]")]),
            "the third is old too"
        );
        assert_eq!(projected[3].parts, messages[3].parts, "the last two stay");
        assert_eq!(projected[4].parts, messages[4].parts);
        assert!(matches!(
            &projected[3].parts[0],
            ContentPart::ToolResult { is_error: true, .. }
        ));
        assert!(matches!(
            &projected[0].parts[0],
            ContentPart::ToolResult {
                is_error: false,
                ..
            }
        ));
        assert!(matches!(
            &projected[2].parts[0],
            ContentPart::ToolResult {
                is_error: false,
                ..
            }
        ));
    }

    #[test]
    fn nothing_to_elide_is_no_clone() {
        assert_eq!(elide_old_results(&results(&[5_000, 5_000]), 2, 1_000), None);
        assert_eq!(elide_old_results(&results(&[10, 10, 10]), 1, 1_000), None);
        assert_eq!(
            elide_old_results(&[Message::text(Role::User, "hi")], 0, 0),
            None
        );
    }

    fn ids_and_flags(messages: &[Message]) -> Vec<(String, bool)> {
        messages
            .iter()
            .flat_map(|m| m.parts.iter())
            .filter_map(|p| match p {
                ContentPart::ToolResult {
                    tool_use_id,
                    is_error,
                    ..
                } => Some((tool_use_id.clone(), *is_error)),
                _ => None,
            })
            .collect()
    }

    proptest! {
        #[test]
        fn the_projection_keeps_ids_and_the_tail_and_is_idempotent(
            sizes in proptest::collection::vec(0usize..3_000, 0..12),
            keep in 0usize..6,
        ) {
            let messages = results(&sizes);
            let once = elide_old_results(&messages, keep, 1_000).unwrap_or_else(|| messages.clone());
            prop_assert_eq!(ids_and_flags(&once), ids_and_flags(&messages));
            let total = sizes.len();
            for (n, message) in once.iter().enumerate() {
                let untouched = n + keep >= total || sizes[n] < 1_000;
                prop_assert_eq!(message.parts == messages[n].parts, untouched, "message {}", n);
            }
            let twice = elide_old_results(&once, keep, 1_000).unwrap_or_else(|| once.clone());
            prop_assert_eq!(twice, once);
        }
    }
}

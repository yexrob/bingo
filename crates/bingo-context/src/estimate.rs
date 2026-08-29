//! What a journal costs, in the kernel's own arithmetic: ~4 ASCII characters
//! or one non-ASCII character per token, a flat 1,600 per image.
//!
//! The kernel keeps this ruler in `bingo_core::context::estimate_tokens`, which
//! a plugin may not import (ADR-0001) and the sdk does not re-export, so the
//! rule is spelled again here. Nothing outside this crate compares the two
//! numbers: the kernel's acceptance rule weighs `before` against `after`, and
//! both come from this module.

use bingo_sdk::{ContentPart, Item, ItemBody, SystemBlock};

/// A string as the model would be charged for it.
pub fn text(s: &str) -> u64 {
    let (ascii, other) = s.chars().fold((0u64, 0u64), |(a, o), c| {
        if c.is_ascii() { (a + 1, o) } else { (a, o + 1) }
    });
    ascii.div_ceil(4) + other
}

pub fn blocks(blocks: &[SystemBlock]) -> u64 {
    blocks.iter().map(|b| text(&b.text)).sum()
}

pub fn items(items: &[Item]) -> u64 {
    items.iter().map(item).sum()
}

/// One item's share of the request. The fold's own wrappers — the note that
/// opens a summary, the marker of an interruption — are a handful of tokens
/// against a budget in the thousands, and repeating their wording here would
/// be a second copy of the fold.
pub fn item(item: &Item) -> u64 {
    match &item.body {
        ItemBody::User { parts, .. } => content(parts),
        ItemBody::Assistant { text: t } | ItemBody::Reasoning { text: t, .. } => text(t),
        ItemBody::ToolCall {
            name,
            input,
            output,
            ..
        } => {
            let result = output.as_ref().map(|o| content(&o.parts)).unwrap_or(0);
            text(name) + text(&input.to_string()) + result
        }
        ItemBody::Compaction { summary, .. } => text(summary),
        ItemBody::Interruption { marker } => text(marker),
        ItemBody::QuestionAnswer {
            question, answer, ..
        } => text(question) + text(answer),
        _ => 0,
    }
}

fn content(parts: &[ContentPart]) -> u64 {
    parts
        .iter()
        .map(|p| match p {
            ContentPart::Text { text: t } | ContentPart::Reasoning { text: t, .. } => text(t),
            ContentPart::Image { .. } => 1_600,
            ContentPart::ToolUse { name, input, .. } => text(name) + text(&input.to_string()),
            ContentPart::ToolResult { parts, .. } => content(parts),
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{assistant, tool, user};

    #[test]
    fn four_ascii_characters_are_one_token_and_a_cjk_character_is_one() {
        assert_eq!(text("abcd"), 1);
        assert_eq!(text("abcde"), 2);
        assert_eq!(text("汉字"), 2);
    }

    #[test]
    fn an_image_costs_a_flat_sixteen_hundred() {
        assert_eq!(
            content(&[ContentPart::Image {
                media_type: "image/png".into(),
                data: "aaaa".into(),
            }]),
            1_600
        );
    }

    #[test]
    fn a_tool_call_costs_its_name_its_input_and_its_result() {
        let call = tool("t", "Read", r#"{"path":"/a"}"#, Some("hello there"));
        assert_eq!(
            item(&call),
            text("Read") + text(r#"{"path":"/a"}"#) + text("hello there")
        );
    }

    #[test]
    fn a_journal_is_the_sum_of_its_items() {
        let journal = vec![user("u", "hello"), assistant("a", "hi there")];
        assert_eq!(items(&journal), item(&journal[0]) + item(&journal[1]));
    }
}

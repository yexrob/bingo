//! The cheap token estimate every measuring site shares (ADR-0006): ~4 ASCII
//! chars or one non-ASCII char per token, a flat 1 600 per image, tool
//! schemas as their JSON. The kernel anchors it on the server's count; a
//! plugin that cuts uses the same rule so its `before` and `after` mean what
//! the kernel's numbers mean.

use crate::event::{Item, ItemBody};
use crate::model::{ContentPart, Message, SystemBlock, ToolSpec};

pub const IMAGE_TOKENS: u64 = 1_600;

pub fn text(s: &str) -> u64 {
    let (ascii, other) = s.chars().fold((0u64, 0u64), |(a, o), c| {
        if c.is_ascii() { (a + 1, o) } else { (a, o + 1) }
    });
    ascii.div_ceil(4) + other
}

pub fn parts(parts: &[ContentPart]) -> u64 {
    parts
        .iter()
        .map(|p| match p {
            ContentPart::Text { text: t } | ContentPart::Reasoning { text: t, .. } => text(t),
            ContentPart::Image(_) => IMAGE_TOKENS,
            ContentPart::ToolUse { name, input, .. } => text(name) + text(&input.to_string()),
            ContentPart::ToolResult { parts: inner, .. } => self::parts(inner),
        })
        .sum()
}

pub fn blocks(blocks: &[SystemBlock]) -> u64 {
    blocks.iter().map(|b| text(&b.text)).sum()
}

/// One item's own content, before the fold adds its wrappers.
pub fn item(item: &Item) -> u64 {
    match &item.body {
        ItemBody::User { parts: p, .. } => parts(p),
        ItemBody::Assistant { text: t } | ItemBody::Reasoning { text: t, .. } => text(t),
        ItemBody::ToolCall {
            name,
            input,
            output,
            ..
        } => {
            let result = output.as_ref().map(|o| parts(&o.parts)).unwrap_or(0);
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

pub fn items(items: &[Item]) -> u64 {
    items.iter().map(item).sum()
}

/// A whole request as the provider would count it.
pub fn estimate(system: &[SystemBlock], messages: &[Message], tools: &[ToolSpec]) -> u64 {
    let mut n = blocks(system);
    for m in messages {
        n += parts(&m.parts);
    }
    for t in tools {
        n += text(&t.name) + text(&t.description) + text(&t.input_schema.to_string());
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Image;

    #[test]
    fn four_ascii_characters_are_one_token_and_a_cjk_character_is_one() {
        assert_eq!(text("abcd"), 1);
        assert_eq!(text("abcde"), 2);
        assert_eq!(text("汉字"), 2);
        assert_eq!(text(""), 0);
    }

    #[test]
    fn an_image_costs_a_flat_sixteen_hundred_wherever_it_sits() {
        let image = ContentPart::Image(Image {
            media_type: "image/png".into(),
            data: "aaaa".into(),
        });
        assert_eq!(parts(std::slice::from_ref(&image)), IMAGE_TOKENS);
        assert_eq!(
            parts(&[ContentPart::ToolResult {
                tool_use_id: "c".into(),
                parts: vec![image],
                is_error: false,
            }]),
            IMAGE_TOKENS
        );
    }

    #[test]
    fn a_request_counts_its_system_messages_and_tool_schemas() {
        let system = [SystemBlock {
            text: "abcdefgh".into(),
            cache: false,
        }];
        let messages = [Message::text(crate::model::Role::User, "abcd")];
        let tools = [ToolSpec {
            name: "Read".into(),
            description: "read".into(),
            input_schema: serde_json::json!({}),
            meta: Default::default(),
        }];
        assert_eq!(estimate(&system, &messages, &tools), 2 + 1 + 1 + 1 + 1);
    }
}

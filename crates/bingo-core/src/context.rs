//! The second reducer: journal → provider messages. Also the one ruler for
//! context size, used by the compaction trigger and by every display.

use bingo_sdk::*;

/// Folds items into the messages a provider receives. Pure; the golden tests
/// per journal version live next to it.
#[derive(Debug)]
pub struct ContextView;

impl ContextView {
    /// Apply `Compacted` and `Rewound` to the items in a journal, then fold.
    pub fn fold(frames: &[Frame]) -> Vec<Message> {
        Self::fold_items(&Self::items(frames))
    }

    /// The transcript after compaction and rewind, in order.
    pub fn items(frames: &[Frame]) -> Vec<Item> {
        let mut items: Vec<Item> = Vec::new();
        for frame in frames {
            match &frame.event {
                Event::ItemStarted { item }
                | Event::ItemUpdated { item }
                | Event::ItemCompleted { item } => {
                    match items.iter_mut().find(|i| i.id == item.id) {
                        Some(slot) => *slot = item.clone(),
                        None => items.push(item.clone()),
                    }
                }
                Event::Compacted {
                    boundary,
                    kept,
                    summary,
                    ..
                } => {
                    let Some(cut) = items.iter().position(|i| &i.id == boundary) else {
                        continue;
                    };
                    let summary_item = items
                        .iter()
                        .position(|i| &i.id == summary)
                        .map(|p| items.remove(p));
                    let cut = cut.min(items.len());
                    let (head, tail) = items.split_at(cut);
                    let mut next: Vec<Item> = head
                        .iter()
                        .filter(|i| kept.contains(&i.id))
                        .cloned()
                        .collect();
                    next.extend(summary_item);
                    next.extend(tail.iter().cloned());
                    items = next;
                }
                Event::Rewound { dropped, .. } => items.retain(|i| !dropped.contains(&i.id)),
                _ => {}
            }
        }
        items
    }

    pub fn fold_items(items: &[Item]) -> Vec<Message> {
        let mut out = Folder::default();
        for item in items {
            match &item.body {
                ItemBody::User { parts, .. } => out.user(parts.clone()),
                ItemBody::Assistant { text } => {
                    if !text.is_empty() {
                        out.assistant(vec![ContentPart::text(text.clone())]);
                    }
                }
                ItemBody::Reasoning {
                    text,
                    provider_metadata,
                } => {
                    if !text.is_empty() {
                        out.assistant(vec![ContentPart::Reasoning {
                            text: text.clone(),
                            provider_metadata: provider_metadata.clone(),
                        }]);
                    }
                }
                ItemBody::ToolCall {
                    call_id,
                    name,
                    input,
                    output,
                    ..
                } => {
                    out.assistant(vec![ContentPart::ToolUse {
                        id: call_id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                    }]);
                    let (parts, is_error) = match output {
                        Some(o) => (o.parts.clone(), o.is_error),
                        None => (
                            vec![ContentPart::text("[no result: the call did not complete]")],
                            true,
                        ),
                    };
                    out.pending.push(ContentPart::ToolResult {
                        tool_use_id: call_id.clone(),
                        parts,
                        is_error,
                    });
                }
                ItemBody::Compaction { summary, .. } => {
                    out.user(vec![ContentPart::text(format!(
                        "[Summary of the conversation so far]\n{summary}"
                    ))]);
                }
                ItemBody::Interruption { marker } => {
                    out.user(vec![ContentPart::text(marker.clone())])
                }
                ItemBody::QuestionAnswer {
                    question, answer, ..
                } => {
                    out.user(vec![ContentPart::text(format!(
                        "Q: {question}\nA: {answer}"
                    ))]);
                }
                ItemBody::Action { .. }
                | ItemBody::Rewind { .. }
                | ItemBody::Notice { .. }
                | ItemBody::PermissionReceipt { .. }
                | ItemBody::Asset { .. } => {}
            }
        }
        out.finish()
    }
}

#[derive(Default)]
struct Folder {
    messages: Vec<Message>,
    /// Tool results owed to the next user message; they always come first in it.
    pending: Vec<ContentPart>,
}

impl Folder {
    fn user(&mut self, parts: Vec<ContentPart>) {
        let mut all = std::mem::take(&mut self.pending);
        all.extend(parts);
        match self.messages.last_mut() {
            Some(m) if m.role == Role::User => m.parts.extend(all),
            _ => self.messages.push(Message::user(all)),
        }
    }

    fn assistant(&mut self, parts: Vec<ContentPart>) {
        if !self.pending.is_empty() {
            let owed = std::mem::take(&mut self.pending);
            self.user(owed);
        }
        match self.messages.last_mut() {
            Some(m) if m.role == Role::Assistant => m.parts.extend(parts),
            _ => self.messages.push(Message::assistant(parts)),
        }
    }

    fn finish(mut self) -> Vec<Message> {
        if !self.pending.is_empty() {
            let owed = std::mem::take(&mut self.pending);
            self.user(owed);
        }
        self.messages.retain(|m| !m.parts.is_empty());
        self.messages
    }
}

/// A cheap estimate when the provider cannot count: ~4 ASCII chars or one
/// non-ASCII char per token, a flat 1,600 per image, schemas as their JSON.
pub fn estimate_tokens(system: &[SystemBlock], messages: &[Message], tools: &[ToolSpec]) -> u64 {
    let mut n: u64 = system.iter().map(|b| text_tokens(&b.text)).sum();
    for m in messages {
        n += parts_tokens(&m.parts);
    }
    for t in tools {
        n += text_tokens(&t.name)
            + text_tokens(&t.description)
            + text_tokens(&t.input_schema.to_string());
    }
    n
}

fn parts_tokens(parts: &[ContentPart]) -> u64 {
    parts
        .iter()
        .map(|p| match p {
            ContentPart::Text { text } => text_tokens(text),
            ContentPart::Image { .. } => 1_600,
            ContentPart::ToolUse { name, input, .. } => {
                text_tokens(name) + text_tokens(&input.to_string())
            }
            ContentPart::ToolResult { parts, .. } => parts_tokens(parts),
            ContentPart::Reasoning { text, .. } => text_tokens(text),
        })
        .sum()
}

fn text_tokens(s: &str) -> u64 {
    let (ascii, other) = s.chars().fold((0u64, 0u64), |(a, o), c| {
        if c.is_ascii() { (a + 1, o) } else { (a, o + 1) }
    });
    ascii.div_ceil(4) + other
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::Timestamp;

    fn item(id: &str, body: ItemBody) -> Item {
        Item {
            id: ItemId::from_raw(id),
            turn: None,
            round: 0,
            status: ItemStatus::Completed,
            started_at: Timestamp::from_second(0).unwrap(),
            completed_at: None,
            intent: None,
            body,
            meta: Default::default(),
        }
    }

    fn user(id: &str, text: &str) -> Item {
        item(
            id,
            ItemBody::User {
                parts: vec![ContentPart::text(text)],
                origin: Origin::surface("t"),
            },
        )
    }

    fn tool(id: &str, call: &str, output: Option<&str>) -> Item {
        item(
            id,
            ItemBody::ToolCall {
                call_id: call.into(),
                name: "Read".into(),
                input: serde_json::json!({"file_path": "x"}),
                output: output.map(ToolOutput::text),
                progress: None,
                child_session: None,
                duration_ms: None,
            },
        )
    }

    #[test]
    fn a_tool_round_folds_into_assistant_then_user_with_results_first() {
        let items = vec![
            user("i1", "read x"),
            item(
                "i2",
                ItemBody::Assistant {
                    text: "Looking.".into(),
                },
            ),
            tool("i3", "c1", Some("contents")),
            user("i4", "also this"),
            item(
                "i5",
                ItemBody::Assistant {
                    text: "Done.".into(),
                },
            ),
        ];
        let msgs = ContextView::fold_items(&items);
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
        assert!(matches!(msgs[1].parts[1], ContentPart::ToolUse { .. }));
        assert_eq!(msgs[2].role, Role::User);
        assert!(
            matches!(&msgs[2].parts[0], ContentPart::ToolResult { tool_use_id, .. } if tool_use_id == "c1")
        );
        assert_eq!(msgs[2].parts[1].as_text(), Some("also this"));
        assert_eq!(msgs[3].role, Role::Assistant);
    }

    #[test]
    fn every_tool_use_gets_a_result_even_without_output() {
        let items = vec![user("i1", "go"), tool("i2", "c1", None)];
        let msgs = ContextView::fold_items(&items);
        assert_eq!(msgs.len(), 3);
        assert!(matches!(
            &msgs[2].parts[0],
            ContentPart::ToolResult { is_error: true, .. }
        ));
    }

    #[test]
    fn empty_assistant_text_never_becomes_a_message() {
        let items = vec![
            user("i1", "go"),
            item(
                "i2",
                ItemBody::Assistant {
                    text: String::new(),
                },
            ),
        ];
        assert_eq!(ContextView::fold_items(&items).len(), 1);
    }

    #[test]
    fn compaction_replaces_the_head_and_keeps_what_it_says() {
        let ts = Timestamp::from_second(0).unwrap();
        let ses = SessionId::from_raw("ses_1");
        let f = |seq: u64, event: Event| Frame {
            seq: Seq(seq),
            ts,
            session: ses.clone(),
            cause: None,
            event,
        };
        let frames = vec![
            f(
                1,
                Event::ItemCompleted {
                    item: user("i1", "first"),
                },
            ),
            f(
                2,
                Event::ItemCompleted {
                    item: item("i2", ItemBody::Assistant { text: "a".into() }),
                },
            ),
            f(
                3,
                Event::ItemCompleted {
                    item: user("i3", "second"),
                },
            ),
            f(
                4,
                Event::ItemCompleted {
                    item: item("i4", ItemBody::Assistant { text: "b".into() }),
                },
            ),
            f(
                5,
                Event::ItemCompleted {
                    item: item(
                        "i9",
                        ItemBody::Compaction {
                            summary: "we did a".into(),
                            replaced: 2,
                            before: 100,
                            after: 20,
                            duration_ms: 1,
                        },
                    ),
                },
            ),
            f(
                6,
                Event::Compacted {
                    generation: 1,
                    boundary: ItemId::from_raw("i3"),
                    summary: ItemId::from_raw("i9"),
                    kept: vec![ItemId::from_raw("i1")],
                },
            ),
        ];
        let items = ContextView::items(&frames);
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["i1", "i9", "i3", "i4"]);
        let msgs = ContextView::fold(&frames);
        assert_eq!(msgs[0].parts[0].as_text(), Some("first"));
        assert!(msgs[0].parts[1].as_text().unwrap().starts_with("[Summary"));
    }

    #[test]
    fn rewind_drops_items() {
        let ts = Timestamp::from_second(0).unwrap();
        let ses = SessionId::from_raw("ses_1");
        let f = |seq: u64, event: Event| Frame {
            seq: Seq(seq),
            ts,
            session: ses.clone(),
            cause: None,
            event,
        };
        let frames = vec![
            f(
                1,
                Event::ItemCompleted {
                    item: user("i1", "a"),
                },
            ),
            f(
                2,
                Event::ItemCompleted {
                    item: item("i2", ItemBody::Assistant { text: "b".into() }),
                },
            ),
            f(
                3,
                Event::Rewound {
                    generation: 1,
                    to_turn: TurnId::from_raw("t"),
                    dropped: vec![ItemId::from_raw("i2")],
                    files_restored: vec![],
                },
            ),
        ];
        assert_eq!(ContextView::items(&frames).len(), 1);
    }

    #[test]
    fn the_estimate_counts_cjk_per_character_and_images_flat() {
        assert_eq!(text_tokens("abcdefgh"), 2);
        assert_eq!(text_tokens("你好"), 2);
        let msgs = vec![Message::user(vec![ContentPart::Image {
            media_type: "image/png".into(),
            data: String::new(),
        }])];
        assert_eq!(estimate_tokens(&[], &msgs, &[]), 1_600);
    }
}

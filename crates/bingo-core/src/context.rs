//! The second reducer: journal → provider messages. Also the one ruler for
//! context size, used by the compaction trigger and by every display.

pub mod budget;
pub mod elide;

use bingo_sdk::*;
use serde_json::Value;

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
                    // A boundary this journal never saw is not a cut it can make.
                    if let Some(cut) = items.iter().position(|i| &i.id == boundary) {
                        splice_compaction(&mut items, cut, kept, summary);
                    }
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
            out.item(item);
        }
        out.finish()
    }
}

/// The one splice a compaction performs: before `cut` only `kept` survives, the
/// summary item takes the seam, the tail is untouched.
pub(crate) fn splice_compaction(
    items: &mut Vec<Item>,
    cut: usize,
    kept: &[ItemId],
    summary: &ItemId,
) {
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
    *items = next;
}

/// What the model reads when a journal opens on its own words: the API wants
/// a person to speak first, and nothing is invented about what they said.
const OPENING_NOTE: &str = "[The conversation begins here.]";

#[derive(Default)]
struct Folder {
    messages: Vec<Message>,
    /// Tool results owed to the next user message; they always come first in it.
    pending: Vec<ContentPart>,
    /// The turn and round of the open assistant message: every tool call of
    /// one response joins it, and their results join the one user message
    /// after it, as the model produced them.
    round: Option<(Option<TurnId>, u32)>,
}

impl Folder {
    /// One item as the provider sees it; a body with no wire form is skipped.
    fn item(&mut self, item: &Item) {
        let round = (item.turn.clone(), item.round);
        match &item.body {
            ItemBody::User { parts, origin } => self.user(spoken(parts, origin)),
            ItemBody::Assistant { text } => self.text(text),
            ItemBody::Reasoning {
                text,
                provider_metadata,
            } => self.reasoning(text, provider_metadata),
            ItemBody::ToolCall {
                call_id,
                name,
                input,
                output,
                ..
            } => self.tool_call(round, call_id, name, input, output.as_ref()),
            ItemBody::Compaction { summary, .. } => {
                self.note(format!("[Summary of the conversation so far]\n{summary}"))
            }
            ItemBody::Interruption { marker } => self.note(marker.clone()),
            ItemBody::QuestionAnswer {
                question, answer, ..
            } => self.note(format!("Q: {question}\nA: {answer}")),
            ItemBody::Action {
                name,
                args,
                result: Some(result),
            } => self.note(format!("[{name}] {}\n{}", plain(args), plain(result))),
            ItemBody::Action { result: None, .. }
            | ItemBody::Rewind { .. }
            | ItemBody::Notice { .. }
            | ItemBody::PermissionReceipt { .. }
            | ItemBody::Asset { .. } => {}
        }
    }

    /// The kernel speaking to the model in the user's turn.
    fn note(&mut self, text: String) {
        self.user(vec![ContentPart::text(text)]);
    }

    fn text(&mut self, text: &str) {
        if !text.is_empty() {
            self.assistant(vec![ContentPart::text(text.to_string())]);
        }
    }

    /// Reasoning with no text still goes back when it carries the provider's
    /// replay data: an encrypted chain of thought without a summary is what
    /// a stateless OpenAI turn gets, and dropping it makes the model think
    /// again from nothing.
    fn reasoning(&mut self, text: &str, provider_metadata: &ProviderMetadata) {
        if !text.is_empty() || !provider_metadata.is_empty() {
            self.assistant(vec![ContentPart::Reasoning {
                text: text.to_string(),
                provider_metadata: provider_metadata.clone(),
            }]);
        }
    }

    /// The call, then the result it owes the next user message. A call with no
    /// output never completed, and the model is told so.
    fn tool_call(
        &mut self,
        round: (Option<TurnId>, u32),
        call_id: &str,
        name: &str,
        input: &Value,
        output: Option<&ToolOutput>,
    ) {
        let part = ContentPart::ToolUse {
            id: call_id.to_string(),
            name: name.to_string(),
            input: input.clone(),
        };
        match self.messages.last_mut() {
            Some(m) if m.role == Role::Assistant && self.round.as_ref() == Some(&round) => {
                m.parts.push(part);
            }
            _ => {
                self.assistant(vec![part]);
                self.round = Some(round);
            }
        }
        let (parts, is_error) = match output {
            Some(o) => (o.parts.clone(), o.is_error),
            None => (
                vec![ContentPart::text("[no result: the call did not complete]")],
                true,
            ),
        };
        self.pending.push(ContentPart::ToolResult {
            tool_use_id: call_id.to_string(),
            parts,
            is_error,
        });
    }

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
        if self
            .messages
            .first()
            .is_some_and(|m| m.role == Role::Assistant)
        {
            self.messages
                .insert(0, Message::text(Role::User, OPENING_NOTE));
        }
        self.messages
    }
}

pub use bingo_sdk::tokens::estimate as estimate_tokens;

/// A JSON value as a person wrote it: a string verbatim, anything else compact.
fn plain(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A user item that names who spoke carries that name to the model
/// (ADR-0010 §5): an agent's message, or a person's in a group, must not read
/// as the one the session works for.
fn spoken(parts: &[ContentPart], origin: &Origin) -> Vec<ContentPart> {
    let Some(principal) = origin.principal.as_deref().filter(|p| !p.is_empty()) else {
        return parts.to_vec();
    };
    let mut out = Vec::with_capacity(parts.len() + 1);
    out.push(ContentPart::text(format!("[from {principal}]")));
    out.extend(parts.iter().cloned());
    out
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
    fn reasoning_with_only_replay_data_still_goes_back_to_the_provider() {
        let mut replay = ProviderMetadata::new();
        replay.insert(
            "openai".into(),
            serde_json::from_value(serde_json::json!({"id": "rs_1", "encrypted_content": "gAAA"}))
                .unwrap(),
        );
        let items = vec![
            user("i1", "go"),
            item(
                "i2",
                ItemBody::Reasoning {
                    text: String::new(),
                    provider_metadata: replay.clone(),
                },
            ),
            item(
                "i3",
                ItemBody::Reasoning {
                    text: String::new(),
                    provider_metadata: ProviderMetadata::new(),
                },
            ),
        ];
        let messages = ContextView::fold_items(&items);
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[1].parts,
            vec![ContentPart::Reasoning {
                text: String::new(),
                provider_metadata: replay,
            }],
            "the encrypted part is replayed; the empty one is not"
        );
    }

    // ----- what every projection must satisfy, on random journals -----

    /// One item of a random journal. Tool calls are numbered so their ids
    /// are unique, as the kernel mints them.
    #[derive(Clone, Debug)]
    enum Shape {
        User(String),
        Assistant(String),
        Reasoning { text: String, replay: bool },
        Tool { answered: bool },
        Interruption,
        Compaction(String),
    }

    fn any_shape() -> impl proptest::strategy::Strategy<Value = Shape> {
        use proptest::prelude::*;
        prop_oneof![
            "[a-z ]{0,12}".prop_map(Shape::User),
            "[a-z ]{0,12}".prop_map(Shape::Assistant),
            ("[a-z ]{0,12}", any::<bool>())
                .prop_map(|(text, replay)| Shape::Reasoning { text, replay }),
            any::<bool>().prop_map(|answered| Shape::Tool { answered }),
            Just(Shape::Interruption),
            "[a-z ]{1,12}".prop_map(Shape::Compaction),
        ]
    }

    fn items_of(shapes: &[Shape]) -> Vec<Item> {
        let mut replay = ProviderMetadata::new();
        replay.insert("p".into(), serde_json::Map::new());
        shapes
            .iter()
            .enumerate()
            .map(|(n, shape)| {
                let id = format!("i{n}");
                match shape {
                    Shape::User(text) => user(&id, text),
                    Shape::Assistant(text) => item(&id, ItemBody::Assistant { text: text.clone() }),
                    Shape::Reasoning { text, replay: r } => item(
                        &id,
                        ItemBody::Reasoning {
                            text: text.clone(),
                            provider_metadata: if *r {
                                replay.clone()
                            } else {
                                ProviderMetadata::new()
                            },
                        },
                    ),
                    Shape::Tool { answered } => {
                        tool(&id, &format!("c{n}"), answered.then_some("out"))
                    }
                    Shape::Interruption => item(
                        &id,
                        ItemBody::Interruption {
                            marker: "[interrupted]".into(),
                        },
                    ),
                    Shape::Compaction(summary) => item(
                        &id,
                        ItemBody::Compaction {
                            summary: summary.clone(),
                            replaced: 0,
                            before: 0,
                            after: 0,
                            duration_ms: 0,
                        },
                    ),
                }
            })
            .collect()
    }

    fn tool_use_ids(message: &Message) -> Vec<&str> {
        message
            .parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolUse { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect()
    }

    fn tool_result_ids(message: &Message) -> Vec<&str> {
        message
            .parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolResult { tool_use_id, .. } => Some(tool_use_id.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The frames a live session would have written for these items.
    fn frames_of(items: &[Item]) -> Vec<Frame> {
        let ts = Timestamp::from_second(0).unwrap();
        items
            .iter()
            .enumerate()
            .flat_map(|(n, item)| {
                let seq = |k: u64| Seq(1 + 2 * n as u64 + k);
                [
                    (seq(0), Event::ItemStarted { item: item.clone() }),
                    (seq(1), Event::ItemCompleted { item: item.clone() }),
                ]
            })
            .map(|(seq, event)| Frame {
                seq,
                ts,
                session: SessionId::from_raw("ses_1"),
                cause: None,
                event,
            })
            .collect()
    }

    proptest::proptest! {
        #[test]
        fn every_projection_is_legal_for_the_api(
            shapes in proptest::collection::vec(any_shape(), 0..24)
        ) {
            let messages = ContextView::fold_items(&items_of(&shapes));
            if let Some(first) = messages.first() {
                proptest::prop_assert_eq!(first.role, Role::User, "a conversation opens with the user");
            }
            for message in &messages {
                proptest::prop_assert!(!message.parts.is_empty(), "no message is empty");
            }
            for (n, message) in messages.iter().enumerate() {
                let uses = tool_use_ids(message);
                if uses.is_empty() {
                    continue;
                }
                let next = messages.get(n + 1);
                proptest::prop_assert!(next.is_some(), "a tool use is always answered");
                let next = next.unwrap();
                proptest::prop_assert_eq!(next.role, Role::User);
                let mut results = tool_result_ids(next);
                results.sort_unstable();
                let mut wanted = uses.clone();
                wanted.sort_unstable();
                proptest::prop_assert_eq!(results, wanted, "one result per use, none extra");
            }
            for message in &messages {
                proptest::prop_assert!(
                    message.role == Role::User || tool_result_ids(message).is_empty(),
                    "results are in user messages only"
                );
            }
        }

        #[test]
        fn a_replayed_journal_folds_exactly_like_the_live_session(
            shapes in proptest::collection::vec(any_shape(), 0..16)
        ) {
            let items = items_of(&shapes);
            let replayed = ContextView::items(&frames_of(&items));
            proptest::prop_assert_eq!(&replayed, &items);
            proptest::prop_assert_eq!(
                ContextView::fold_items(&replayed),
                ContextView::fold_items(&items)
            );
        }
    }

    /// Version 1 frames, recorded from a real tool round through the fake
    /// provider. What the kernel makes of them is pinned so a format change
    /// is a deliberate migration, never an accident.
    #[test]
    fn version_one_frames_fold_to_the_same_messages_forever() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/frames-v1.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        let frames: Vec<Frame> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let messages = ContextView::fold_items(&ContextView::items(&frames));
        insta::assert_json_snapshot!(messages);
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
        assert_eq!(bingo_sdk::tokens::text("abcdefgh"), 2);
        assert_eq!(bingo_sdk::tokens::text("你好"), 2);
        let msgs = vec![Message::user(vec![ContentPart::Image {
            media_type: "image/png".into(),
            data: String::new(),
        }])];
        assert_eq!(estimate_tokens(&[], &msgs, &[]), 1_600);
    }

    /// An action with a result is told to the model as the person wrote it
    /// (ADR-0008 §5); one still running has no wire form.
    #[test]
    fn actions_with_results_reach_the_model_as_notes() {
        let done = item(
            "a1",
            ItemBody::Action {
                name: "!".into(),
                args: serde_json::json!("ls"),
                result: Some(serde_json::json!("a\nb\n[exit 1]")),
            },
        );
        let pending = item(
            "a2",
            ItemBody::Action {
                name: "login".into(),
                args: serde_json::json!({"provider": "x"}),
                result: None,
            },
        );
        let messages = ContextView::fold_items(&[done, pending]);
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].parts[0].as_text(),
            Some("[!] ls\na\nb\n[exit 1]")
        );
    }
}

//! Folds a provider's `ModelEvent`s into `Item`s. Text and reasoning blocks
//! become running items that complete on their end event; a tool call
//! becomes a pending item once its input is complete. The finish rules
//! decide what an unclosed block means when the stream stops early.

use std::collections::HashMap;

use bingo_sdk::*;
use jiff::Timestamp;
use serde_json::Value;

/// What the loop publishes as it folds.
#[derive(Clone, Debug, PartialEq)]
pub enum Emit {
    Started(Item),
    Delta {
        item: ItemId,
        n: u32,
        kind: DeltaKind,
        data: String,
    },
    Completed(Item),
}

#[derive(Debug)]
struct Open {
    item: Item,
    deltas: u32,
}

/// The folded response: every item in order, the calls to gate, and how the stream ended.
#[derive(Debug, Default)]
pub struct Finished {
    pub items: Vec<Item>,
    pub tool_calls: Vec<(ItemId, ToolCall)>,
    pub usage: Usage,
    pub finish_reason: Option<FinishReason>,
    pub error: Option<ProviderError>,
    /// A text or reasoning block was still open when the stream stopped.
    pub truncated: bool,
}

#[derive(Debug)]
pub struct Accumulator {
    turn: TurnId,
    round: u32,
    items: Vec<Item>,
    open: HashMap<String, Open>,
    tool_inputs: HashMap<String, (String, String)>,
    tool_calls: Vec<(ItemId, ToolCall)>,
    usage: Usage,
    finish_reason: Option<FinishReason>,
    error: Option<ProviderError>,
}

impl Accumulator {
    pub fn new(turn: TurnId, round: u32) -> Self {
        Self {
            turn,
            round,
            items: Vec::new(),
            open: HashMap::new(),
            tool_inputs: HashMap::new(),
            tool_calls: Vec::new(),
            usage: Usage::default(),
            finish_reason: None,
            error: None,
        }
    }

    /// The ids of every item started so far, for withdrawal on retry.
    pub fn item_ids(&self) -> Vec<ItemId> {
        self.items.iter().map(|i| i.id.clone()).collect()
    }

    fn fresh(&self, body: ItemBody, status: ItemStatus) -> Item {
        Item {
            id: ItemId::mint(),
            turn: Some(self.turn.clone()),
            round: self.round,
            status,
            started_at: Timestamp::now(),
            completed_at: None,
            intent: None,
            body,
            meta: Default::default(),
        }
    }

    fn start_block(&mut self, id: String, body: ItemBody) -> Emit {
        let item = self.fresh(body, ItemStatus::Running);
        self.items.push(item.clone());
        self.open.insert(
            id,
            Open {
                item: item.clone(),
                deltas: 0,
            },
        );
        Emit::Started(item)
    }

    fn delta(&mut self, id: &str, kind: DeltaKind, data: String) -> Option<Emit> {
        let open = self.open.get_mut(id)?;
        match (&mut open.item.body, kind) {
            (ItemBody::Assistant { text }, DeltaKind::Text) => text.push_str(&data),
            (ItemBody::Reasoning { text, .. }, DeltaKind::Reasoning) => text.push_str(&data),
            _ => return None,
        }
        let n = open.deltas;
        open.deltas += 1;
        Some(Emit::Delta {
            item: open.item.id.clone(),
            n,
            kind,
            data,
        })
    }

    fn end_block(&mut self, id: &str, status: ItemStatus) -> Option<Emit> {
        let mut open = self.open.remove(id)?;
        open.item.status = status;
        open.item.completed_at = Some(Timestamp::now());
        self.replace(&open.item);
        Some(Emit::Completed(open.item))
    }

    fn replace(&mut self, item: &Item) {
        if let Some(slot) = self.items.iter_mut().find(|i| i.id == item.id) {
            *slot = item.clone();
        }
    }

    pub fn push(&mut self, event: ModelEvent) -> Vec<Emit> {
        match event {
            ModelEvent::StreamStart { .. } | ModelEvent::ResponseMetadata { .. } => Vec::new(),
            ModelEvent::TextStart { id } => vec![self.start_block(
                id,
                ItemBody::Assistant {
                    text: String::new(),
                },
            )],
            ModelEvent::TextDelta { id, delta } => self
                .delta(&id, DeltaKind::Text, delta)
                .into_iter()
                .collect(),
            ModelEvent::TextEnd { id } => self
                .end_block(&id, ItemStatus::Completed)
                .into_iter()
                .collect(),
            ModelEvent::ReasoningStart { id } => vec![self.start_block(
                id,
                ItemBody::Reasoning {
                    text: String::new(),
                    provider_metadata: ProviderMetadata::new(),
                },
            )],
            ModelEvent::ReasoningDelta { id, delta } => self
                .delta(&id, DeltaKind::Reasoning, delta)
                .into_iter()
                .collect(),
            ModelEvent::ReasoningEnd {
                id,
                provider_metadata,
            } => {
                if let Some(open) = self.open.get_mut(&id)
                    && let ItemBody::Reasoning {
                        provider_metadata: slot,
                        ..
                    } = &mut open.item.body
                {
                    *slot = provider_metadata;
                }
                self.end_block(&id, ItemStatus::Completed)
                    .into_iter()
                    .collect()
            }
            ModelEvent::ToolInputStart { id, name } => {
                self.tool_inputs.insert(id, (name, String::new()));
                Vec::new()
            }
            ModelEvent::ToolInputDelta { id, delta } => {
                if let Some((_, buf)) = self.tool_inputs.get_mut(&id) {
                    buf.push_str(&delta);
                }
                Vec::new()
            }
            ModelEvent::ToolInputEnd { .. } => Vec::new(),
            ModelEvent::ToolCall { id, name, input } => {
                self.tool_inputs.remove(&id);
                let (input, meta) = match serde_json::from_str::<Value>(&input) {
                    Ok(v) => (v, serde_json::Map::new()),
                    Err(e) => {
                        let mut meta = serde_json::Map::new();
                        meta.insert("invalidInput".into(), Value::String(e.to_string()));
                        (Value::String(input), meta)
                    }
                };
                let mut item = self.fresh(
                    ItemBody::ToolCall {
                        call_id: id.clone(),
                        name: name.clone(),
                        input: input.clone(),
                        output: None,
                        progress: None,
                        child_session: None,
                        duration_ms: None,
                    },
                    ItemStatus::Pending,
                );
                item.meta = meta;
                self.items.push(item.clone());
                self.tool_calls.push((
                    item.id.clone(),
                    ToolCall {
                        call_id: id,
                        name,
                        input,
                    },
                ));
                vec![Emit::Started(item)]
            }
            ModelEvent::Finish {
                usage,
                finish_reason,
            } => {
                self.usage = usage;
                self.finish_reason = Some(finish_reason);
                Vec::new()
            }
            ModelEvent::Error {
                message,
                retryable,
                retry_after_ms,
            } => {
                self.error = Some(if retryable {
                    match retry_after_ms {
                        Some(ms) => ProviderError::RateLimited {
                            retry_after_ms: Some(ms),
                        },
                        None => ProviderError::Stream { message },
                    }
                } else {
                    ProviderError::Request { message }
                });
                Vec::new()
            }
        }
    }

    /// Close whatever is still open. Unclosed text keeps its content; an
    /// unclosed tool input is not a call; `interrupted` marks the survivors.
    pub fn finish(mut self, interrupted: bool) -> (Vec<Emit>, Finished) {
        let status = if interrupted {
            ItemStatus::Interrupted
        } else {
            ItemStatus::Completed
        };
        let truncated = !self.open.is_empty();
        let ids: Vec<String> = self.open.keys().cloned().collect();
        let mut emits = Vec::new();
        for id in ids {
            emits.extend(self.end_block(&id, status));
        }
        if interrupted {
            for item in &mut self.items {
                if let ItemBody::ToolCall { .. } = item.body
                    && item.status == ItemStatus::Pending
                {
                    item.status = ItemStatus::Interrupted;
                }
            }
        }
        let finished = Finished {
            items: self.items,
            tool_calls: self.tool_calls,
            usage: self.usage,
            finish_reason: self.finish_reason,
            error: self.error,
            truncated,
        };
        (emits, finished)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acc() -> Accumulator {
        Accumulator::new(TurnId::from_raw("trn_1"), 0)
    }

    #[test]
    fn a_text_block_starts_streams_and_completes() {
        let mut a = acc();
        let started = a.push(ModelEvent::TextStart { id: "b1".into() });
        assert!(matches!(
            started[0],
            Emit::Started(Item {
                status: ItemStatus::Running,
                ..
            })
        ));
        let d = a.push(ModelEvent::TextDelta {
            id: "b1".into(),
            delta: "Hel".into(),
        });
        assert!(matches!(
            &d[0],
            Emit::Delta {
                n: 0,
                kind: DeltaKind::Text,
                ..
            }
        ));
        a.push(ModelEvent::TextDelta {
            id: "b1".into(),
            delta: "lo".into(),
        });
        let done = a.push(ModelEvent::TextEnd { id: "b1".into() });
        match &done[0] {
            Emit::Completed(item) => {
                assert_eq!(
                    item.body,
                    ItemBody::Assistant {
                        text: "Hello".into()
                    }
                );
                assert_eq!(item.status, ItemStatus::Completed);
            }
            other => panic!("{other:?}"),
        }
        let (emits, fin) = a.finish(false);
        assert!(emits.is_empty());
        assert_eq!(fin.items.len(), 1);
        assert!(!fin.truncated);
    }

    #[test]
    fn a_tool_call_becomes_a_pending_item_with_parsed_input() {
        let mut a = acc();
        a.push(ModelEvent::ToolInputStart {
            id: "c1".into(),
            name: "Read".into(),
        });
        a.push(ModelEvent::ToolInputDelta {
            id: "c1".into(),
            delta: "{\"file_path\":".into(),
        });
        let e = a.push(ModelEvent::ToolCall {
            id: "c1".into(),
            name: "Read".into(),
            input: "{\"file_path\":\"x\"}".into(),
        });
        match &e[0] {
            Emit::Started(item) => {
                assert_eq!(item.status, ItemStatus::Pending);
                assert!(
                    matches!(&item.body, ItemBody::ToolCall { name, input, .. } if name == "Read" && input["file_path"] == "x")
                );
            }
            other => panic!("{other:?}"),
        }
        a.push(ModelEvent::Finish {
            usage: Usage {
                input_tokens: 5,
                output_tokens: 2,
                ..Default::default()
            },
            finish_reason: FinishReason::unified(UnifiedFinish::ToolCalls),
        });
        let (_, fin) = a.finish(false);
        assert_eq!(fin.tool_calls.len(), 1);
        assert_eq!(fin.tool_calls[0].1.call_id, "c1");
        assert_eq!(fin.usage.input_tokens, 5);
        assert_eq!(
            fin.finish_reason.map(|r| r.unified),
            Some(UnifiedFinish::ToolCalls)
        );
    }

    #[test]
    fn invalid_tool_input_is_kept_as_a_string_and_flagged() {
        let mut a = acc();
        let e = a.push(ModelEvent::ToolCall {
            id: "c1".into(),
            name: "Read".into(),
            input: "{not json".into(),
        });
        let Emit::Started(item) = &e[0] else { panic!() };
        assert!(item.meta.contains_key("invalidInput"));
        assert!(matches!(
            &item.body,
            ItemBody::ToolCall {
                input: Value::String(_),
                ..
            }
        ));
    }

    #[test]
    fn an_interrupted_stream_keeps_text_and_drops_unfinished_tool_input() {
        let mut a = acc();
        a.push(ModelEvent::TextStart { id: "b1".into() });
        a.push(ModelEvent::TextDelta {
            id: "b1".into(),
            delta: "partial".into(),
        });
        a.push(ModelEvent::ToolInputStart {
            id: "c1".into(),
            name: "Bash".into(),
        });
        a.push(ModelEvent::ToolInputDelta {
            id: "c1".into(),
            delta: "{\"command\":".into(),
        });
        let (emits, fin) = a.finish(true);
        assert_eq!(emits.len(), 1);
        assert!(fin.truncated);
        assert!(fin.tool_calls.is_empty());
        assert_eq!(fin.items.len(), 1);
        assert_eq!(fin.items[0].status, ItemStatus::Interrupted);
        assert_eq!(
            fin.items[0].body,
            ItemBody::Assistant {
                text: "partial".into()
            }
        );
    }

    #[test]
    fn a_stream_error_is_classified_by_retryability() {
        let mut a = acc();
        a.push(ModelEvent::Error {
            message: "overloaded".into(),
            retryable: true,
            retry_after_ms: None,
        });
        let (_, fin) = a.finish(false);
        assert!(matches!(fin.error, Some(ProviderError::Stream { .. })));
        let mut b = acc();
        b.push(ModelEvent::Error {
            message: "bad".into(),
            retryable: false,
            retry_after_ms: None,
        });
        let (_, fin) = b.finish(false);
        assert!(matches!(fin.error, Some(ProviderError::Request { .. })));
    }

    #[test]
    fn reasoning_metadata_lands_on_the_completed_item() {
        let mut a = acc();
        a.push(ModelEvent::ReasoningStart { id: "r1".into() });
        a.push(ModelEvent::ReasoningDelta {
            id: "r1".into(),
            delta: "think".into(),
        });
        let mut meta = ProviderMetadata::new();
        meta.insert(
            "anthropic".into(),
            serde_json::json!({"signature": "sig"})
                .as_object()
                .cloned()
                .unwrap(),
        );
        let done = a.push(ModelEvent::ReasoningEnd {
            id: "r1".into(),
            provider_metadata: meta.clone(),
        });
        let Emit::Completed(item) = &done[0] else {
            panic!()
        };
        assert_eq!(
            item.body,
            ItemBody::Reasoning {
                text: "think".into(),
                provider_metadata: meta
            }
        );
    }
}

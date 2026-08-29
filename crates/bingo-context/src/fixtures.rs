//! Journal items as the tests write them down.

use bingo_sdk::{ContentPart, Item, ItemBody, ItemId, ItemStatus, Origin, ToolOutput, TurnId};
use jiff::Timestamp;

fn at(id: &str, body: ItemBody) -> Item {
    Item {
        id: ItemId::from_raw(id),
        turn: Some(TurnId::from_raw("trn_1")),
        round: 0,
        status: ItemStatus::Completed,
        started_at: Timestamp::UNIX_EPOCH,
        completed_at: None,
        intent: None,
        body,
        meta: serde_json::Map::new(),
    }
}

pub fn user(id: &str, text: &str) -> Item {
    at(
        id,
        ItemBody::User {
            parts: vec![ContentPart::text(text)],
            origin: Origin::default(),
        },
    )
}

pub fn assistant(id: &str, text: &str) -> Item {
    at(
        id,
        ItemBody::Assistant {
            text: text.to_string(),
        },
    )
}

pub fn tool(id: &str, name: &str, input: &str, output: Option<&str>) -> Item {
    at(
        id,
        ItemBody::ToolCall {
            call_id: format!("call_{id}"),
            name: name.to_string(),
            input: serde_json::from_str(input).unwrap_or(serde_json::Value::Null),
            output: output.map(ToolOutput::text),
            progress: None,
            child_session: None,
            duration_ms: None,
        },
    )
}

/// The same item, in another round of the same turn.
pub fn in_round(mut item: Item, round: u32) -> Item {
    item.round = round;
    item
}

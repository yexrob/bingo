//! What this adapter posted, and how to reach it again.
//!
//! Two things come back from a send: a CardKit entity a stream is written
//! into, and a message that can be edited in place. `Posted` is one opaque
//! string to everybody else — that is the point of it — so the two live
//! behind one spelling, here and nowhere else.

use crate::conversation::Posted;

const CARD: &str = "card:";
const MESSAGE: &str = "message:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Handle {
    /// A CardKit entity, by `card_id`: streamed into, and never editable
    /// through `im/v1`, which silently does nothing to it.
    Card(String),
    /// A message, by `message_id`: editable in place.
    Message(String),
}

impl Handle {
    pub fn posted(&self) -> Posted {
        match self {
            Handle::Card(id) => Posted::new(format!("{CARD}{id}")),
            Handle::Message(id) => Posted::new(format!("{MESSAGE}{id}")),
        }
    }

    pub fn of(posted: &Posted) -> Option<Self> {
        let raw = posted.as_str();
        raw.strip_prefix(CARD)
            .map(|id| Handle::Card(id.to_string()))
            .or_else(|| {
                raw.strip_prefix(MESSAGE)
                    .map(|id| Handle::Message(id.to_string()))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_kinds_round_trip_and_nothing_else_parses() {
        for handle in [Handle::Card("ctp_1".into()), Handle::Message("om_1".into())] {
            assert_eq!(Handle::of(&handle.posted()), Some(handle));
        }
        assert_eq!(Handle::of(&Posted::new("om_1")), None);
    }
}

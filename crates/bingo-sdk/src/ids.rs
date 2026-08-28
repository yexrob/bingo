//! Identifiers. Session, turn, item and interaction ids are minted once by the
//! session actor and persisted; intent ids are minted by clients and double as
//! idempotency keys. All are prefixed ULIDs, so they sort by time within a kind.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

macro_rules! id {
    ($name:ident, $prefix:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(
            Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
        )]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Mint a fresh, time-ordered id.
            pub fn mint() -> Self {
                Self(format!("{}_{}", $prefix, ulid::Ulid::generate()))
            }

            /// Wrap an id that already exists (replay, wire, tests).
            pub fn from_raw(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

id!(
    SessionId,
    "ses",
    "One persisted context: one journal, one actor."
);
id!(
    TurnId,
    "trn",
    "One run of the turn loop, closed exactly once."
);
id!(ItemId, "itm", "One unit in the transcript order.");
id!(
    InteractionId,
    "int",
    "A pending question or permission, answered once by id."
);
id!(
    IntentId,
    "req",
    "A client-minted write id; the outcome arrives as `IntentAck`."
);

/// Position of a frame in a session's journal. Durable frames are gapless;
/// clients only require monotonic order.
#[derive(
    Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, Default,
)]
#[serde(transparent)]
pub struct Seq(pub u64);

impl Seq {
    pub const ZERO: Seq = Seq(0);

    pub fn next(self) -> Seq {
        Seq(self.0 + 1)
    }
}

impl fmt::Debug for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_prefixed_and_time_ordered() {
        let a = SessionId::mint();
        let b = SessionId::mint();
        assert!(a.as_str().starts_with("ses_"));
        assert!(a <= b);
    }

    #[test]
    fn ids_round_trip_as_plain_strings() {
        let id = ItemId::from_raw("itm_01ARZ3NDEKTSV4RRFFQ69G5FAV");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"itm_01ARZ3NDEKTSV4RRFFQ69G5FAV\"");
        assert_eq!(serde_json::from_str::<ItemId>(&json).unwrap(), id);
    }
}

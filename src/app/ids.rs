//! Server-owned opaque resource identifiers.
//!
//! Every identifier a client sees is minted by the server, opaque, non-empty, and
//! unique within its resource type and one server epoch (spec "Lifecycle and
//! ordering invariants" #2). Clients never choose one. The wire form is a bare
//! JSON string; the prefix below is the shape the minting side (B2) writes and
//! the only thing this module promises about the interior — a client that parses
//! past it is reading an implementation detail.
// The contract lands before its caller: the actor that mints these (B2) is the
// first consumer. Remove this allow when it arrives.
#![allow(dead_code)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Wall-clock instant, milliseconds since the Unix epoch. Stamped by the actor
/// when it sequences the event, not when a transport serializes it.
pub type UnixMillis = u64;

macro_rules! opaque_ids {
    ($( $(#[$meta:meta])* $name:ident => $prefix:literal ),+ $(,)?) => {
        $(
            $(#[$meta])*
            #[derive(
                Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
            )]
            pub struct $name(pub String);

            impl $name {
                /// The prefix the server mints this identifier with.
                pub const PREFIX: &'static str = $prefix;

                pub fn new(value: impl Into<String>) -> Self {
                    Self(value.into())
                }

                pub fn as_str(&self) -> &str {
                    &self.0
                }
            }

            impl std::fmt::Display for $name {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str(&self.0)
                }
            }

            impl From<&str> for $name {
                fn from(value: &str) -> Self {
                    Self(value.to_string())
                }
            }
        )+

        /// Every identifier type and its prefix, for the drift guard below and
        /// for the minting side to read rather than restate.
        pub const ID_PREFIXES: &[(&str, &str)] = &[$((stringify!($name), $prefix)),+];
    };
}

opaque_ids! {
    /// One run of the server process. Every other identifier is scoped to it;
    /// a restart invalidates them all.
    EpochId => "epoch_",
    /// The open session. Immutable within an epoch — a rename moves the storage
    /// locator and the display name, not this.
    SessionId => "sess_",
    ConversationId => "conv_",
    TurnId => "turn_",
    ItemId => "item_",
    InteractionId => "int_",
    OperationId => "op_",
    AssetId => "asset_",
    /// One entry in a conversation's input queue.
    QueueId => "queue_",
    AgentId => "agent_",
    RoomId => "room_",
    TaskId => "task_",
    /// One direct message's delivery record.
    DeliveryId => "dm_",
    /// A background (promoted or detached) command run.
    CommandId => "cmd_",
    /// A permission rule the server derived and would install for the session.
    /// Handed out with the prompt so `allowSession` names a scope the server
    /// verified rather than one the client composed.
    ScopeId => "scope_",
    /// One raised warning/notice, so it can be cleared by identity.
    FeedbackId => "fb_",
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifier_prefixes_are_unique_and_well_formed() {
        let mut seen: Vec<&str> = Vec::new();
        for (name, prefix) in ID_PREFIXES {
            assert!(
                prefix.ends_with('_') && prefix.len() > 1,
                "{name}: prefix {prefix:?} must be a non-empty lowercase stem ending in '_'"
            );
            assert!(
                prefix.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{name}: prefix {prefix:?} must be lowercase ascii"
            );
            assert!(
                !seen.contains(prefix),
                "{name}: prefix {prefix:?} is already taken; two resource types sharing a \
                 prefix make a mistyped id look valid"
            );
            seen.push(prefix);
        }
        assert_eq!(
            seen.len(),
            16,
            "identifier types are a contract; adding one is a decision"
        );
    }

    #[test]
    fn identifiers_are_bare_strings_on_the_wire() {
        let id = ConversationId::new("conv_main");
        assert_eq!(
            serde_json::to_value(&id).unwrap_or_else(|error| panic!("{error}")),
            serde_json::json!("conv_main")
        );
        let decoded: ConversationId = serde_json::from_value(serde_json::json!("conv_main"))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(decoded, id);
        assert_eq!(id.to_string(), "conv_main");
    }
}

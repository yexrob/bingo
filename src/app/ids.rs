//! Server-owned opaque resource identifiers, and the mint that issues them.
//!
//! Every identifier a client sees is minted by the server, opaque, non-empty, and
//! unique within its resource type and one server epoch (spec "Lifecycle and
//! ordering invariants" #2). Clients never choose one. The wire form is a bare
//! JSON string; the prefix below is the shape [`IdMint`] writes and the only
//! thing this module promises about the interior — a client that parses past it
//! is reading an implementation detail.
// Most identifier types are contract ahead of their first minting site: the
// resources they name land with B3/B4. Remove this allow when they arrive.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Wall-clock instant, milliseconds since the Unix epoch. Stamped by the actor
/// when it sequences the event, not when a transport serializes it.
pub type UnixMillis = u64;

/// Now, in the one unit every timestamp on the wire uses. A clock before the
/// Unix epoch reads as the epoch rather than failing: a timestamp is annotation,
/// and no event is worth losing to a misconfigured system clock.
pub fn now_millis() -> UnixMillis {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            since.as_millis().min(u128::from(u64::MAX)) as u64
        })
}

/// A minted identifier type: what it is prefixed with, and how a minted body
/// becomes one. Implemented for every type below, so [`IdMint`] can issue any of
/// them without a method per resource.
pub trait OpaqueId: Sized {
    const PREFIX: &'static str;

    fn from_minted(body: String) -> Self;
}

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

            impl OpaqueId for $name {
                const PREFIX: &'static str = $prefix;

                fn from_minted(body: String) -> Self {
                    Self(body)
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

impl EpochId {
    /// A fresh epoch. One per running core, distinct from every other epoch this
    /// process minted: two cores alive at once must not hand out the same
    /// identifier space.
    pub fn mint() -> Self {
        static ORDINAL: AtomicU64 = AtomicU64::new(0);
        let ordinal = ORDINAL.fetch_add(1, Ordering::Relaxed);
        Self(format!("{}{}_{ordinal}", Self::PREFIX, now_millis()))
    }
}

/// The identifier mint: one epoch, one counter per resource type.
///
/// It lives inside the actor and is used from nowhere else. An identifier states
/// the order a resource was created in, so it comes from the same single place
/// the event sequence does — a second minting site would be a second ordering.
///
/// Identifiers carry no epoch stamp of their own: the protocol scopes every
/// identifier to the epoch it advertises and a restart invalidates all of them
/// at once (spec "Snapshots and recovery"), so repeating the epoch in every
/// identifier of every frame would pay for a guard the connection already owes
/// once at `initialize`.
#[derive(Debug)]
pub struct IdMint {
    epoch: EpochId,
    counters: HashMap<&'static str, u64>,
}

impl IdMint {
    pub fn new(epoch: EpochId) -> Self {
        Self {
            epoch,
            counters: HashMap::new(),
        }
    }

    pub fn epoch(&self) -> &EpochId {
        &self.epoch
    }

    /// The next identifier of one type. Counters are per type, so `conv_1` and
    /// `turn_1` coexist and neither is ever reused within the epoch.
    pub fn mint<T: OpaqueId>(&mut self) -> T {
        let counter = self.counters.entry(T::PREFIX).or_insert(0);
        *counter = counter.saturating_add(1);
        T::from_minted(format!("{}{counter}", T::PREFIX))
    }
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
    fn minted_identifiers_are_unique_within_a_type_and_independent_across_types() {
        let mut mint = IdMint::new(EpochId::mint());
        let conversations: Vec<ConversationId> = (0..3).map(|_| mint.mint()).collect();
        let turns: Vec<TurnId> = (0..3).map(|_| mint.mint()).collect();

        assert_eq!(
            conversations,
            vec![
                ConversationId::new("conv_1"),
                ConversationId::new("conv_2"),
                ConversationId::new("conv_3"),
            ]
        );
        assert_eq!(
            turns,
            vec![
                TurnId::new("turn_1"),
                TurnId::new("turn_2"),
                TurnId::new("turn_3"),
            ],
            "one counter per type: a turn does not consume a conversation's number"
        );
        for id in &conversations {
            assert!(id.as_str().starts_with(ConversationId::PREFIX));
        }
    }

    #[test]
    fn every_epoch_is_its_own_identifier_space() {
        let first = EpochId::mint();
        let second = EpochId::mint();
        assert_ne!(
            first, second,
            "two cores in one process must not share an epoch, whatever the clock says"
        );
        assert!(first.as_str().starts_with(EpochId::PREFIX));
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

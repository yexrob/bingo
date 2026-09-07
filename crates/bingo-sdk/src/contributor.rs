//! How features enter the prompt. The loop asks contributors at three
//! placements; everything the old loop hard-coded (inbox, reminders,
//! notifications, norms, recall) is one of these.

use std::path::Path;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::{ContextUsage, Item, ItemBody, SessionSummary};
use crate::host::HostHandle;
use crate::ids::TurnId;
use crate::model::{ContentPart, ModelCapabilities, SystemBlock};

/// When a contributor speaks. Serializable because a contributor may live in
/// another process: it declares its placement once, over the bridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Placement {
    /// A system block, recomputed per request; lower `order` first.
    System { order: i32 },
    /// A user piece at the start of each round.
    RoundStart,
    /// A user piece after tool results, before the next request.
    Barrier,
}

#[derive(Clone, Copy, Debug)]
pub struct ContextQuery<'a> {
    pub session: &'a SessionSummary,
    /// The whole host (ADR-0011 §3): a contributor that reads a session's
    /// extensions, or another session, reaches it here.
    pub host: &'a HostHandle,
    pub turn: &'a TurnId,
    pub round: u32,
    pub items: &'a [Item],
    pub usage: &'a ContextUsage,
    pub capabilities: &'a ModelCapabilities,
    pub cwd: &'a Path,
}

/// What a contributor adds. Serializable for the same reason a placement is:
/// a piece written in another process crosses as it is, never as a copy of
/// itself the bridge invented.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ContextPiece {
    System(SystemBlock),
    /// Recorded as a user item with `Origin { surface: "contributor:<id>" }`, so
    /// the transcript and the provider cache prefix agree.
    User {
        parts: Vec<ContentPart>,
        label: String,
    },
}

/// The journal origin of a contributor's user pieces, followed by its id.
pub const CONTRIBUTOR_PREFIX: &str = "contributor:";

impl ContextPiece {
    /// Append changed state without rewriting the system prompt or history.
    /// Use one snapshot per contributor id; `items` is the visible journal,
    /// so a removed snapshot is naturally reissued after compaction or rewind.
    /// Clearing state must render an explicit empty-state message.
    pub fn snapshot(id: &str, text: impl Into<String>, items: &[Item]) -> Option<Self> {
        let parts = vec![ContentPart::text(text)];
        let previous = items.iter().rev().find_map(|item| match &item.body {
            ItemBody::User { parts, origin }
                if origin.surface.strip_prefix(CONTRIBUTOR_PREFIX) == Some(id) =>
            {
                Some(parts)
            }
            _ => None,
        });
        (previous != Some(&parts)).then(|| Self::User {
            parts,
            label: id.to_string(),
        })
    }
}

#[async_trait]
pub trait ContextContributor: Send + Sync {
    fn id(&self) -> &str;

    fn placement(&self) -> Placement;

    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError>;
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ContextError(pub String);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ItemBody, ItemId, ItemStatus, Origin};

    fn recorded(id: &str, text: &str) -> Item {
        Item {
            id: ItemId::from_raw("itm_snapshot"),
            turn: None,
            round: 0,
            status: ItemStatus::Completed,
            started_at: jiff::Timestamp::UNIX_EPOCH,
            completed_at: None,
            intent: None,
            body: ItemBody::User {
                parts: vec![ContentPart::text(text)],
                origin: Origin::surface(format!("contributor:{id}")),
            },
            meta: Default::default(),
        }
    }

    #[test]
    fn a_first_snapshot_uses_the_existing_user_piece_wire_form() {
        let piece = ContextPiece::snapshot("inventory", "Two entries.", &[]).unwrap();
        assert_eq!(
            serde_json::to_value(piece).unwrap(),
            serde_json::json!({
                "kind": "user",
                "parts": [{"type": "text", "text": "Two entries."}],
                "label": "inventory"
            })
        );
    }

    #[test]
    fn an_unchanged_snapshot_is_silent_even_after_a_journal_round_trip() {
        let items = vec![recorded("inventory", "Two entries.")];
        let restored: Vec<Item> =
            serde_json::from_str(&serde_json::to_string(&items).unwrap()).unwrap();
        assert_eq!(
            ContextPiece::snapshot("inventory", "Two entries.", &restored),
            None
        );
    }

    #[test]
    fn a_changed_snapshot_compares_with_the_latest_visible_one() {
        let items = vec![
            recorded("inventory", "Two entries."),
            recorded("inventory", "No entries."),
            recorded("elsewhere", "Two entries."),
        ];
        assert!(ContextPiece::snapshot("inventory", "Two entries.", &items).is_some());
        assert_eq!(
            ContextPiece::snapshot("inventory", "No entries.", &items),
            None
        );
        assert_eq!(items[0], recorded("inventory", "Two entries."));
    }

    #[test]
    fn a_snapshot_is_reissued_when_its_record_left_visible_context() {
        assert!(ContextPiece::snapshot("inventory", "No entries.", &[]).is_some());
    }
}

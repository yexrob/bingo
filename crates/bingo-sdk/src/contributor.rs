//! How features enter the prompt. The loop asks contributors at three
//! placements; everything the old loop hard-coded (inbox, reminders,
//! notifications, norms, recall) is one of these.

use std::path::Path;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::{ContextUsage, Item, SessionSummary};
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

#[async_trait]
pub trait ContextContributor: Send + Sync {
    fn id(&self) -> &str;

    fn placement(&self) -> Placement;

    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError>;
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ContextError(pub String);

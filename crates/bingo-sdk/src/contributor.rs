//! How features enter the prompt. The loop asks contributors at three
//! placements; everything the old loop hard-coded (inbox, reminders,
//! notifications, norms, recall) is one of these.

use std::path::Path;

use async_trait::async_trait;

use crate::event::{ContextUsage, Item, SessionSummary};
use crate::ids::TurnId;
use crate::model::{ContentPart, ModelCapabilities, SystemBlock};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    pub turn: &'a TurnId,
    pub round: u32,
    pub items: &'a [Item],
    pub usage: &'a ContextUsage,
    pub capabilities: &'a ModelCapabilities,
    pub cwd: &'a Path,
}

#[derive(Clone, Debug, PartialEq)]
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

//! Compaction strategy. The kernel owns the ruler and the breaker; the
//! plugin owns the summary.

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::error::KernelError;
use crate::event::{ContextUsage, Item};
use crate::ids::ItemId;
use crate::model::ModelCapabilities;
use crate::provider::Provider;

#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum CompactReason {
    Threshold,
    Overflow { message: String },
    Manual { instructions: Option<String> },
}

pub struct CompactContext<'a> {
    pub items: &'a [Item],
    pub usage: ContextUsage,
    pub capabilities: &'a ModelCapabilities,
    pub provider: Arc<dyn Provider>,
    pub model: &'a str,
    pub cancel: CancellationToken,
}

impl std::fmt::Debug for CompactContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompactContext")
            .field("items", &self.items.len())
            .field("usage", &self.usage)
            .finish_non_exhaustive()
    }
}

/// The result: a summary, the boundary before which items are replaced, and
/// the items before it to keep anyway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compaction {
    pub summary: String,
    pub boundary: ItemId,
    pub kept: Vec<ItemId>,
    pub before: u64,
    pub after: u64,
}

#[async_trait]
pub trait Compactor: Send + Sync {
    /// Used tokens at which the loop calls `compact` with `Threshold`.
    fn threshold(&self, capabilities: &ModelCapabilities) -> u64;

    async fn compact(
        &self,
        cx: CompactContext<'_>,
        reason: CompactReason,
    ) -> Result<Compaction, KernelError>;
}

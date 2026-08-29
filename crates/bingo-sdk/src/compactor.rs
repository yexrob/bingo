//! Compaction strategy. The kernel owns the ruler, the thresholds and the
//! breaker; the plugin owns the summary (ADR-0006).

use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::error::KernelError;
use crate::event::{ContextUsage, Item};
use crate::ids::ItemId;
use crate::model::{ModelCapabilities, Usage};
use crate::provider::Provider;

#[derive(Clone, Debug, PartialEq, Eq)]
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
    /// Consecutive compactions the kernel discarded; at three the breaker is
    /// tripped and a strategy takes its rung that needs no model.
    pub failures: u32,
    /// Tokens of the newest items a cut should leave intact (a quarter of
    /// the effective window).
    pub keep_budget: u64,
}

impl std::fmt::Debug for CompactContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompactContext")
            .field("items", &self.items.len())
            .field("usage", &self.usage)
            .finish_non_exhaustive()
    }
}

/// The result: a summary, the boundary before which items are replaced, the
/// items before it to keep anyway, and what the summary cost. The kernel
/// accepts it only when `after < before`; the cost is billed either way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Compaction {
    pub summary: String,
    pub boundary: ItemId,
    pub kept: Vec<ItemId>,
    pub before: u64,
    pub after: u64,
    pub usage: Usage,
}

#[async_trait]
pub trait Compactor: Send + Sync {
    async fn compact(
        &self,
        cx: CompactContext<'_>,
        reason: CompactReason,
    ) -> Result<Compaction, KernelError>;
}

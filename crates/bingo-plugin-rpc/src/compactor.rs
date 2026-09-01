//! One plugin compaction strategy as a bingo one.
//!
//! The kernel owns the ruler, the thresholds and the breaker; a strategy owns
//! the summary (ADR-0006), and this one owns it in another process. The struct
//! implements the sdk's own `Compactor` and its `compact` is a wire call
//! (ADR-0030 §1).
//!
//! What crosses is the projection: the provider the in-process context carries
//! stays on this side, so a remote strategy summarises by its own means or
//! cuts by none. Past the deadline the call fails with the error the trait
//! already speaks, and the kernel's breaker counts it like any other failure.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{CompactContext, CompactReason, Compaction, Compactor, ErrorCode, KernelError};

use crate::connection::Connection;
use crate::deadline;
use crate::wire::{
    CompactorCompactParams, CompactorCompactResult, CompactorContext, CompactorSpec, name,
};

/// A compaction strategy a plugin process declared, bound to the pipe that
/// answers it.
pub struct RemoteCompactor {
    plugin: String,
    spec: CompactorSpec,
    connection: Arc<Connection>,
}

impl RemoteCompactor {
    pub fn new(plugin: &str, spec: CompactorSpec, connection: Arc<Connection>) -> Self {
        Self {
            plugin: plugin.to_string(),
            spec,
            connection,
        }
    }

    fn params(&self, cx: &CompactContext<'_>, reason: CompactReason) -> CompactorCompactParams {
        CompactorCompactParams {
            id: self.spec.id.clone(),
            context: CompactorContext::from(cx),
            reason,
        }
    }

    async fn ask(&self, params: CompactorCompactParams) -> Result<Compaction, KernelError> {
        let value = serde_json::to_value(params)
            .map_err(|e| self.failed(ErrorCode::Internal, e.to_string()))?;
        let answered = tokio::time::timeout(
            deadline::COMPACT,
            self.connection.request(name::COMPACTOR_COMPACT, value),
        )
        .await;
        match answered {
            Ok(Ok(value)) => serde_json::from_value::<CompactorCompactResult>(value)
                .map(|result| result.compaction)
                .map_err(|e| self.failed(ErrorCode::Internal, e.to_string())),
            Ok(Err(error)) => Err(self.failed(ErrorCode::Internal, error.message)),
            Err(_) => Err(self.failed(
                ErrorCode::Timeout,
                format!("no compaction within {}s", deadline::COMPACT.as_secs()),
            )),
        }
    }

    fn failed(&self, code: ErrorCode, why: String) -> KernelError {
        KernelError::new(code, format!("{}: {why}", self.plugin))
    }
}

#[async_trait]
impl Compactor for RemoteCompactor {
    async fn compact(
        &self,
        cx: CompactContext<'_>,
        reason: CompactReason,
    ) -> Result<Compaction, KernelError> {
        self.ask(self.params(&cx, reason)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{capabilities, unanswering};

    fn spec() -> CompactorSpec {
        CompactorSpec { id: "cut".into() }
    }

    /// The deadline on a clock that does not tick: the process is alive and
    /// says nothing, and the call fails with the error the trait speaks.
    #[tokio::test(start_paused = true)]
    async fn a_compactor_past_its_deadline_fails_the_call() {
        let remote = RemoteCompactor::new("slow", spec(), unanswering());
        let error = remote
            .compact(
                CompactContext {
                    items: &[],
                    usage: Default::default(),
                    capabilities: &capabilities(),
                    provider: Arc::new(crate::testing::NoProvider),
                    model: "m",
                    cancel: Default::default(),
                    failures: 0,
                    keep_budget: 100,
                },
                CompactReason::Threshold,
            )
            .await
            .expect_err("a process that says nothing compacts nothing");
        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(error.message.starts_with("slow: "), "{error}");
        assert!(error.message.contains("within 60s"), "{error}");
    }
}

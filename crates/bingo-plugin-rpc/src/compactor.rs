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
use bingo_sdk::{
    CompactContext, CompactError, CompactReason, Compaction, Compactor, ErrorCode, KernelError,
};

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

    async fn ask(&self, params: CompactorCompactParams) -> Result<Compaction, CompactError> {
        let value = serde_json::to_value(params)
            .map_err(|e| self.failed(ErrorCode::Internal, e.to_string()))?;
        let answered = tokio::time::timeout(
            deadline::COMPACT,
            self.connection.request(name::COMPACTOR_COMPACT, value),
        )
        .await;
        match answered {
            Ok(Ok(value)) => {
                let result = serde_json::from_value::<CompactorCompactResult>(value)
                    .map_err(|e| self.failed(ErrorCode::Internal, e.to_string()))?;
                match result {
                    CompactorCompactResult::Completed { compaction } => Ok(compaction),
                    CompactorCompactResult::Failed { error } => Err(error),
                }
            }
            Ok(Err(error)) => Err(self.failed(ErrorCode::Internal, error.message).into()),
            Err(_) => Err(self
                .failed(
                    ErrorCode::Timeout,
                    format!("no compaction within {}s", deadline::COMPACT.as_secs()),
                )
                .into()),
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
    ) -> Result<Compaction, CompactError> {
        self.ask(self.params(&cx, reason)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{capabilities, unanswering};

    #[test]
    fn the_remote_context_carries_the_native_parent_request() {
        let request: bingo_sdk::ModelRequest = serde_json::from_value(serde_json::json!({
            "model": "m", "maxTokens": 500, "system": [], "messages": [],
            "tools": [], "session": "ses_parent",
            "providerOptions": {"openai": {"seed": 7}}
        }))
        .expect("native request fixture");
        let cx = CompactContext {
            items: &[],
            usage: Default::default(),
            capabilities: &capabilities(),
            provider: Arc::new(crate::testing::NoProvider),
            request: &request,
            cancel: Default::default(),
            failures: 2,
            keep_budget: 100,
        };
        let value = serde_json::to_value(CompactorContext::from(&cx)).expect("wire context");
        assert_eq!(
            value["request"],
            serde_json::to_value(request).expect("request")
        );
        assert!(value.get("model").is_none());
        assert!(value.get("provider").is_none());
    }

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
                    request: &serde_json::from_value(serde_json::json!({"model":"m", "maxTokens":100, "system":[], "messages":[], "tools":[]})).expect("request"),
                    cancel: Default::default(),
                    failures: 0,
                    keep_budget: 100,
                },
                CompactReason::Threshold,
            )
            .await
            .expect_err("a process that says nothing compacts nothing");
        assert_eq!(error.error.code, ErrorCode::Timeout);
        assert_eq!(
            error.usage,
            bingo_sdk::Usage::default(),
            "transport timeout has no observed remote usage"
        );
        assert!(error.error.message.starts_with("slow: "), "{error}");
        assert!(error.error.message.contains("within 60s"), "{error}");
    }
}

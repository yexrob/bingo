//! What ADR-0030 opened first: a round's pieces and a compaction, written in
//! another process.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    CancellationToken, CompactContext, CompactReason, ContextPiece, ContextQuery,
    EndpointCapabilities, ModelCapabilities, ModelRequest, ModelStream, Placement, Provider,
    ProviderError, SessionSummary, TurnId,
};
use serde_json::json;

use crate::harness::started;

/// What a `ContextQuery` borrows; the query itself is built where it is asked.
fn round() -> (SessionSummary, TurnId, ModelCapabilities) {
    let summary = serde_json::from_value(json!({
        "id": "ses_test",
        "cwd": "/work",
        "createdAt": "2026-01-01T00:00:00Z",
        "updatedAt": "2026-01-01T00:00:00Z",
    }))
    .expect("a session summary");
    (
        summary,
        TurnId::from_raw("trn_test"),
        ModelCapabilities {
            context_window: 200_000,
            max_output: 8_000,
            images: false,
            reasoning: false,
            count_tokens: false,
            caching: false,
        },
    )
}

/// The model a remote strategy never asks: it summarises on its own side, and
/// a `CompactContext` still has to carry a provider.
struct NoProvider;

#[async_trait]
impl Provider for NoProvider {
    fn id(&self) -> &str {
        "none"
    }
    fn endpoint(&self, _model: &str) -> EndpointCapabilities {
        EndpointCapabilities::default()
    }
    async fn stream(
        &self,
        _request: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<ModelStream, ProviderError> {
        Err(ProviderError::Unsupported {
            message: "this test has no model".into(),
        })
    }
}

/// The exit criterion of ADR-0030 for context: a process the kernel knows
/// nothing about contributes a piece, and the piece is a user item with the
/// origin the contributor's id earns it (`contributor:stub:notes`).
#[tokio::test]
async fn a_plugin_s_contributor_speaks_at_the_placement_it_declared() {
    let (manager, _home, project) = started(&[]).await;
    let mut contributors = manager.contributors().await;
    assert_eq!(contributors.len(), 1, "the stub declares one contributor");
    let contributor = contributors.remove(0);
    assert_eq!(contributor.id(), "stub:notes");
    assert_eq!(contributor.placement(), Placement::RoundStart);

    let (session, turn, capabilities) = round();
    let host = bingo_sdk::testing::NoHost::handle();
    let pieces = contributor
        .contribute(ContextQuery {
            session: &session,
            host: &host,
            turn: &turn,
            round: 3,
            items: &[],
            usage: &Default::default(),
            capabilities: &capabilities,
            cwd: project.path(),
        })
        .await
        .expect("the contributor answered");
    let ContextPiece::User { parts, label } = &pieces[0] else {
        panic!("the stub contributes a user piece: {pieces:?}");
    };
    assert_eq!(label, "notes");
    assert_eq!(
        parts[0].as_text(),
        Some("notes: round 3 of ses_test with 0 items"),
        "the query's projection crossed whole"
    );
    manager.shutdown().await;
}

/// And for compaction: the summary is written in the other process.
#[tokio::test]
async fn a_plugin_s_compaction_strategy_answers_a_compaction() {
    let (manager, _home, _project) = started(&[]).await;
    let mut compactors = manager.compactors().await;
    assert_eq!(compactors.len(), 1, "the stub declares one strategy");
    let compactor = compactors.remove(0);
    let (_, _, capabilities) = round();
    let compaction = compactor
        .compact(
            CompactContext {
                items: &[],
                usage: bingo_sdk::ContextUsage {
                    used: 900,
                    window: 1_000,
                    trigger: 800,
                },
                capabilities: &capabilities,
                provider: Arc::new(NoProvider),
                model: "m",
                cancel: CancellationToken::new(),
                failures: 0,
                keep_budget: 250,
            },
            CompactReason::Threshold,
        )
        .await
        .expect("the strategy answered");
    assert_eq!(compaction.summary, "cut cut on threshold");
    assert_eq!((compaction.before, compaction.after), (900, 450));
    manager.shutdown().await;
}

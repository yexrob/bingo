//! Doubles this crate's own tests share: a live process that never answers,
//! and the facts a query has to carry to be one.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    CancellationToken, EndpointCapabilities, HostHandle, ModelCapabilities, ModelRequest,
    ModelStream, Provider, ProviderError, SessionSummary, TurnId,
};

use crate::connection::Connection;
use crate::manifest::Entry;

/// A process that is alive, reads what it is sent and answers nothing: every
/// request on this connection waits for its deadline and for nothing else.
/// `cat` buffers a pipe rather than echoing it, and a line echoed back would
/// be a request the router ignores anyway.
pub fn unanswering() -> Arc<Connection> {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let entry = Entry {
        command: "cat".into(),
        args: Vec::new(),
        env: Default::default(),
    };
    let connection = Connection::spawn("quiet", &entry, dir.path(), dir.path(), None)
        .expect("`cat` exists on every unix");
    Arc::new(connection)
}

/// What a `ContextQuery` borrows. The query itself is built by the caller:
/// it holds references, so its parts have to outlive it there.
pub fn query() -> (SessionSummary, TurnId, HostHandle) {
    let summary = serde_json::from_value(serde_json::json!({
        "id": "ses_1",
        "cwd": "/work",
        "createdAt": "2026-01-01T00:00:00Z",
        "updatedAt": "2026-01-01T00:00:00Z",
    }))
    .expect("a session summary");
    (
        summary,
        TurnId::from_raw("trn_1"),
        bingo_sdk::testing::NoHost::handle(),
    )
}

/// The model a remote compaction never asks: the strategy is on the other
/// side of the pipe, and a `CompactContext` still has to carry a provider.
pub struct NoProvider;

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

pub fn capabilities() -> ModelCapabilities {
    ModelCapabilities {
        context_window: 200_000,
        max_output: 8_000,
        images: false,
        reasoning: false,
        count_tokens: false,
        caching: false,
    }
}

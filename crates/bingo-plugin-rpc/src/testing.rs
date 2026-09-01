//! Doubles this crate's own tests share: a live process that never answers,
//! and the facts a query has to carry to be one.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Answer, AnswerSpec, CancellationToken, EndpointCapabilities, HostHandle, InteractionKind,
    ItemBody, ItemId, KernelError, ModelCapabilities, ModelRequest, ModelStream, Prompter,
    Provider, ProviderError, SessionSummary, ToolHost, TurnId,
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

/// Where a hook is standing, for the tests that only need it to be somewhere:
/// one session, one turn, one directory, and nothing that answers.
pub fn hook_context() -> bingo_sdk::HookContext {
    bingo_sdk::HookContext {
        session: bingo_sdk::SessionId::from_raw("ses_1"),
        turn: Some(TurnId::from_raw("trn_1")),
        cwd: "/work".into(),
        provider: None,
        model: Some("stub-1".into()),
        host: bingo_sdk::testing::NoHost::handle(),
    }
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

/// A call that is running and nothing more: it takes a progress line, records
/// nothing, and answers every question the same way. What a test files as
/// running when what it is testing is the filing, not the answering.
pub struct Silent(pub Answer);

impl Silent {
    /// A call whose person always cancels.
    pub fn cancelling() -> Arc<dyn ToolHost> {
        Arc::new(Silent(Answer::Cancel))
    }
}

#[async_trait]
impl Prompter for Silent {
    async fn ask(&self, _: InteractionKind, _: Vec<AnswerSpec>) -> Result<Answer, KernelError> {
        Ok(self.0.clone())
    }
}

#[async_trait]
impl ToolHost for Silent {
    fn progress(&self, _item: &ItemId, _tail: String) {}

    async fn record(&self, _body: ItemBody) -> Result<ItemId, KernelError> {
        Ok(ItemId::from_raw("itm_silent"))
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

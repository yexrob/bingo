//! One plugin contributor as a bingo contributor.
//!
//! The kernel keeps seeing `Arc<dyn ContextContributor>` and never learns
//! which of them are processes: this struct implements the sdk's own trait and
//! its `contribute` is a wire call (ADR-0030 §1). N remote contributors are N
//! of these, differing by the handshake data they were built from.
//!
//! `contribute` runs on every round, so the wait is bounded. Past the deadline
//! the round goes on without this contributor's pieces, and the error the
//! trait already speaks carries whose deadline was missed — the kernel turns
//! it into the `CONTRIBUTOR_FAILED` notice it turns every other one into.

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{ContextContributor, ContextError, ContextPiece, ContextQuery, Placement};

use crate::connection::Connection;
use crate::deadline;
use crate::wire::{
    ContextContributeParams, ContextContributeResult, ContributeQuery, ContributorSpec, name,
};

/// The kernel-visible id of a plugin's contributor: the plugin's name and the
/// contributor's own. Two plugins may both declare a `notes` contributor, and
/// a transcript's `contributor:<id>` origin still says which one wrote.
pub fn contributor_id(plugin: &str, contributor: &str) -> String {
    format!("{plugin}:{contributor}")
}

/// A contributor a plugin process declared, bound to the pipe that answers it.
pub struct RemoteContributor {
    plugin: String,
    /// The id the kernel sees; the process is asked by [`ContributorSpec::id`].
    id: String,
    spec: ContributorSpec,
    connection: Arc<Connection>,
}

impl RemoteContributor {
    pub fn new(plugin: &str, spec: ContributorSpec, connection: Arc<Connection>) -> Self {
        Self {
            plugin: plugin.to_string(),
            id: contributor_id(plugin, &spec.id),
            spec,
            connection,
        }
    }

    fn params(&self, query: ContextQuery<'_>) -> ContextContributeParams {
        ContextContributeParams {
            id: self.spec.id.clone(),
            query: ContributeQuery::from(query),
        }
    }

    /// The pieces, or why there are none. A process that answers late is
    /// reported like one that answers badly: the round is what matters, and
    /// it has already gone on.
    async fn ask(
        &self,
        params: ContextContributeParams,
    ) -> Result<Vec<ContextPiece>, ContextError> {
        let value = serde_json::to_value(params).map_err(|e| self.failed(e.to_string()))?;
        let answered = tokio::time::timeout(
            deadline::CONTRIBUTE,
            self.connection.request(name::CONTEXT_CONTRIBUTE, value),
        )
        .await;
        match answered {
            Ok(Ok(value)) => serde_json::from_value::<ContextContributeResult>(value)
                .map(|result| result.pieces)
                .map_err(|e| self.failed(e.to_string())),
            Ok(Err(error)) => Err(self.failed(error.message)),
            Err(_) => Err(self.failed(format!(
                "nothing within {}s; the round went on without it",
                deadline::CONTRIBUTE.as_secs()
            ))),
        }
    }

    fn failed(&self, why: String) -> ContextError {
        ContextError(format!("{}: {why}", self.plugin))
    }
}

#[async_trait]
impl ContextContributor for RemoteContributor {
    fn id(&self) -> &str {
        &self.id
    }

    /// Handshake data: asked once, when the process said what it has.
    fn placement(&self) -> Placement {
        self.spec.placement
    }

    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        self.ask(self.params(query)).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{query, unanswering};

    #[test]
    fn a_contributor_is_named_for_its_plugin_and_itself() {
        assert_eq!(contributor_id("notes", "recall"), "notes:recall");
    }

    fn declared(placement: Placement) -> ContributorSpec {
        ContributorSpec {
            id: "recall".into(),
            placement,
        }
    }

    #[tokio::test]
    async fn the_placement_is_the_one_the_handshake_declared() {
        let remote = RemoteContributor::new(
            "notes",
            declared(Placement::System { order: 7 }),
            unanswering(),
        );
        assert_eq!(remote.placement(), Placement::System { order: 7 });
        assert_eq!(remote.id(), "notes:recall");
    }

    /// The whole protection of the hot path, on a clock that does not tick:
    /// the process is alive and says nothing, and the round still goes on.
    #[tokio::test(start_paused = true)]
    async fn a_contributor_past_its_deadline_contributes_nothing_and_says_whose() {
        let remote =
            RemoteContributor::new("notes", declared(Placement::RoundStart), unanswering());
        let (session, turn, host) = query();
        let error = remote
            .contribute(ContextQuery {
                session: &session,
                host: &host,
                turn: &turn,
                round: 0,
                items: &[],
                usage: &Default::default(),
                capabilities: &crate::testing::capabilities(),
                cwd: std::path::Path::new("/work"),
            })
            .await
            .expect_err("a process that says nothing contributes nothing");
        let said = error.to_string();
        assert!(said.starts_with("notes: "), "{said}");
        assert!(said.contains("within 3s"), "{said}");
    }
}

//! Journal-owned system blocks, retained until the session or history restarts.

use std::future::Future;
use std::path::PathBuf;

use async_trait::async_trait;
use bingo_sdk::{
    ClientIdentity, ContextError, ContextPiece, ContextQuery, Hook, HookContext, HookMatcher,
    HookPoint, Phase, SessionSelector, SessionState, SystemBlock,
};
use serde::{Deserialize, Serialize};

/// The journal namespace the plugin keeps its state under: the baselines,
/// and what the memory contributor publishes beside them.
pub(crate) const PLUGIN: &str = "_bingo.context";
const CONTRIBUTORS: [&str; 2] = [crate::instructions::ID, crate::memory::ID];

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Baseline {
    history_generation: u64,
    blocks: Vec<SystemBlock>,
}

/// The future is polled only on a cache miss, so retained knowledge never
/// touches the files it came from during ordinary rounds or turns.
pub(crate) async fn contribute(
    id: &str,
    query: ContextQuery<'_>,
    render: impl Future<Output = Vec<SystemBlock>>,
) -> Result<Vec<ContextPiece>, ContextError> {
    let state = query
        .host
        .open(
            SessionSelector::ById {
                id: query.session.id.clone(),
            },
            ClientIdentity {
                name: id.into(),
                surface: PLUGIN.into(),
            },
            Default::default(),
        )
        .await
        .map_err(|error| ContextError(error.to_string()))?
        .snapshot;
    let blocks = match retained(&state, id) {
        Some(blocks) => blocks,
        None => capture(id, query, state.history_generation, render.await).await?,
    };
    Ok(blocks.into_iter().map(ContextPiece::System).collect())
}

async fn capture(
    id: &str,
    query: ContextQuery<'_>,
    history_generation: u64,
    blocks: Vec<SystemBlock>,
) -> Result<Vec<SystemBlock>, ContextError> {
    let baseline = Baseline {
        history_generation,
        blocks,
    };
    let payload =
        serde_json::to_value(&baseline).map_err(|error| ContextError(error.to_string()))?;
    query
        .host
        .extend(&query.session.id, PLUGIN, id, payload)
        .await
        .map_err(|error| ContextError(error.to_string()))?;
    Ok(baseline.blocks)
}

fn retained(state: &SessionState, id: &str) -> Option<Vec<SystemBlock>> {
    let payload = state.extensions.get(PLUGIN)?.get(id)?;
    let baseline: Baseline = serde_json::from_value(payload.clone()).ok()?;
    (baseline.history_generation == state.history_generation).then_some(baseline.blocks)
}

/// Invalidates the baselines when a session starts, and publishes where the
/// session's memories are (M84) — both once per session, both journal
/// state of the plugin's own.
#[derive(Debug)]
pub(crate) struct BaselineHook {
    data_dir: PathBuf,
}

impl BaselineHook {
    pub(crate) fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }
}

#[async_trait]
impl Hook for BaselineHook {
    fn id(&self) -> &str {
        "context:baselines"
    }

    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::Session],
            tool: None,
        }
    }

    async fn on_session(&self, phase: Phase, cx: &HookContext) {
        if phase != Phase::Start {
            return;
        }
        // Opening this same session here would wait on its own startup hook.
        for id in CONTRIBUTORS {
            if let Err(error) = cx
                .host
                .extend(&cx.session, PLUGIN, id, serde_json::Value::Null)
                .await
            {
                tracing::warn!(%error, contributor = id, "context: baseline was not invalidated");
            }
        }
        crate::memory::publish(&cx.host, &cx.session, &self.data_dir, &cx.cwd).await;
    }
}

#[cfg(test)]
mod lifecycle;
#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;

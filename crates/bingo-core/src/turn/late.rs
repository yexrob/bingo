//! What a turn resolves when it starts (ADR-0009 §1): the tools, the
//! contributors and the compaction strategy, each the ones registered up front
//! plus whatever the sources answer with now.
//!
//! This is the one point a late contribution becomes concrete. A kind that
//! learns to arrive after I/O — a bridge's, an MCP server's — is a set here and
//! nowhere else: two resolution points would be two answers to "what does this
//! turn have", which is the debt ADR-0011 forbids.

use std::sync::Arc;

use bingo_sdk::*;

/// The tools a session may call: the ones registered up front, the sources
/// that answer late, and the names the session is limited to.
#[derive(Clone, Default)]
pub struct ToolSet {
    pub fixed: Vec<Arc<dyn Tool>>,
    pub sources: Vec<Arc<dyn ToolSource>>,
    /// `Some` restricts the set to these names; `None` is every tool.
    pub only: Option<Vec<String>>,
}

impl ToolSet {
    pub fn fixed(tools: Vec<Arc<dyn Tool>>) -> Self {
        Self {
            fixed: tools,
            sources: Vec::new(),
            only: None,
        }
    }

    /// The set as it stands now: fixed tools first, then every source in
    /// order; a later tool whose name is taken is dropped and reported.
    pub async fn gather(&self) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        let mut shadowed = Vec::new();
        let mut take = |tool: Arc<dyn Tool>| {
            let name = tool.spec().name;
            if self
                .only
                .as_ref()
                .is_some_and(|names| !names.contains(&name))
            {
                return;
            }
            if tools.iter().any(|t| t.spec().name == name) {
                shadowed.push(name);
            } else {
                tools.push(tool);
            }
        };
        for tool in &self.fixed {
            take(tool.clone());
        }
        for source in &self.sources {
            for tool in source.tools().await {
                take(tool);
            }
        }
        (tools, shadowed)
    }
}

/// The contributors that may speak this turn: the registered ones, then every
/// source's. Two contributors may share an id — they are asked, never looked
/// up by name — so nothing here drops a duplicate.
#[derive(Clone, Default)]
pub struct ContributorSet {
    pub fixed: Vec<Arc<dyn ContextContributor>>,
    pub sources: Vec<Arc<dyn ContextSource>>,
}

impl ContributorSet {
    pub fn fixed(contributors: Vec<Arc<dyn ContextContributor>>) -> Self {
        Self {
            fixed: contributors,
            sources: Vec::new(),
        }
    }

    pub async fn gather(&self) -> Vec<Arc<dyn ContextContributor>> {
        let mut contributors = self.fixed.clone();
        for source in &self.sources {
            contributors.extend(source.contributors().await);
        }
        contributors
    }
}

/// The compaction strategy this turn would use. The slot holds one (ADR-0006
/// leaves the kernel the ruler and a plugin the strategy), so a source's is the
/// turn's only where nothing in-process holds it — the registry's first-wins
/// rule, read where the sources are.
#[derive(Clone, Default)]
pub struct CompactorSet {
    pub fixed: Option<Arc<dyn Compactor>>,
    pub sources: Vec<Arc<dyn CompactorSource>>,
}

impl CompactorSet {
    pub fn fixed(compactor: Option<Arc<dyn Compactor>>) -> Self {
        Self {
            fixed: compactor,
            sources: Vec::new(),
        }
    }

    pub async fn resolve(&self) -> Option<Arc<dyn Compactor>> {
        if let Some(compactor) = &self.fixed {
            self.report_shadowed();
            return Some(Arc::clone(compactor));
        }
        for source in &self.sources {
            if let Some(compactor) = source.compactors().await.into_iter().next() {
                return Some(compactor);
            }
        }
        None
    }

    /// A strategy that will never run is worth a line in the log and not a
    /// notice every turn: nobody at the keyboard can act on it mid-session.
    fn report_shadowed(&self) {
        for source in &self.sources {
            tracing::debug!(
                source = source.id(),
                "a compaction strategy from a source is unused; one is already registered"
            );
        }
    }
}

/// Everything the three sets answered with, gathered once when a turn starts.
pub struct Late {
    pub tools: Vec<Arc<dyn Tool>>,
    /// Tool names a source offered that were already taken; said once, here.
    pub shadowed: Vec<String>,
    pub contributors: Vec<Arc<dyn ContextContributor>>,
    pub compactor: Option<Arc<dyn Compactor>>,
}

impl Late {
    pub async fn gather(cfg: &super::TurnConfig) -> Self {
        let (tools, shadowed) = cfg.tools.gather().await;
        Self {
            tools,
            shadowed,
            contributors: cfg.contributors.gather().await,
            compactor: cfg.compactor.resolve().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{ScriptedCompactor, ScriptedCompactorSource, ScriptedContextSource};

    fn compaction() -> Compaction {
        Compaction {
            summary: "said".into(),
            boundary: ItemId::from_raw("itm_1"),
            kept: Vec::new(),
            before: 10,
            after: 1,
            usage: Usage::default(),
        }
    }

    #[tokio::test]
    async fn a_source_s_contributors_join_the_registered_ones_in_order() {
        let source = ScriptedContextSource::new("late");
        let set = ContributorSet {
            fixed: vec![crate::test_support::fixed_contributor("early")],
            sources: vec![Arc::clone(&source) as Arc<dyn ContextSource>],
        };
        assert_eq!(
            ids(&set.gather().await),
            ["early"],
            "the source had nothing"
        );
        source.set(vec![crate::test_support::fixed_contributor("late")]);
        assert_eq!(ids(&set.gather().await), ["early", "late"]);
    }

    fn ids(contributors: &[Arc<dyn ContextContributor>]) -> Vec<String> {
        contributors.iter().map(|c| c.id().to_string()).collect()
    }

    #[tokio::test]
    async fn a_source_s_strategy_is_used_where_the_slot_is_free() {
        let source =
            ScriptedCompactorSource::new(vec![ScriptedCompactor::new(vec![Ok(compaction())])]);
        let set = CompactorSet {
            fixed: None,
            sources: vec![source],
        };
        assert!(set.resolve().await.is_some());
    }

    #[tokio::test]
    async fn the_registered_strategy_keeps_the_slot_against_a_source_s() {
        let held = ScriptedCompactor::new(vec![Ok(compaction())]);
        let source =
            ScriptedCompactorSource::new(vec![ScriptedCompactor::new(vec![Ok(compaction())])]);
        let set = CompactorSet {
            fixed: Some(Arc::clone(&held) as Arc<dyn Compactor>),
            sources: vec![source],
        };
        let resolved = set.resolve().await.expect("a strategy");
        assert!(Arc::ptr_eq(
            &(Arc::clone(&held) as Arc<dyn Compactor>),
            &resolved
        ));
    }

    #[tokio::test]
    async fn a_turn_with_no_strategy_anywhere_resolves_to_none() {
        assert!(CompactorSet::default().resolve().await.is_none());
    }
}

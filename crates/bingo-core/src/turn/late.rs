//! What a turn resolves when it starts (ADR-0009 §1): the tools, the
//! contributors and the compaction strategy, each the ones registered up front
//! plus whatever the sources answer with now.
//!
//! Every kind that learns to arrive after I/O — a bridge's, an MCP server's —
//! is a set here and nowhere else: two resolution points would be two answers
//! to "what does this session have", which is the debt ADR-0011 forbids.
//!
//! Three of the sets are gathered by [`Late`] when a turn starts.
//! [`ProviderSet`] is gathered where a provider is resolved instead — a model
//! is chosen when a session opens, when `/model` rewrites it and when a
//! catalogue is read, never per round — and the host reads it at that one
//! point. [`HookSet`] is asked at each of the kernel's own hook points, which
//! is where hooks have always been read: a session opening, a submission, a
//! turn's edges, a call at the gate.

use std::sync::Arc;

use bingo_sdk::*;

use crate::gate::hook_applies;

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

/// The hooks a session runs: the registered ones, then every source's. Order
/// is composition order — a source's hooks join the end of the registered
/// list, and a bridge hook composes with an in-process one exactly as two
/// in-process hooks compose (ADR-0032 §2).
///
/// Two hooks may share an id, as two contributors may: they are run, never
/// looked up by name.
#[derive(Clone, Default)]
pub struct HookSet {
    pub fixed: Vec<Arc<dyn Hook>>,
    pub sources: Vec<Arc<dyn HookSource>>,
}

impl HookSet {
    pub fn fixed(hooks: Vec<Arc<dyn Hook>>) -> Self {
        Self {
            fixed: hooks,
            sources: Vec::new(),
        }
    }

    /// Whether anything could answer at all. A session with no hook anywhere
    /// skips the machinery rather than gathering an empty list.
    pub fn is_empty(&self) -> bool {
        self.fixed.is_empty() && self.sources.is_empty()
    }

    pub async fn gather(&self) -> Vec<Arc<dyn Hook>> {
        let mut hooks = self.fixed.clone();
        for source in &self.sources {
            hooks.extend(source.hooks().await);
        }
        hooks
    }

    /// The hooks that claim this point, for this tool where the point has one.
    /// The matcher is the cheap skip: a hook that does not want this point is
    /// never asked, and for a hook that lives in another process that means
    /// the event never crosses the pipe.
    pub async fn at(&self, point: HookPoint, tool: Option<&str>) -> Vec<Arc<dyn Hook>> {
        self.gather()
            .await
            .into_iter()
            .filter(|hook| hook_applies(&hook.matcher(), point, tool))
            .collect()
    }
}

/// The providers a model may be chosen from: the registered ones, then every
/// source's. An id answers for one provider, so a late one whose id is already
/// taken is dropped — the registry's own rule for a duplicate, read where the
/// sources are, because a source cannot refuse a boot that has already happened.
#[derive(Clone, Default)]
pub struct ProviderSet {
    pub fixed: Vec<Arc<dyn Provider>>,
    pub sources: Vec<Arc<dyn ProviderSource>>,
}

impl ProviderSet {
    pub fn fixed(providers: Vec<Arc<dyn Provider>>) -> Self {
        Self {
            fixed: providers,
            sources: Vec::new(),
        }
    }

    pub async fn gather(&self) -> Vec<Arc<dyn Provider>> {
        let mut providers = self.fixed.clone();
        for source in &self.sources {
            for provider in source.providers().await {
                Self::take(&mut providers, source.id(), provider);
            }
        }
        providers
    }

    fn take(providers: &mut Vec<Arc<dyn Provider>>, source: &str, provider: Arc<dyn Provider>) {
        if providers.iter().any(|held| held.id() == provider.id()) {
            tracing::debug!(
                source,
                provider = provider.id(),
                "a provider from a source is unused; one of that id is already registered"
            );
            return;
        }
        providers.push(provider);
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
    use crate::test_support::{
        ScriptedCompactor, ScriptedCompactorSource, ScriptedContextSource, ScriptedProvider,
        ScriptedProviderSource,
    };

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

    /// A provider that arrived after I/O joins the registered ones, and the
    /// order is the registered ones first: nothing a source answers with can
    /// take an id the composition already gave away.
    #[tokio::test]
    async fn a_source_s_providers_join_the_registered_ones_and_never_shadow_one() {
        let source = ScriptedProviderSource::new(vec![]);
        let set = ProviderSet {
            fixed: vec![ScriptedProvider::new(vec![]) as Arc<dyn Provider>],
            sources: vec![Arc::clone(&source) as Arc<dyn ProviderSource>],
        };
        assert_eq!(provider_ids(&set.gather().await), ["scripted"]);

        source.set(vec![Arc::new(Named("late")) as Arc<dyn Provider>]);
        assert_eq!(provider_ids(&set.gather().await), ["scripted", "late"]);

        source.set(vec![Arc::new(Named("scripted")) as Arc<dyn Provider>]);
        assert_eq!(
            provider_ids(&set.gather().await),
            ["scripted"],
            "the registered one keeps its id"
        );
    }

    #[tokio::test]
    async fn a_turn_with_no_provider_anywhere_gathers_none() {
        assert!(ProviderSet::default().gather().await.is_empty());
    }

    fn provider_ids(providers: &[Arc<dyn Provider>]) -> Vec<String> {
        providers.iter().map(|p| p.id().to_string()).collect()
    }

    /// A provider that is nothing but its id.
    struct Named(&'static str);

    #[async_trait::async_trait]
    impl Provider for Named {
        fn id(&self) -> &str {
            self.0
        }
        fn endpoint(&self, _model: &str) -> EndpointCapabilities {
            EndpointCapabilities::default()
        }
        async fn stream(
            &self,
            _request: ModelRequest,
            _cancel: CancellationToken,
        ) -> Result<ModelStream, ProviderError> {
            unreachable!("this double runs no turn")
        }
    }

    /// A hook that is nothing but an id and the points it claims.
    struct Claims(&'static str, Vec<HookPoint>);

    #[async_trait::async_trait]
    impl Hook for Claims {
        fn id(&self) -> &str {
            self.0
        }
        fn matcher(&self) -> HookMatcher {
            HookMatcher {
                points: self.1.clone(),
                tool: None,
            }
        }
    }

    fn claims(id: &'static str, points: Vec<HookPoint>) -> Arc<dyn Hook> {
        Arc::new(Claims(id, points))
    }

    #[tokio::test]
    async fn a_source_s_hooks_join_the_registered_ones_in_order() {
        let set = HookSet {
            fixed: vec![claims("early", vec![])],
            sources: vec![crate::test_support::ScriptedHookSource::new(vec![claims(
                "late",
                vec![],
            )])],
        };
        assert_eq!(hook_ids(&set.gather().await), ["early", "late"]);
    }

    /// The cheap skip, which is also what keeps an unmatched event off a
    /// plugin's pipe: a hook that does not claim the point is not asked.
    #[tokio::test]
    async fn a_point_asks_only_the_hooks_that_claim_it() {
        let set = HookSet::fixed(vec![
            claims("everything", vec![]),
            claims("submits", vec![HookPoint::Submit]),
            claims("watches", vec![HookPoint::Event]),
        ]);
        assert_eq!(
            hook_ids(&set.at(HookPoint::Submit, None).await),
            ["everything", "submits"]
        );
        assert_eq!(
            hook_ids(&set.at(HookPoint::Event, None).await),
            ["everything", "watches"]
        );
    }

    #[tokio::test]
    async fn a_session_with_no_hook_anywhere_is_empty() {
        assert!(HookSet::default().is_empty());
        assert!(HookSet::default().gather().await.is_empty());
    }

    fn hook_ids(hooks: &[Arc<dyn Hook>]) -> Vec<String> {
        hooks.iter().map(|h| h.id().to_string()).collect()
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

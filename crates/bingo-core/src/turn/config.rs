//! What one turn reads: the budget it may spend, and everything the host
//! resolved for this session before the first round.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bingo_sdk::*;

use crate::models::Learned;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnBudget {
    pub max_rounds: u32,
    pub max_retries: u32,
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self {
            max_rounds: 100,
            max_retries: 10,
        }
    }
}

/// The model the host resolved for one session (ADR-0004): who serves it,
/// what it may assume, how much output it asks for.
pub struct ModelChoice {
    pub provider: Arc<dyn Provider>,
    pub id: String,
    pub capabilities: ModelCapabilities,
    pub max_tokens: u32,
    /// Only set when the model reasons: the wire parameter would 400 otherwise.
    pub reasoning: Option<Effort>,
    /// Where an overflow's lesson about the window goes.
    pub learned: Arc<Learned>,
}

/// Compactions the kernel discarded in a row, per session (ADR-0006). At
/// `TRIP` the breaker is tripped: no more summaries are paid for until one
/// shrinks something.
#[derive(Debug, Default)]
pub struct Breaker {
    failures: AtomicU32,
}

impl Breaker {
    pub const TRIP: u32 = bingo_sdk::compactor::BREAKER_TRIP;

    pub fn failures(&self) -> u32 {
        self.failures.load(Ordering::Relaxed)
    }

    pub fn tripped(&self) -> bool {
        self.failures() >= Self::TRIP
    }

    pub fn failed(&self) -> u32 {
        self.failures.fetch_add(1, Ordering::Relaxed) + 1
    }

    pub fn succeeded(&self) {
        self.failures.store(0, Ordering::Relaxed);
    }
}

/// The tools a session may call: the ones registered up front, the sources
/// that answer late (ADR-0009), and the names the session is limited to.
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

/// Everything a turn reads. Built by the host per session; plugins are already resolved.
pub struct TurnConfig {
    pub session: SessionSummary,
    pub cwd: PathBuf,
    pub model: ModelChoice,
    pub compaction: Arc<Breaker>,
    pub system: Vec<SystemBlock>,
    pub tools: ToolSet,
    pub policy: Arc<dyn PermissionPolicy>,
    pub hooks: Vec<Arc<dyn Hook>>,
    pub contributors: Vec<Arc<dyn ContextContributor>>,
    pub compactor: Option<Arc<dyn Compactor>>,
    pub budget: TurnBudget,
    pub env: Arc<Env>,
    /// The whole host, for a tool, a hook and a redirect (ADR-0011 §3).
    pub host: HostHandle,
    /// What a call may do to its own session.
    pub tool_host: Arc<dyn ToolHost>,
}

impl std::fmt::Debug for TurnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnConfig")
            .field("session", &self.session.id)
            .field("model", &self.model.id)
            .finish_non_exhaustive()
    }
}

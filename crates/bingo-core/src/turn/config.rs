//! What one turn reads: the budget it may spend, and everything the host
//! resolved for this session before the first round.

use std::path::PathBuf;
use std::sync::Arc;

use bingo_sdk::*;

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

/// Everything a turn reads. Built by the host per session; plugins are already resolved.
pub struct TurnConfig {
    pub session: SessionSummary,
    pub cwd: PathBuf,
    pub provider: Arc<dyn Provider>,
    pub model: String,
    pub capabilities: ModelCapabilities,
    pub max_tokens: u32,
    pub reasoning: Option<Effort>,
    pub system: Vec<SystemBlock>,
    pub tools: Vec<Arc<dyn Tool>>,
    pub policy: Arc<dyn PermissionPolicy>,
    pub hooks: Vec<Arc<dyn Hook>>,
    pub contributors: Vec<Arc<dyn ContextContributor>>,
    pub compactor: Option<Arc<dyn Compactor>>,
    pub budget: TurnBudget,
    pub env: Arc<Env>,
    pub tool_host: Arc<dyn ToolHost>,
}

impl std::fmt::Debug for TurnConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnConfig")
            .field("session", &self.session.id)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

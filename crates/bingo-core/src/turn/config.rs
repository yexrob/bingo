//! What one turn reads: the budget it may spend, and everything the host
//! resolved for this session before the first round.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bingo_sdk::*;

use super::late::{CompactorSet, ContributorSet, HookSet, ToolSet};
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
#[derive(Clone)]
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

impl std::fmt::Debug for ModelChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelChoice")
            .field("provider", &self.provider.id())
            .field("id", &self.id)
            .field("reasoning", &self.reasoning)
            .field("max_tokens", &self.max_tokens)
            .finish_non_exhaustive()
    }
}

/// A summary saying what the session runs on. The choice is the fact; the
/// summary's `provider` and `model` are its shadow, stamped here and nowhere
/// else — a summary read back from a journal names the model of the process
/// that wrote it, which is not necessarily the one that answers now.
pub fn runs_on(summary: SessionSummary, choice: Option<&ModelChoice>) -> SessionSummary {
    SessionSummary {
        model: choice.map(|c| c.id.clone()),
        provider: choice.map(|c| c.provider.id().to_string()),
        ..summary
    }
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

/// Everything a turn reads. Built by the host per session; what arrives after
/// I/O is a set here and resolved when the turn starts ([`super::late`]).
pub struct TurnConfig {
    pub session: SessionSummary,
    pub cwd: PathBuf,
    /// `None` for a session nothing answers (ADR-0011 §1): it opens no turn.
    pub model: Option<ModelChoice>,
    pub compaction: Arc<Breaker>,
    pub system: Vec<SystemBlock>,
    pub tools: ToolSet,
    pub policy: Arc<dyn PermissionPolicy>,
    /// The one place the kernel's hooks come from, registered and late alike;
    /// every point asks this set and no other list (ADR-0032 §1).
    pub hooks: HookSet,
    pub contributors: ContributorSet,
    pub compactor: CompactorSet,
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
            .field("model", &self.model.as_ref().map(|m| &m.id))
            .finish_non_exhaustive()
    }
}

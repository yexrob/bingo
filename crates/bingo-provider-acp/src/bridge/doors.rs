//! The seam: the two doors the bridge needs into bingo, and nothing else.
//!
//! Everything on the transport side of ADR-0036 — the listener, the token,
//! the MCP loop — is written against this trait alone, so none of it knows
//! what a turn is, what a catalog is, or that a kernel exists. The other side
//! of the seam is implemented over `HostHandle`: [`Doors::offer`] reads the
//! tools catalog and keeps the entries that said `shared` (§1), and
//! [`Doors::call`] is the one kernel verb that delivers a call into the
//! session's running turn (§2).
//!
//! Two verbs, on purpose. A third would mean the bridge had started deciding
//! something, and what may be called and what a call does are both bingo's to
//! say, not the transport's.
//!
//! The offer is not static: when the catalog moves, the bridge is told through
//! [`super::Bridge::offer_changed`] and every live conversation hears MCP's
//! `notifications/tools/list_changed`. That word travels the other way down
//! the seam, which is why it is not on this trait.

use async_trait::async_trait;
use bingo_sdk::{ToolCall, ToolOutput, ToolSpec};

#[async_trait]
pub trait Doors: Send + Sync + 'static {
    /// What this ACP session may call, as the agent will see it. Derived from
    /// the catalog every time it is asked — the bridge keeps no list of its
    /// own, so a tool that says `shared` later appears with no edit here
    /// (ADR-0036 §1).
    async fn offer(&self) -> Vec<ToolSpec>;

    /// Deliver one call into the session's running turn and wait for what it
    /// returns.
    ///
    /// `Ok` is the tool's own answer, `is_error` and all: the call ran. `Err`
    /// is a refusal to run it — no turn was in flight, the gate said no, the
    /// turn was interrupted — and reaches the agent as an MCP error *result*
    /// rather than a protocol error, because the call was heard and answered.
    async fn call(&self, call: ToolCall) -> Result<ToolOutput, Refused>;
}

/// Why a call never ran, in the words the agent reads.
///
/// One string and no variants: the only thing this side of the seam does with
/// a refusal is say it, and a shape richer than that would be a fact with two
/// representations.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Refused(pub String);

impl Refused {
    pub fn new(why: impl Into<String>) -> Self {
        Self(why.into())
    }
}

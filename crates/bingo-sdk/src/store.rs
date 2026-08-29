//! Persistence of the journal. The actor appends before it publishes.

use async_trait::async_trait;

use crate::error::KernelError;
use crate::event::{Frame, SessionSummary};
use crate::host::SessionFilter;
use crate::ids::{Seq, SessionId};

#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create(&self, summary: &SessionSummary) -> Result<(), KernelError>;

    async fn append(&self, session: &SessionId, frame: &Frame) -> Result<(), KernelError>;

    /// Durable frames with `seq > since`, in order.
    async fn replay(&self, session: &SessionId, since: Seq) -> Result<Vec<Frame>, KernelError>;

    async fn list(&self, filter: &SessionFilter) -> Result<Vec<SessionSummary>, KernelError>;

    async fn delete(&self, session: &SessionId) -> Result<(), KernelError>;

    /// Take the session for this process, from create or resume until
    /// `release` or exit; a second holder gets `SessionLocked` (ADR-0005).
    async fn acquire(&self, _session: &SessionId) -> Result<(), KernelError> {
        Ok(())
    }

    async fn release(&self, _session: &SessionId) -> Result<(), KernelError> {
        Ok(())
    }
}

//! Doubles a plugin's tests share, behind the `testing` feature: the host
//! that is not there, for a context that must carry one.

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{ErrorCode, KernelError};
use crate::event::*;
use crate::host::*;
use crate::ids::{IntentId, SessionId};

/// A host every call to which is an error: what a test hands a tool or a
/// hook that must never reach the host.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoHost;

impl NoHost {
    pub fn handle() -> HostHandle {
        HostHandle(Arc::new(NoHost))
    }

    fn absent<T>() -> Result<T, KernelError> {
        Err(KernelError::new(
            ErrorCode::Internal,
            "this test has no host",
        ))
    }
}

#[async_trait]
impl HostApi for NoHost {
    async fn sessions(&self, _: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        Self::absent()
    }

    async fn open(
        &self,
        _: SessionSelector,
        _: ClientIdentity,
        _: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        Self::absent()
    }

    async fn close(&self, _: &SessionId, _: CloseReason) -> Result<(), KernelError> {
        Self::absent()
    }

    async fn delete(&self, _: &SessionId) -> Result<(), KernelError> {
        Self::absent()
    }

    async fn deliver(
        &self,
        _: &SessionId,
        _: IntentId,
        _: Input,
        _: Delivery,
    ) -> Result<(), KernelError> {
        Self::absent()
    }

    async fn extend(&self, _: &SessionId, _: &str, _: &str, _: Value) -> Result<(), KernelError> {
        Self::absent()
    }

    async fn signal(&self, _: &SessionId, _: &str, _: &str, _: Value) -> Result<(), KernelError> {
        Self::absent()
    }

    async fn catalog(&self, _: CatalogKind) -> Result<Catalog, KernelError> {
        Self::absent()
    }

    fn gateway_events(&self) -> GatewayStream {
        Box::pin(futures::stream::empty())
    }

    fn service_any(&self, _: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

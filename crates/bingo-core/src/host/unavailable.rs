//! The handle a host hands out after it has been torn down.

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::*;

pub(super) struct Unavailable;

fn unavailable() -> KernelError {
    KernelError::new(ErrorCode::SessionClosed, "the host is shut down")
}

#[async_trait]
impl HostApi for Unavailable {
    async fn sessions(&self, _: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        Err(unavailable())
    }
    async fn open(
        &self,
        _: SessionSelector,
        _: ClientIdentity,
        _: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        Err(unavailable())
    }
    async fn close(&self, _: &SessionId, _: CloseReason) -> Result<(), KernelError> {
        Err(unavailable())
    }
    async fn delete(&self, _: &SessionId) -> Result<(), KernelError> {
        Err(unavailable())
    }
    async fn catalog(&self, kind: CatalogKind) -> Result<Catalog, KernelError> {
        Ok(Catalog {
            kind,
            entries: Vec::new(),
        })
    }
    fn gateway_events(&self) -> GatewayStream {
        Box::pin(futures::stream::empty())
    }
    fn service_any(&self, _: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        None
    }
}

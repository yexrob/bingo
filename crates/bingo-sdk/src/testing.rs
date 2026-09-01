//! Doubles a plugin's tests share, behind the `testing` feature: the host
//! that is not there, for a context that must carry one, and the host that is
//! nothing but the services opened in it.

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::{ErrorCode, KernelError};
use crate::event::*;
use crate::host::*;
use crate::ids::{IntentId, SessionId};
use crate::service::{Services, WireService};

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

/// A host that is nothing but its services: it keeps what a plugin opens in
/// it and hands both faces back, and answers everything else the way
/// [`NoHost`] does. What a bridge's own tests hand a process (ADR-0031 §4).
#[derive(Debug, Default)]
pub struct ServiceHost(Services);

impl ServiceHost {
    pub fn handle() -> HostHandle {
        HostHandle(Arc::new(ServiceHost::default()))
    }
}

#[async_trait]
impl HostApi for ServiceHost {
    async fn sessions(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        NoHost.sessions(filter).await
    }

    async fn open(
        &self,
        selector: SessionSelector,
        who: ClientIdentity,
        options: OpenOptions,
    ) -> Result<Attachment, KernelError> {
        NoHost.open(selector, who, options).await
    }

    async fn close(&self, session: &SessionId, reason: CloseReason) -> Result<(), KernelError> {
        NoHost.close(session, reason).await
    }

    async fn delete(&self, session: &SessionId) -> Result<(), KernelError> {
        NoHost.delete(session).await
    }

    async fn deliver(
        &self,
        to: &SessionId,
        intent: IntentId,
        input: Input,
        delivery: Delivery,
    ) -> Result<(), KernelError> {
        NoHost.deliver(to, intent, input, delivery).await
    }

    async fn extend(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        NoHost.extend(session, plugin, kind, payload).await
    }

    async fn signal(
        &self,
        session: &SessionId,
        plugin: &str,
        kind: &str,
        payload: Value,
    ) -> Result<(), KernelError> {
        NoHost.signal(session, plugin, kind, payload).await
    }

    async fn catalog(&self, kind: CatalogKind) -> Result<Catalog, KernelError> {
        NoHost.catalog(kind).await
    }

    fn gateway_events(&self) -> GatewayStream {
        NoHost.gateway_events()
    }

    fn service_any(&self, key: &str) -> Option<Arc<dyn Any + Send + Sync>> {
        self.0.value(key)
    }

    fn service_wire(&self, key: &str) -> Option<Arc<dyn WireService>> {
        self.0.wire(key)
    }

    fn open_service(&self, key: &str, wire: Arc<dyn WireService>) -> Result<(), KernelError> {
        self.0
            .open(key, wire)
            .map_err(|why| KernelError::new(ErrorCode::InvalidInput, why))
    }
}

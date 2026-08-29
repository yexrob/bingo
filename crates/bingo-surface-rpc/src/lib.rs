//! The JSON-RPC surface: the sdk with an envelope, and nothing else (ADR-0007).
//!
//! [`serve`] is `bingo serve --stdio`; [`RemoteKernel`] is the same contract
//! seen from the other end of the pipe; [`schema::document`] is both of them
//! written down as JSON Schema, committed at `schema/rpc.json`.

pub mod codec;
pub mod methods;
pub mod schema;
pub mod server;
mod session;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ErrorCode, Exit, HostHandle, KernelError, Plugin, PluginError, PluginManifest, Registrar,
    Surface, SurfaceKind, SurfaceOptions,
};
use serde_json::Value;

pub use schema::document;
pub use server::serve;

/// The surface id, and the transport `SurfaceOptions.args` must ask for.
pub const SURFACE_ID: &str = "rpc";
const STDIO: &str = "stdio";

/// Owns stdio, has no prompt and no session of its own.
#[derive(Debug, Default, Clone, Copy)]
pub struct RpcSurface;

#[async_trait]
impl Surface for RpcSurface {
    fn id(&self) -> &str {
        SURFACE_ID
    }

    fn kind(&self) -> SurfaceKind {
        SurfaceKind::Exclusive
    }

    async fn run(&self, host: HostHandle, opts: SurfaceOptions) -> Result<Exit, KernelError> {
        transport(&opts.args)?;
        serve(host, tokio::io::stdin(), tokio::io::stdout()).await
    }
}

/// A WebSocket carries the same bytes and arrives with the first client that
/// needs one (ADR-0007); until then the only transport is named explicitly.
fn transport(args: &Value) -> Result<(), KernelError> {
    match args.get("transport").and_then(Value::as_str) {
        Some(STDIO) => Ok(()),
        other => Err(KernelError::new(
            ErrorCode::InvalidInput,
            format!("unsupported rpc transport {other:?}; only {STDIO:?}"),
        )),
    }
}

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.surface.rpc",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["surface:rpc"],
    requires: &[],
    config: None,
};

#[derive(Debug, Default, Clone, Copy)]
pub struct RpcPlugin;

#[async_trait]
impl Plugin for RpcPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        registrar.surface(Arc::new(RpcSurface) as Arc<dyn Surface>);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn only_stdio_is_a_transport() {
        assert!(transport(&json!({ "transport": "stdio" })).is_ok());
        for args in [json!({}), json!({ "transport": "ws" }), json!(null)] {
            let error = transport(&args).expect_err("only stdio is served");
            assert_eq!(error.code, ErrorCode::InvalidInput);
        }
    }

    #[test]
    fn the_surface_owns_stdio_alone() {
        assert_eq!(RpcSurface.id(), SURFACE_ID);
        assert_eq!(RpcSurface.kind(), SurfaceKind::Exclusive);
        assert_eq!(MANIFEST.provides, &["surface:rpc"]);
    }
}

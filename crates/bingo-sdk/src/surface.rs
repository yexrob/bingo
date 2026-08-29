//! A frontend is a client. The kernel calls nothing on it but `run`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::KernelError;
use crate::host::{HostHandle, SessionSelector};
use crate::tool::Env;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceKind {
    /// Owns the terminal or stdio; one at a time.
    Exclusive,
    /// Runs beside others (IM channels, servers).
    Concurrent,
}

#[derive(Clone, Debug)]
pub struct SurfaceOptions {
    pub cwd: PathBuf,
    pub selector: SessionSelector,
    /// A first prompt to submit, for headless use.
    pub prompt: Option<String>,
    /// Surface-specific options, from the command line or config.
    pub args: Value,
    /// Where this process keeps its files (prompt history, caches). Process-local
    /// by nature, so it is handed to the surface, not asked of the host.
    pub env: Arc<Env>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    pub code: i32,
}

#[async_trait]
pub trait Surface: Send + Sync {
    fn id(&self) -> &str;

    fn kind(&self) -> SurfaceKind;

    async fn run(&self, host: HostHandle, opts: SurfaceOptions) -> Result<Exit, KernelError>;
}

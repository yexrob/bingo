//! A frontend is a client. The kernel calls nothing on it but `run`.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::KernelError;
use crate::host::{HostHandle, SessionSelector};

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

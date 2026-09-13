//! A frontend is a client. The kernel calls nothing on it but `run`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::KernelError;
use crate::event::TurnStatus;
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

impl Exit {
    /// What a run that ends on this turn leaves behind: the shell's own
    /// spelling of the three ways a turn can end. `130` is what a shell
    /// reports for a program stopped by `SIGINT`, which is what an interrupt
    /// is to whoever started the run.
    pub fn for_turn(status: &TurnStatus) -> Self {
        let code = match status {
            TurnStatus::Completed => 0,
            TurnStatus::Failed { .. } => 1,
            TurnStatus::Interrupted { .. } => 130,
        };
        Self { code }
    }
}

#[async_trait]
pub trait Surface: Send + Sync {
    fn id(&self) -> &str;

    fn kind(&self) -> SurfaceKind;

    async fn run(&self, host: HostHandle, opts: SurfaceOptions) -> Result<Exit, KernelError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use crate::event::InterruptReason;

    #[test]
    fn a_turn_ends_a_run_the_way_a_shell_reads_it() {
        assert_eq!(Exit::for_turn(&TurnStatus::Completed), Exit { code: 0 });
        assert_eq!(
            Exit::for_turn(&TurnStatus::Failed {
                error: KernelError::new(ErrorCode::Internal, "no"),
            }),
            Exit { code: 1 }
        );
        assert_eq!(
            Exit::for_turn(&TurnStatus::Interrupted {
                reason: InterruptReason::UserCancel,
            }),
            Exit { code: 130 }
        );
    }
}

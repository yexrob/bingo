//! The `HostHandle` a plugin is handed at `start`, shared with the tools, the
//! hook and the command, which were all registered before it existed.

use std::sync::OnceLock;

use bingo_sdk::{ErrorCode, HostHandle, KernelError};

/// Set once, by `Plugin::start`. Everything that needs the session tree reads
/// it through here rather than being built twice.
#[derive(Default)]
pub struct LateHost(OnceLock<HostHandle>);

impl LateHost {
    /// The host, or nothing while the process is still registering.
    pub fn get(&self) -> Option<&HostHandle> {
        self.0.get()
    }

    /// Whether this was the call that set it.
    pub fn set(&self, host: HostHandle) -> bool {
        self.0.set(host).is_ok()
    }

    /// The host, or the error a call before `start` deserves.
    pub fn require(&self) -> Result<&HostHandle, KernelError> {
        self.get().ok_or_else(|| {
            KernelError::new(
                ErrorCode::NotInitialized,
                "the agents plugin has not started",
            )
        })
    }
}

impl std::fmt::Debug for LateHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LateHost")
            .field(&self.get().is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::Fleet;

    #[test]
    fn nothing_reaches_the_host_before_start() {
        let late = LateHost::default();
        assert!(late.get().is_none());
        let error = late.require().expect_err("not started");
        assert_eq!(error.code, ErrorCode::NotInitialized);
    }

    #[test]
    fn the_first_start_wins_and_the_host_is_there_after_it() {
        let late = LateHost::default();
        assert!(late.set(Fleet::default().handle()));
        assert!(!late.set(Fleet::default().handle()), "started twice");
        assert!(late.require().is_ok());
    }
}

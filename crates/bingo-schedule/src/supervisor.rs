//! Ownership follows the running loop, not the process's first attempt.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use bingo_sdk::CancellationToken;
use tokio::sync::Notify;

use crate::lock::{Claim, ClaimError};
use crate::runner::Runner;

const RETRY: Duration = Duration::from_secs(1);

pub(crate) struct Supervisor {
    pub dir: PathBuf,
    pub held: Arc<Mutex<Weak<Claim>>>,
    pub trouble: Arc<Mutex<Option<String>>>,
    pub changed: Arc<Notify>,
    pub cancel: CancellationToken,
    pub runner: Runner,
}

impl Supervisor {
    /// The task owns the only strong reference. A panic or abort therefore
    /// releases the OS lock without leaving a separate "running" flag set.
    pub fn acquire(&self) -> Option<Arc<Claim>> {
        match Claim::take(&self.dir) {
            Ok(claim) => {
                let claim = Arc::new(claim);
                *self.held.lock().unwrap_or_else(|held| held.into_inner()) = Arc::downgrade(&claim);
                *self.trouble.lock().unwrap_or_else(|held| held.into_inner()) = None;
                Some(claim)
            }
            Err(ClaimError::WouldBlock { .. }) => {
                *self.trouble.lock().unwrap_or_else(|held| held.into_inner()) = None;
                None
            }
            Err(error) => {
                let said = error.to_string();
                let mut trouble = self.trouble.lock().unwrap_or_else(|held| held.into_inner());
                if trouble.as_ref() != Some(&said) {
                    tracing::warn!(%error, "the scheduler could not take ownership");
                    *trouble = Some(said);
                }
                None
            }
        }
    }

    pub async fn run(self, mut claim: Option<Arc<Claim>>) {
        loop {
            if self.cancel.is_cancelled() {
                return;
            }
            if let Some(claim) = claim {
                // Do not release ownership merely because cancellation was
                // requested: an in-flight dispatch must finish under it.
                self.runner.run().await;
                drop(claim);
                return;
            }
            tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return,
                _ = tokio::time::sleep(RETRY) => {},
                _ = self.changed.notified() => {},
            }
            if !self.cancel.is_cancelled() {
                claim = self.acquire();
            }
        }
    }
}

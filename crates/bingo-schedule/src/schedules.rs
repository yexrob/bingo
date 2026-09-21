//! What this process has: the store's entries, the claim on running them,
//! the bell the tools ring when they write — and the wakes of the sessions
//! it runs, which are no part of the store.
//!
//! One of these is built when the plugin registers and shared by the tools,
//! the command and the runner, so "do schedules fire here?" has one answer
//! and every surface reads the same one.

use std::path::Path;
use std::sync::{Arc, Mutex, Weak};

use bingo_sdk::{CancellationToken, HostHandle};
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::lock::{self, Claim};
use crate::runner::Runner;
use crate::store::Store;
use crate::supervisor::Supervisor;
use crate::wakes::{self, Wakes};

#[derive(Debug)]
pub struct Schedules {
    store: Arc<Store>,
    changed: Arc<Notify>,
    trouble: Arc<Mutex<Option<String>>>,
    /// The running task owns the claim; this reference cannot prolong it.
    claim: Arc<Mutex<Weak<Claim>>>,
    running: Mutex<Option<JoinHandle<()>>>,
    /// The wakes standing on this process's sessions (ADR-0019 §8). Every
    /// process delivers its own, claim or no claim.
    wakes: Arc<Wakes>,
    cancel: CancellationToken,
}

impl Schedules {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            store: Arc::new(Store::new(data_dir)),
            changed: Arc::new(Notify::new()),
            trouble: Arc::new(Mutex::new(None)),
            claim: Arc::new(Mutex::new(Weak::new())),
            running: Mutex::new(None),
            wakes: Arc::default(),
            cancel: CancellationToken::new(),
        }
    }

    /// The last fire that never reached a turn, if there was one.
    ///
    /// The loop's own account of itself is `tracing`, and this tree installs
    /// no subscriber, so a warning it writes is a warning nobody reads. What
    /// goes wrong inside a fired turn lands in that turn's transcript; what
    /// stops a fire from becoming a turn at all would otherwise be silent,
    /// and this is where a person finds it.
    pub fn trouble(&self) -> Option<String> {
        self.trouble
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn wakes(&self) -> &Arc<Wakes> {
        &self.wakes
    }

    /// Whether schedules fire in this process.
    pub fn held(&self) -> bool {
        self.claim
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .strong_count()
            > 0
    }

    /// The one line every surface shows about who runs these schedules.
    pub fn holder(&self) -> String {
        lock::holder(self.store.dir(), self.held())
    }

    /// The store changed: whatever the runner is sleeping for, it is now
    /// sleeping for the wrong thing.
    pub fn changed(&self) {
        self.changed.notify_one();
    }

    /// Every process delivers its own wakes. One runs the schedules; the
    /// others keep trying so an owner that leaves needs no manual successor.
    pub fn start(self: &Arc<Self>, host: HostHandle) {
        let mut running = self.running.lock().unwrap_or_else(|held| held.into_inner());
        if running.is_some() || self.cancel.is_cancelled() {
            return;
        }
        let supervisor = self.supervisor(host.clone());
        let claim = supervisor.acquire();
        let wakes = Arc::clone(&self.wakes);
        let cancel = self.cancel.clone();
        *running = Some(tokio::spawn(async move {
            tokio::join!(supervisor.run(claim), wakes::run(wakes, host, cancel));
        }));
    }

    fn supervisor(&self, host: HostHandle) -> Supervisor {
        Supervisor {
            dir: self.store.dir().to_path_buf(),
            held: Arc::clone(&self.claim),
            trouble: Arc::clone(&self.trouble),
            changed: Arc::clone(&self.changed),
            cancel: self.cancel.clone(),
            runner: Runner::new(
                Arc::clone(&self.store),
                host,
                Arc::clone(&self.changed),
                Arc::clone(&self.trouble),
                self.cancel.clone(),
            ),
        }
    }

    /// Cancellation asks the loop to end; joining proves it has ended and
    /// given back its claim, including any dispatch already in flight.
    pub async fn stop(&self) {
        self.cancel.cancel();
        let running = self
            .running
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .take();
        if let Some(running) = running
            && let Err(error) = running.await
        {
            tracing::warn!(%error, "the scheduler task ended unexpectedly");
        }
    }
}

#[cfg(test)]
mod tests;

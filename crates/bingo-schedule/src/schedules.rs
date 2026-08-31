//! What this process has of the store: the entries, the claim on running
//! them, and the bell the tools ring when they write.
//!
//! One of these is built when the plugin registers and shared by the tools,
//! the command and the runner, so "do schedules fire here?" has one answer
//! and every surface reads the same one.

use std::path::Path;
use std::sync::{Arc, Mutex};

use bingo_sdk::{CancellationToken, HostHandle};
use tokio::sync::Notify;

use crate::lock::{self, Claim};
use crate::runner::Runner;
use crate::store::Store;

#[derive(Debug)]
pub struct Schedules {
    store: Arc<Store>,
    changed: Arc<Notify>,
    /// Held from `start` to `stop`; `None` in a process that came second.
    claim: Mutex<Option<Claim>>,
    cancel: CancellationToken,
}

impl Schedules {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            store: Arc::new(Store::new(data_dir)),
            changed: Arc::new(Notify::new()),
            claim: Mutex::new(None),
            cancel: CancellationToken::new(),
        }
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Whether schedules fire in this process.
    pub fn held(&self) -> bool {
        self.claim().is_some()
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

    /// Take the store's claim and run the loop behind it, or leave the
    /// schedules dormant and say who has them (ADR-0019 §5).
    pub fn start(self: &Arc<Self>, host: HostHandle) {
        match Claim::take(self.store.dir()) {
            Ok(claim) => {
                *self.claim() = Some(claim);
                tokio::spawn(
                    Runner::new(
                        Arc::clone(&self.store),
                        host,
                        Arc::clone(&self.changed),
                        self.cancel.clone(),
                    )
                    .run(),
                );
            }
            Err(dormant) => tracing::info!("schedules are {dormant}"),
        }
    }

    /// Stop firing and give the claim back, in that order: a store nobody
    /// runs must not look like one somebody does.
    pub fn stop(&self) {
        self.cancel.cancel();
        self.claim().take();
    }

    fn claim(&self) -> std::sync::MutexGuard<'_, Option<Claim>> {
        self.claim.lock().unwrap_or_else(|held| held.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::testing::NoHost;

    fn schedules(home: &tempfile::TempDir) -> Arc<Schedules> {
        Arc::new(Schedules::new(home.path()))
    }

    #[test]
    fn a_process_that_has_not_started_holds_nothing() {
        let home = tempfile::tempdir().expect("a temp home");
        let schedules = schedules(&home);
        assert!(!schedules.held());
        assert_eq!(schedules.holder(), "dormant — no runner holds this store");
        assert_eq!(schedules.store().dir(), home.path().join("schedules"));
    }

    #[tokio::test]
    async fn the_first_process_holds_the_store_and_the_second_is_dormant() {
        let home = tempfile::tempdir().expect("a temp home");
        let first = schedules(&home);
        first.start(NoHost::handle());
        assert!(first.held());
        assert_eq!(first.holder(), "held by this process");

        let second = schedules(&home);
        second.start(NoHost::handle());
        assert!(!second.held(), "one runner per store (ADR-0019 §5)");
        let dormant = second.holder();
        assert!(dormant.starts_with("dormant — held by pid "), "{dormant}");
        assert!(dormant.contains("runner.lock"), "{dormant}");

        first.stop();
        assert!(!first.held(), "the claim is given back");
        assert_eq!(second.holder(), "dormant — no runner holds this store");
    }

    #[tokio::test]
    async fn a_process_that_stopped_lets_the_next_one_take_the_store() {
        let home = tempfile::tempdir().expect("a temp home");
        let first = schedules(&home);
        first.start(NoHost::handle());
        first.stop();
        let second = schedules(&home);
        second.start(NoHost::handle());
        assert!(second.held());
        second.stop();
    }
}

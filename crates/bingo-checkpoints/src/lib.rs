//! Checkpoints (ADR-0045): the bytes of a file before the turn that changed
//! them, and `/rewind` to put both the files and the conversation back.
//!
//! The pieces, in the order they depend on each other:
//!
//! - [`store`] — the directory: one turn per directory, one index line per
//!   file, the bytes beside it.
//! - [`hook`] — the map from a tool to the field naming the file it writes,
//!   and the `BeforeTool` hook that keeps it.
//! - [`turns`] — the turns of a transcript, pure.
//! - [`restore`] — what going back to a turn does to the files, planned
//!   before a byte moves.
//! - [`command`] — `/rewind`, which is the whole of what a person touches.
//!
//! What is *not* here: anything a shell line wrote. A `Bash` call names no
//! path this could read, so its changes are not snapshotted and a rewind does
//! not claim to undo them (ADR-0045 §2).

pub mod command;
pub mod hook;
pub mod restore;
pub mod store;
pub mod turns;

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bingo_sdk::{
    Command, Contribution, Hook, HostHandle, Plugin, PluginError, PluginManifest, Registrar,
    SessionFilter,
};

pub use command::RewindCommand;
pub use hook::SnapshotHook;
pub use store::Checkpoints;

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.checkpoints",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["hook:checkpoints", "command:rewind"],
    requires: &[],
    // Where the snapshots live follows the data directory; how many a turn
    // keeps follows what the turn edited. There is nothing to configure.
    config: None,
};

/// Registers the snapshot hook and `/rewind`, and collects the checkpoints of
/// sessions that are no longer there when the process starts.
#[derive(Debug, Default)]
pub struct CheckpointsPlugin {
    /// Built in `register`, where the environment is; used by `start`, which
    /// is handed nothing but the host.
    store: OnceLock<Arc<Checkpoints>>,
}

#[async_trait]
impl Plugin for CheckpointsPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let store = Arc::new(Checkpoints::new(&registrar.env().data_dir));
        registrar.add(Contribution::Hook(
            Arc::new(SnapshotHook::new(store.clone())) as Arc<dyn Hook>,
        ));
        registrar.add(Contribution::Command(
            Arc::new(RewindCommand::new(store.clone())) as Arc<dyn Command>,
        ));
        self.store
            .set(store)
            .map_err(|_| PluginError::Failed("the checkpoints plugin registered twice".into()))
    }

    /// Nothing expires a checkpoint but the end of the session it belongs to
    /// (ADR-0045 §4). A host that lists no session at all is a host with no
    /// store, not a host whose sessions have all been deleted, so it collects
    /// nothing: silence is never evidence of a deletion.
    async fn start(&self, host: HostHandle) -> Result<(), PluginError> {
        let Some(store) = self.store.get() else {
            return Ok(());
        };
        let alive = match host.sessions(SessionFilter::default()).await {
            Ok(sessions) => sessions,
            // Collection that cannot run is not a reason to refuse to start.
            Err(error) => {
                tracing::warn!(%error, "checkpoints were not collected");
                return Ok(());
            }
        };
        if alive.is_empty() {
            return Ok(());
        }
        let kept: Vec<String> = alive.iter().map(|s| s.id.as_str().to_string()).collect();
        let gone = store.collect(&kept);
        if !gone.is_empty() {
            tracing::info!(count = gone.len(), "collected checkpoints of gone sessions");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use bingo_sdk::{Env, SessionId, SessionSummary, TurnId};

    fn registered(home: &std::path::Path) -> Vec<Contribution> {
        let mut registrar = Registrar::new(MANIFEST.id, serde_json::Value::Null, Env::rooted(home));
        CheckpointsPlugin::default()
            .register(&mut registrar)
            .expect("registering does no i/o");
        registrar.into_contributions()
    }

    #[test]
    fn the_manifest_says_what_it_provides_and_claims_no_settings() {
        assert_eq!(MANIFEST.id, "bingo.checkpoints");
        assert!(MANIFEST.requires.is_empty());
        assert!(MANIFEST.config.is_none());
    }

    #[test]
    fn registering_contributes_a_hook_and_a_command_and_creates_nothing() {
        let home = tempfile::tempdir().expect("a temp home");
        let contributions = registered(home.path());
        assert_eq!(contributions.len(), MANIFEST.provides.len());
        assert!(matches!(contributions[0], Contribution::Hook(_)));
        assert!(matches!(contributions[1], Contribution::Command(_)));
        assert!(
            !home.path().join(".bingo/data/checkpoints").exists(),
            "registering creates no directory"
        );
    }

    /// One host registers a plugin once.
    #[test]
    fn registering_twice_is_refused_rather_than_silently_forgotten() {
        let home = tempfile::tempdir().expect("a temp home");
        let plugin = CheckpointsPlugin::default();
        let mut first = Registrar::new(
            MANIFEST.id,
            serde_json::Value::Null,
            Env::rooted(home.path()),
        );
        plugin.register(&mut first).expect("the first");
        let mut again = Registrar::new(
            MANIFEST.id,
            serde_json::Value::Null,
            Env::rooted(home.path()),
        );
        assert!(plugin.register(&mut again).is_err());
    }

    /// A host holding one session, for the sweep to measure the store against.
    #[derive(Clone)]
    struct OneSession(Option<SessionId>);

    #[async_trait]
    impl bingo_sdk::HostApi for OneSession {
        async fn sessions(
            &self,
            _filter: bingo_sdk::SessionFilter,
        ) -> Result<Vec<SessionSummary>, bingo_sdk::KernelError> {
            Ok(self
                .0
                .iter()
                .map(|id| SessionSummary {
                    id: id.clone(),
                    ..crate::tests::summary()
                })
                .collect())
        }

        async fn open(
            &self,
            _selector: bingo_sdk::SessionSelector,
            _who: bingo_sdk::ClientIdentity,
            _options: bingo_sdk::OpenOptions,
        ) -> Result<bingo_sdk::Attachment, bingo_sdk::KernelError> {
            unreachable!("the sweep opens nothing")
        }

        async fn close(
            &self,
            _session: &SessionId,
            _reason: bingo_sdk::CloseReason,
        ) -> Result<(), bingo_sdk::KernelError> {
            unreachable!("the sweep closes nothing")
        }

        async fn delete(&self, _session: &SessionId) -> Result<(), bingo_sdk::KernelError> {
            unreachable!("the sweep deletes no session")
        }

        async fn deliver(
            &self,
            _to: &SessionId,
            _intent: bingo_sdk::IntentId,
            _input: bingo_sdk::Input,
            _delivery: bingo_sdk::Delivery,
        ) -> Result<(), bingo_sdk::KernelError> {
            unreachable!("the sweep delivers nothing")
        }

        async fn extend(
            &self,
            _session: &SessionId,
            _plugin: &str,
            _kind: &str,
            _payload: serde_json::Value,
        ) -> Result<(), bingo_sdk::KernelError> {
            unreachable!("the sweep publishes nothing")
        }

        async fn signal(
            &self,
            _session: &SessionId,
            _plugin: &str,
            _kind: &str,
            _payload: serde_json::Value,
        ) -> Result<(), bingo_sdk::KernelError> {
            unreachable!("the sweep signals nothing")
        }

        async fn catalog(
            &self,
            _kind: bingo_sdk::CatalogKind,
        ) -> Result<bingo_sdk::Catalog, bingo_sdk::KernelError> {
            unreachable!("the sweep reads no catalog")
        }

        fn gateway_events(&self) -> bingo_sdk::GatewayStream {
            unreachable!("the sweep watches no gateway")
        }

        fn service_any(&self, _key: &str) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
            None
        }
    }

    /// A plugin that registered, with one file kept for each of two sessions.
    fn planted(home: &std::path::Path) -> CheckpointsPlugin {
        let plugin = CheckpointsPlugin::default();
        let mut registrar = Registrar::new(MANIFEST.id, serde_json::Value::Null, Env::rooted(home));
        plugin.register(&mut registrar).expect("register");
        let file = home.join("a.txt");
        std::fs::write(&file, b"x").expect("a file");
        let store = plugin.store.get().expect("a store");
        for id in ["ses_here", "ses_gone"] {
            store
                .snapshot(&SessionId::from_raw(id), &TurnId::from_raw("trn_1"), &file)
                .expect("a snapshot");
        }
        plugin
    }

    #[tokio::test]
    async fn starting_collects_the_checkpoints_of_a_session_that_is_no_longer_there() {
        let home = tempfile::tempdir().expect("a temp home");
        let plugin = planted(home.path());
        plugin
            .start(HostHandle(Arc::new(OneSession(Some(SessionId::from_raw(
                "ses_here",
            ))))))
            .await
            .expect("start");
        assert_eq!(
            plugin.store.get().expect("a store").sessions(),
            ["ses_here"]
        );
    }

    /// Silence is not evidence of a deletion: a host with no store lists no
    /// session, and a run without one must not take the snapshots with it.
    #[tokio::test]
    async fn a_host_that_lists_no_session_collects_nothing() {
        let home = tempfile::tempdir().expect("a temp home");
        let plugin = planted(home.path());
        plugin
            .start(HostHandle(Arc::new(OneSession(None))))
            .await
            .expect("start");
        assert_eq!(
            plugin.store.get().expect("a store").sessions(),
            ["ses_gone", "ses_here"]
        );
    }

    #[tokio::test]
    async fn a_plugin_that_never_registered_starts_without_a_store() {
        CheckpointsPlugin::default()
            .start(bingo_sdk::testing::NoHost::handle())
            .await
            .expect("start");
    }
}

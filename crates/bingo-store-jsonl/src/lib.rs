//! The session store on disk: one directory per session holding a JSONL
//! journal, a sidecar lock and a derived summary (ADR-0005).
//!
//! The journal is the session; the summary exists so `list` never reads a
//! journal body, and the lock is the only claim of ownership. The kernel says
//! when to take and give back that claim — `create` does not lock by itself.

pub mod gc;
pub mod journal;
pub mod layout;
pub mod lock;
pub mod summary;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bingo_sdk::{
    Contribution, ErrorCode, Event, Frame, HostHandle, KernelError, Plugin, PluginError,
    PluginManifest, Registrar, Seq, SessionFilter, SessionId, SessionStore, SessionSummary,
};
use jiff::Timestamp;

use crate::gc::Gc;
use crate::lock::Locks;

/// The disk failing is the store's fault, never the kernel's.
pub(crate) fn storage(message: impl Into<String>) -> KernelError {
    KernelError::new(ErrorCode::Storage, message)
}

/// `<data_dir>/sessions`, one directory per session.
#[derive(Debug)]
pub struct JsonlStore {
    root: PathBuf,
    locks: Locks,
}

impl JsonlStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            locks: Locks::default(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory of a session that exists. A directory without a journal
    /// is not a session, whatever else it holds.
    fn existing(&self, session: &SessionId) -> Result<PathBuf, KernelError> {
        let dir = layout::session_dir(&self.root, session);
        if layout::journal(&dir).is_file() {
            Ok(dir)
        } else {
            Err(KernelError::new(
                ErrorCode::SessionNotFound,
                format!("no session {session}"),
            ))
        }
    }
}

#[async_trait]
impl SessionStore for JsonlStore {
    async fn create(&self, summary: &SessionSummary) -> Result<(), KernelError> {
        let dir = layout::session_dir(&self.root, &summary.id);
        journal::create(&dir, &summary.id)?;
        summary::write(&dir, summary)
    }

    async fn append(&self, session: &SessionId, frame: &Frame) -> Result<(), KernelError> {
        // The kernel does not send ephemeral frames; the store is the guard
        // that keeps one out of a journal even so.
        if !frame.event.is_durable() {
            return Ok(());
        }
        let dir = self.existing(session)?;
        journal::append(&dir, frame)?;
        match &frame.event {
            Event::SessionUpdated { summary } => summary::write(&dir, summary),
            _ => Ok(()),
        }
    }

    async fn replay(&self, session: &SessionId, since: Seq) -> Result<Vec<Frame>, KernelError> {
        journal::replay(&self.existing(session)?, since)
    }

    async fn list(&self, filter: &SessionFilter) -> Result<Vec<SessionSummary>, KernelError> {
        summary::list(&self.root, filter)
    }

    async fn delete(&self, session: &SessionId) -> Result<(), KernelError> {
        let dir = self.existing(session)?;
        self.locks.release(session);
        std::fs::remove_dir_all(&dir).map_err(|e| storage(format!("remove {}: {e}", dir.display())))
    }

    /// Take the session for this process. The kernel calls this on create and
    /// on resume: creating a directory claims nothing by itself.
    async fn acquire(&self, session: &SessionId) -> Result<(), KernelError> {
        let dir = self.existing(session)?;
        self.locks.acquire(session, &layout::lock(&dir))
    }

    async fn release(&self, session: &SessionId) -> Result<(), KernelError> {
        self.locks.release(session);
        Ok(())
    }
}

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.store.jsonl",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["store:jsonl"],
    requires: &[],
    config: None,
};

/// Registers the store under `<data_dir>/sessions` and collects what it may
/// when the host starts.
#[derive(Debug, Default)]
pub struct JsonlStorePlugin {
    /// `register` learns the root from the registrar; `start` gets no env, so
    /// the store it built is kept here rather than derived a second time.
    store: OnceLock<Arc<JsonlStore>>,
}

#[async_trait]
impl Plugin for JsonlStorePlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let store = Arc::new(JsonlStore::new(registrar.env().data_dir.join("sessions")));
        // One host registers a plugin once; a second attempt keeps the first.
        let _ = self.store.set(Arc::clone(&store));
        registrar.add(Contribution::Store(store));
        Ok(())
    }

    async fn start(&self, _host: HostHandle) -> Result<(), PluginError> {
        let Some(store) = self.store.get() else {
            return Ok(());
        };
        // Collection that cannot run is not a reason to refuse to start: the
        // sessions are still readable, there are only more of them.
        match Gc::daily(Timestamp::now()).run(store.root()) {
            Ok(removed) if !removed.is_empty() => {
                tracing::info!(count = removed.len(), sessions = ?removed, "collected old sessions");
            }
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "session collection failed"),
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use bingo_sdk::{CloseReason, Level, ParentLink, Usage};

    /// The session the fixtures under `fixtures/` were written for.
    pub(crate) fn session() -> SessionId {
        SessionId::from_raw("ses_01JFIXTURE000000000000000")
    }

    pub(crate) fn stamp() -> Timestamp {
        Timestamp::from_second(1_700_000_000).expect("a timestamp")
    }

    pub(crate) fn summary() -> SessionSummary {
        SessionSummary {
            driver: Default::default(),
            id: session(),
            key: None,
            title: Some("the first name".into()),
            cwd: "/work".into(),
            parent: None,
            model: Some("claude-fable-5".into()),
            provider: Some("anthropic".into()),
            created_at: stamp(),
            updated_at: stamp(),
            usage: Usage::default(),
            busy: false,
        }
    }

    pub(crate) fn frame(seq: u64, event: Event) -> Frame {
        Frame {
            seq: Seq(seq),
            ts: stamp(),
            session: session(),
            cause: None,
            event,
        }
    }

    pub(crate) fn fixture(name: &str) -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    pub(crate) fn root_with() -> (tempfile::TempDir, JsonlStore) {
        let root = tempfile::tempdir().expect("temp root");
        let store = JsonlStore::new(root.path().join("sessions"));
        (root, store)
    }

    /// A session as the kernel leaves one — a journal whose head is the
    /// summary — last written at `when`, which is what `list` stamps as
    /// `updated_at` and what collection measures.
    pub(crate) async fn aged_session(store: &JsonlStore, id: &str, when: Timestamp) {
        let mut summary = summary();
        summary.id = SessionId::from_raw(id);
        store.create(&summary).await.expect("create");
        let mut head = frame(
            1,
            Event::SessionUpdated {
                summary: summary.clone(),
            },
        );
        head.session = summary.id.clone();
        store.append(&summary.id, &head).await.expect("append");
        let dir = layout::session_dir(store.root(), &summary.id);
        let journal = std::fs::File::options()
            .append(true)
            .open(layout::journal(&dir))
            .expect("open the journal");
        journal
            .set_modified(std::time::SystemTime::from(when))
            .expect("set the mtime");
    }

    fn ids(listed: &[SessionSummary]) -> Vec<String> {
        listed.iter().map(|s| s.id.to_string()).collect()
    }

    #[tokio::test]
    async fn a_created_session_replays_what_it_appended() {
        let (_root, store) = root_with();
        store.create(&summary()).await.expect("create");
        store
            .append(
                &session(),
                &frame(1, Event::SessionUpdated { summary: summary() }),
            )
            .await
            .expect("append");
        store
            .append(
                &session(),
                &frame(
                    2,
                    Event::SessionClosed {
                        reason: CloseReason::Client,
                    },
                ),
            )
            .await
            .expect("append");

        let frames = store.replay(&session(), Seq::ZERO).await.expect("replay");
        assert_eq!(
            frames.iter().map(|f| f.seq.0).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(
            store
                .replay(&session(), Seq(1))
                .await
                .expect("replay")
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn an_ephemeral_frame_never_reaches_the_journal() {
        let (_root, store) = root_with();
        store.create(&summary()).await.expect("create");
        let notice = frame(
            1,
            Event::Notice {
                level: Level::Info,
                code: "X".into(),
                text: "not for the journal".into(),
            },
        );
        store.append(&session(), &notice).await.expect("ignored");
        assert!(
            store
                .replay(&session(), Seq::ZERO)
                .await
                .expect("replay")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn an_unknown_session_is_not_found() {
        let (_root, store) = root_with();
        let ghost = SessionId::from_raw("ses_ghost");
        let frame = frame(
            1,
            Event::CatalogChanged {
                kind: "models".into(),
            },
        );
        for code in [
            store.append(&ghost, &frame).await.expect_err("append").code,
            store
                .replay(&ghost, Seq::ZERO)
                .await
                .expect_err("replay")
                .code,
            store.delete(&ghost).await.expect_err("delete").code,
            store.acquire(&ghost).await.expect_err("acquire").code,
        ] {
            assert_eq!(code, ErrorCode::SessionNotFound);
        }
    }

    #[tokio::test]
    async fn a_second_store_on_one_root_cannot_open_a_held_session() {
        let (root, mine) = root_with();
        let theirs = JsonlStore::new(root.path().join("sessions"));
        mine.create(&summary()).await.expect("create");

        mine.acquire(&session()).await.expect("the first holder");
        let err = theirs.acquire(&session()).await.expect_err("held");
        assert_eq!(err.code, ErrorCode::SessionLocked);
        assert!(err.message.contains(session().as_str()), "{err}");

        mine.release(&session()).await.expect("release");
        theirs.acquire(&session()).await.expect("released");
    }

    #[tokio::test]
    async fn the_holder_can_delete_what_it_holds() {
        let (_root, store) = root_with();
        store.create(&summary()).await.expect("create");
        store.acquire(&session()).await.expect("acquire");
        store.delete(&session()).await.expect("delete");
        assert!(
            store
                .list(&SessionFilter::default())
                .await
                .expect("list")
                .is_empty()
        );
        assert_eq!(
            store.delete(&session()).await.expect_err("gone").code,
            ErrorCode::SessionNotFound
        );
    }

    #[tokio::test]
    async fn list_answers_most_recently_updated_first() {
        let (_root, store) = root_with();
        aged_session(&store, "ses_a", stamp()).await;
        aged_session(
            &store,
            "ses_b",
            Timestamp::from_second(stamp().as_second() + 60).expect("later"),
        )
        .await;
        aged_session(
            &store,
            "ses_c",
            Timestamp::from_second(stamp().as_second() - 60).expect("earlier"),
        )
        .await;

        let listed = store.list(&SessionFilter::default()).await.expect("list");
        assert_eq!(ids(&listed), vec!["ses_b", "ses_a", "ses_c"]);
        assert_eq!(
            listed[0].updated_at.as_second(),
            stamp().as_second() + 60,
            "updated_at is the journal's mtime, not the summary's copy"
        );
    }

    #[tokio::test]
    async fn list_honours_cwd_parent_and_limit() {
        let (_root, store) = root_with();
        aged_session(&store, "ses_here", stamp()).await;
        let mut elsewhere = summary();
        elsewhere.id = SessionId::from_raw("ses_there");
        elsewhere.cwd = "/elsewhere".into();
        store.create(&elsewhere).await.expect("create");
        let mut child = summary();
        child.id = SessionId::from_raw("ses_child");
        child.parent = Some(ParentLink {
            session: SessionId::from_raw("ses_here"),
            item: Some(bingo_sdk::ItemId::from_raw("itm_1")),
        });
        store.create(&child).await.expect("create");

        let by_cwd = SessionFilter {
            cwd: Some("/elsewhere".into()),
            ..SessionFilter::default()
        };
        assert_eq!(
            ids(&store.list(&by_cwd).await.expect("list")),
            vec!["ses_there"]
        );

        let by_parent = SessionFilter {
            parent: Some(SessionId::from_raw("ses_here")),
            ..SessionFilter::default()
        };
        assert_eq!(
            ids(&store.list(&by_parent).await.expect("list")),
            vec!["ses_child"]
        );

        let limited = SessionFilter {
            limit: Some(2),
            ..SessionFilter::default()
        };
        assert_eq!(store.list(&limited).await.expect("list").len(), 2);
    }

    #[tokio::test]
    async fn deleting_every_summary_loses_nothing() {
        let (root, store) = root_with();
        aged_session(&store, "ses_a", stamp()).await;
        aged_session(&store, "ses_b", stamp()).await;
        let before = store.list(&SessionFilter::default()).await.expect("list");

        for session in layout::sessions(store.root()).expect("sessions") {
            std::fs::remove_file(layout::summary(&session.dir)).expect("remove the summary");
        }
        let after = store.list(&SessionFilter::default()).await.expect("list");

        assert_eq!(before, after);
        assert!(root.path().join("sessions/ses_a/summary.json").is_file());
    }

    #[tokio::test]
    async fn a_directory_that_is_not_a_session_is_skipped() {
        let (_root, store) = root_with();
        std::fs::create_dir_all(store.root().join("not-a-session")).expect("mkdir");
        assert!(
            store
                .list(&SessionFilter::default())
                .await
                .expect("list")
                .is_empty()
        );
    }

    #[test]
    fn the_manifest_says_what_it_provides() {
        assert_eq!(MANIFEST.id, "bingo.store.jsonl");
        assert_eq!(MANIFEST.provides, ["store:jsonl"]);
        assert!(MANIFEST.config.is_none(), "the store claims no settings");
    }

    #[test]
    fn the_plugin_registers_one_store_under_the_data_directory() {
        let plugin = JsonlStorePlugin::default();
        let env = bingo_sdk::Env::rooted("/home/someone");
        let mut registrar = Registrar::new("bingo.store.jsonl", serde_json::Value::Null, env);
        plugin.register(&mut registrar).expect("register");

        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), 1);
        assert!(matches!(contributions[0], Contribution::Store(_)));
        assert_eq!(
            plugin.store.get().expect("the store").root(),
            Path::new("/home/someone/.bingo/data/sessions")
        );
    }
}

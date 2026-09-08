//! Lifecycle tests use a fake host, not the production actor's mailbox.

use super::testing::Journal;
use super::*;
use crate::memory::{dir, store};
use crate::{InstructionsContributor, MemoryContributor};
use bingo_sdk::{Event, ItemId, TurnId};

struct Fixture {
    config: tempfile::TempDir,
    data: tempfile::TempDir,
    cwd: tempfile::TempDir,
    journal: Journal,
}

impl Fixture {
    fn new() -> Self {
        let cwd = tempfile::tempdir().expect("cwd");
        Self {
            config: tempfile::tempdir().expect("config"),
            data: tempfile::tempdir().expect("data"),
            journal: Journal::at(cwd.path()),
            cwd,
        }
    }

    fn write(&self, text: &str) {
        std::fs::write(self.config.path().join("AGENTS.md"), text).expect("instructions");
        let user = dir::user(self.data.path());
        std::fs::create_dir_all(&user).expect("memory directory");
        std::fs::write(dir::index(&user), text).expect("memory index");
    }

    async fn capture(&self) -> Vec<ContextPiece> {
        let mut pieces = self
            .journal
            .contribute(
                &InstructionsContributor::new(self.config.path().into()),
                self.cwd.path(),
            )
            .await;
        pieces.extend(
            self.journal
                .contribute(
                    &MemoryContributor::new(self.data.path().into()),
                    self.cwd.path(),
                )
                .await,
        );
        pieces
    }

    async fn session(&self, phase: Phase) {
        let cx = HookContext {
            session: self.journal.state().summary.id.clone(),
            turn: None,
            cwd: self.cwd.path().into(),
            provider: None,
            model: None,
            host: self.journal.handle(),
        };
        self.journal.opening(true);
        BaselineHook::new(self.data.path().to_path_buf())
            .on_session(phase, &cx)
            .await;
        self.journal.opening(false);
    }
}

#[tokio::test]
async fn a_replayed_baseline_needs_no_private_contributor_state() {
    let mut fixture = Fixture::new();
    fixture.write("first knowledge");
    let first = fixture.capture().await;
    fixture.write("edited knowledge");
    let state =
        serde_json::from_str(&serde_json::to_string(&*fixture.journal.state()).expect("persist"))
            .expect("restore");
    fixture.journal = Journal::holding(state);
    assert_eq!(fixture.capture().await, first);
    assert_eq!(fixture.journal.state().seq.0, 2);
    assert!(
        fixture.journal.state().items.is_empty(),
        "no user snapshots"
    );
}

/// The surface reads these by name (`tui::memory`), so the shape here is the
/// whole of the contract: two absolute directories under one kind.
#[tokio::test]
async fn session_start_publishes_where_the_memories_are() {
    let fixture = Fixture::new();
    fixture.session(Phase::Start).await;
    let project = crate::memory::project_dir(fixture.data.path(), fixture.cwd.path()).await;
    let published = fixture.journal.state().extensions[PLUGIN][crate::memory::DIRECTORIES].clone();
    assert_eq!(
        published["user"],
        crate::memory::dir::user(fixture.data.path())
            .display()
            .to_string()
    );
    assert_eq!(published["project"], project.display().to_string());
    fixture.session(Phase::End).await;
    assert!(
        !fixture.journal.state().extensions[PLUGIN][crate::memory::DIRECTORIES].is_null(),
        "the end of a session takes nothing away"
    );
}

#[tokio::test]
async fn session_start_invalidates_both_keys_without_opening_the_session() {
    let fixture = Fixture::new();
    fixture.write("first knowledge");
    let first = fixture.capture().await;
    fixture.write("resumed knowledge");
    fixture.session(Phase::End).await;
    assert_eq!(fixture.capture().await, first);
    fixture.session(Phase::Start).await;
    for id in CONTRIBUTORS {
        assert!(fixture.journal.state().extensions[PLUGIN][id].is_null());
    }
    let refreshed = fixture.capture().await;
    assert_ne!(refreshed, first);
    assert_eq!(fixture.capture().await, refreshed);
    assert_eq!(
        fixture.journal.state().seq.0,
        7,
        "capture, invalidate and the directories, recapture"
    );
}

#[tokio::test]
async fn compacted_and_rewound_frames_refresh_only_the_next_assembly() {
    let fixture = Fixture::new();
    fixture.write("initial knowledge");
    let first = fixture.capture().await;
    let initial = fixture.journal.state().extensions.clone();
    fixture.write("after compaction");
    fixture.journal.apply(Event::Compacted {
        generation: 1,
        boundary: ItemId::from_raw("itm_boundary"),
        summary: ItemId::from_raw("itm_summary"),
        kept: Vec::new(),
    });
    assert_eq!(
        fixture.journal.state().extensions,
        initial,
        "refresh stays lazy"
    );
    let compacted = fixture.capture().await;
    assert_ne!(compacted, first);
    let retained = fixture.journal.state().extensions.clone();
    fixture.write("after rewind");
    fixture.journal.apply(Event::Rewound {
        generation: 2,
        to_turn: TurnId::from_raw("trn_previous"),
        dropped: Vec::new(),
        files_restored: Vec::new(),
    });
    assert_eq!(
        fixture.journal.state().extensions,
        retained,
        "refresh stays lazy"
    );
    let rewound = fixture.capture().await;
    assert_ne!(rewound, compacted);
    assert_eq!(fixture.capture().await, rewound);
    assert_eq!(fixture.journal.state().seq.0, 8);
}

#[tokio::test]
async fn disk_changes_remain_hidden_when_compaction_does_not_advance_history() {
    let fixture = Fixture::new();
    fixture.write("retained knowledge");
    let first = fixture.capture().await;
    fixture.write("unaccepted knowledge");
    // A failed or rejected compaction produces no history-advancing frame.
    // Actor tests must prove that acceptance rule; this tests its consumer.
    for phase in [Phase::Start, Phase::End] {
        let cx = HookContext {
            session: fixture.journal.state().summary.id.clone(),
            turn: None,
            cwd: fixture.cwd.path().into(),
            provider: None,
            model: None,
            host: fixture.journal.handle(),
        };
        BaselineHook::new(fixture.data.path().to_path_buf())
            .on_compact(phase, &cx)
            .await;
    }
    assert_eq!(fixture.capture().await, first);
    assert_eq!(fixture.journal.state().seq.0, 2);
}

#[tokio::test]
async fn an_empty_instruction_capture_is_retained_when_a_file_appears() {
    let fixture = Fixture::new();
    let first = fixture.capture().await;
    assert_eq!(first.len(), 3, "only the memory blocks");
    fixture.write("a newly created file");
    assert_eq!(fixture.capture().await, first);
    let instructions: Baseline =
        serde_json::from_value(fixture.journal.state().extensions[PLUGIN][CONTRIBUTORS[0]].clone())
            .expect("an empty capture");
    assert!(instructions.blocks.is_empty());
    assert_eq!(fixture.journal.state().seq.0, 2);
}

#[tokio::test]
async fn baseline_retention_does_not_hide_memory_changes_from_disk_readers() {
    let fixture = Fixture::new();
    fixture.write("old index");
    let first = fixture.capture().await;
    fixture.write("live index");
    assert_eq!(fixture.capture().await, first);
    assert_eq!(
        store::index_text(&dir::user(fixture.data.path())).await,
        "live index"
    );
}

//! Before a tool writes a file: what was there first.
//!
//! The one place a tool is mapped to the field of its input that names the
//! file it writes. A tool this table does not name is not snapshotted, which
//! is why a shell line is not (ADR-0045 §2): `Bash` writes through a program
//! whose arguments name no path this could read.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{Hook, HookContext, HookMatcher, HookOutcome, HookPoint, ToolCall};

use crate::store::Checkpoints;

/// Each tool that writes one file, and the field of its input that names it.
pub const WRITERS: &[(&str, &str)] = &[("Write", "file_path"), ("Edit", "file_path")];

/// The file this call is about to write, absolute. `None` for a call this
/// table does not know, and for one whose field is missing or is not a string
/// — a call the tool itself will refuse.
pub fn written(call: &ToolCall, cwd: &Path) -> Option<PathBuf> {
    let (_, field) = WRITERS.iter().find(|(name, _)| *name == call.name)?;
    let named = call.input.get(field)?.as_str()?;
    Some(resolve(named, cwd))
}

/// An absolute path stands; a relative one hangs off the session's working
/// directory, as the tool that is about to write it resolves one. A path
/// outside the working tree is snapshotted all the same: the fact is the
/// file, not where it is.
fn resolve(named: &str, cwd: &Path) -> PathBuf {
    let path = Path::new(named);
    match path.is_absolute() {
        true => path.to_path_buf(),
        false => cwd.join(path),
    }
}

/// Keeps the file's bytes as they are before the call that changes them.
#[derive(Debug)]
pub struct SnapshotHook {
    store: Arc<Checkpoints>,
}

impl SnapshotHook {
    pub fn new(store: Arc<Checkpoints>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Hook for SnapshotHook {
    fn id(&self) -> &str {
        "bingo.checkpoints.snapshot"
    }

    /// Every tool call, filtered by [`WRITERS`] rather than by the matcher:
    /// the matcher takes one name or one prefix, and this table has two names
    /// that share neither.
    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::BeforeTool],
            tool: None,
        }
    }

    /// A snapshot that could not be taken never stops the edit. A checkpoint
    /// is what makes an edit undoable, not what makes it allowed — refusing
    /// the write because the disk is full would be the worse failure.
    async fn before_tool(&self, call: &mut ToolCall, cx: &HookContext) -> HookOutcome {
        let (Some(turn), Some(path)) = (cx.turn.as_ref(), written(call, &cx.cwd)) else {
            return HookOutcome::Continue;
        };
        if let Err(error) = self.store.snapshot(&cx.session, turn, &path) {
            tracing::warn!(%error, path = %path.display(), "no checkpoint for this file");
        }
        HookOutcome::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, input: serde_json::Value) -> ToolCall {
        ToolCall {
            call_id: "call_1".into(),
            name: name.into(),
            input,
        }
    }

    #[test]
    fn a_relative_path_hangs_off_the_sessions_own_directory() {
        let cwd = Path::new("/work");
        assert_eq!(
            written(&call("Write", json!({"file_path": "src/lib.rs"})), cwd),
            Some(PathBuf::from("/work/src/lib.rs"))
        );
        assert_eq!(
            written(&call("Edit", json!({"file_path": "/etc/hosts"})), cwd),
            Some(PathBuf::from("/etc/hosts")),
            "a path outside the tree is still a file"
        );
    }

    #[test]
    fn a_tool_the_table_does_not_name_writes_nothing_this_can_keep() {
        let cwd = Path::new("/work");
        assert_eq!(
            written(&call("Bash", json!({"command": "rm a"})), cwd),
            None
        );
        assert_eq!(written(&call("Read", json!({"file_path": "a"})), cwd), None);
    }

    #[test]
    fn a_call_whose_field_is_missing_or_is_not_a_string_names_no_file() {
        let cwd = Path::new("/work");
        assert_eq!(written(&call("Write", json!({"content": "x"})), cwd), None);
        assert_eq!(written(&call("Write", json!({"file_path": 7})), cwd), None);
    }

    fn context(dir: &Path) -> HookContext {
        HookContext {
            session: bingo_sdk::SessionId::from_raw("ses_one"),
            turn: Some(bingo_sdk::TurnId::from_raw("trn_1")),
            cwd: dir.to_path_buf(),
            provider: None,
            model: None,
            host: bingo_sdk::testing::NoHost::handle(),
        }
    }

    /// The pre-turn state is one fact, however many tools reach for it.
    #[tokio::test]
    async fn a_write_and_an_edit_of_one_file_in_one_turn_keep_it_once() {
        let dir = tempfile::tempdir().expect("a scratch home");
        let cwd = dir.path();
        let store = Arc::new(Checkpoints::new(&cwd.join("data")));
        let hook = SnapshotHook::new(store.clone());
        let file = cwd.join("note.md");
        std::fs::write(&file, b"original").expect("a file");
        let cx = context(cwd);

        hook.before_tool(&mut call("Write", json!({"file_path": "note.md"})), &cx)
            .await;
        std::fs::write(&file, b"written").expect("the write");
        hook.before_tool(&mut call("Edit", json!({"file_path": "note.md"})), &cx)
            .await;

        let entries = store.entries(&cx.session, &bingo_sdk::TurnId::from_raw("trn_1"));
        assert_eq!(entries.len(), 1, "one file, one snapshot: {entries:?}");
        assert_eq!(
            store
                .bytes(
                    &cx.session,
                    &bingo_sdk::TurnId::from_raw("trn_1"),
                    &entries[0]
                )
                .expect("the bytes"),
            b"original"
        );
    }

    /// A file the turn creates: nothing was there, and going back removes it.
    #[tokio::test]
    async fn a_file_the_turn_creates_is_kept_as_absent() {
        let dir = tempfile::tempdir().expect("a scratch home");
        let store = Arc::new(Checkpoints::new(&dir.path().join("data")));
        let hook = SnapshotHook::new(store.clone());
        let cx = context(dir.path());

        hook.before_tool(&mut call("Write", json!({"file_path": "new.md"})), &cx)
            .await;
        let entries = store.entries(&cx.session, &bingo_sdk::TurnId::from_raw("trn_1"));
        assert_eq!(entries[0].state, crate::store::State::Absent);
    }

    #[tokio::test]
    async fn a_call_outside_a_turn_is_not_a_turns_snapshot() {
        let dir = tempfile::tempdir().expect("a scratch data dir");
        let store = Arc::new(Checkpoints::new(dir.path()));
        let hook = SnapshotHook::new(store.clone());
        let cx = HookContext {
            session: bingo_sdk::SessionId::from_raw("ses_one"),
            turn: None,
            cwd: dir.path().to_path_buf(),
            provider: None,
            model: None,
            host: bingo_sdk::testing::NoHost::handle(),
        };
        let mut written = call("Write", json!({"file_path": "a.txt"}));
        assert_eq!(
            hook.before_tool(&mut written, &cx).await,
            HookOutcome::Continue
        );
        assert!(store.sessions().is_empty());
    }
}

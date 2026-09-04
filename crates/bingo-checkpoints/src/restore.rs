//! Putting the files back.
//!
//! Going back to a turn undoes that turn and every later one, so a file two
//! turns edited is put back to what it was before the *first* of them: the
//! oldest snapshot per file wins. Read everything first, write afterwards —
//! a plan that cannot be read is refused before a byte lands.

use std::io;
use std::path::{Path, PathBuf};

use bingo_sdk::{ErrorCode, KernelError, SessionId, TurnId};

use crate::store::{Checkpoints, Entry, State};

/// What going back to a turn does to the files, before it is done.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// A file and the bytes it had, in index order.
    pub put_back: Vec<(PathBuf, Vec<u8>)>,
    /// A file the turns created; going back removes it.
    pub remove: Vec<PathBuf>,
    /// A file too big to have been kept: it stays as it is, and the reply
    /// says so rather than pretending it was restored.
    pub skipped: Vec<PathBuf>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.put_back.is_empty() && self.remove.is_empty() && self.skipped.is_empty()
    }
}

/// The oldest entry per file across `turns`, in the order the turns are
/// given, each beside the turn whose directory holds its bytes. Pure: what
/// the files were before any of these turns ran.
pub fn earliest(turns: Vec<(TurnId, Vec<Entry>)>) -> Vec<(TurnId, Entry)> {
    let mut out: Vec<(TurnId, Entry)> = Vec::new();
    for (turn, entries) in turns {
        for entry in entries {
            if !out.iter().any(|(_, kept)| kept.path == entry.path) {
                out.push((turn.clone(), entry));
            }
        }
    }
    out
}

/// What the files looked like before `turns`, read out of the store. Oldest
/// turn first; a snapshot that cannot be read refuses the whole plan, because
/// half a rewind is worse than none.
pub fn plan(
    store: &Checkpoints,
    session: &SessionId,
    turns: &[TurnId],
) -> Result<Plan, KernelError> {
    let kept = turns
        .iter()
        .map(|turn| (turn.clone(), store.entries(session, turn)))
        .collect();
    let mut plan = Plan::default();
    for (turn, entry) in earliest(kept) {
        match entry.state {
            State::Present => plan
                .put_back
                .push((entry.path.clone(), bytes(store, session, &turn, &entry)?)),
            State::Absent => plan.remove.push(entry.path),
            State::Skipped => plan.skipped.push(entry.path),
        }
    }
    Ok(plan)
}

fn bytes(
    store: &Checkpoints,
    session: &SessionId,
    turn: &TurnId,
    entry: &Entry,
) -> Result<Vec<u8>, KernelError> {
    store.bytes(session, turn, entry).map_err(|e| {
        KernelError::new(
            ErrorCode::Storage,
            format!(
                "the checkpoint of {} cannot be read ({e}); nothing was rewound",
                entry.path.display()
            ),
        )
    })
}

/// Put the plan into the working tree. The first file that will not move
/// stops it and says which one; whatever landed before it stays, and the
/// journal has not been touched.
pub fn apply(plan: &Plan) -> Result<(), KernelError> {
    for (path, bytes) in &plan.put_back {
        write(path, bytes).map_err(|e| failed(path, e))?;
    }
    for path in &plan.remove {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(failed(path, e)),
        }
    }
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

fn failed(path: &Path, error: io::Error) -> KernelError {
    KernelError::new(
        ErrorCode::Storage,
        format!(
            "{} could not be put back ({error}); the conversation is unchanged",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(n: u32, state: State, path: &str) -> Entry {
        Entry {
            n,
            state,
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn the_oldest_snapshot_of_a_file_is_the_one_that_wins() {
        let first = TurnId::from_raw("trn_1");
        let second = TurnId::from_raw("trn_2");
        let chosen = earliest(vec![
            (
                first.clone(),
                vec![
                    entry(1, State::Absent, "/work/a"),
                    entry(2, State::Present, "/work/b"),
                ],
            ),
            (
                second.clone(),
                vec![
                    entry(1, State::Present, "/work/a"),
                    entry(2, State::Present, "/work/c"),
                ],
            ),
        ]);
        assert_eq!(
            chosen,
            vec![
                (first.clone(), entry(1, State::Absent, "/work/a")),
                (first, entry(2, State::Present, "/work/b")),
                (second, entry(2, State::Present, "/work/c")),
            ],
            "a file the older turn created goes back to not being there"
        );
    }

    #[test]
    fn no_turns_is_an_empty_plan_and_not_an_error() {
        assert!(earliest(Vec::new()).is_empty());
        assert!(Plan::default().is_empty());
    }

    fn store() -> (tempfile::TempDir, Checkpoints) {
        let dir = tempfile::tempdir().expect("a scratch data dir");
        let store = Checkpoints::new(dir.path());
        (dir, store)
    }

    #[test]
    fn a_plan_reads_the_bytes_of_the_turn_that_kept_them() {
        let (dir, store) = store();
        let session = SessionId::from_raw("ses_one");
        let first = TurnId::from_raw("trn_1");
        let second = TurnId::from_raw("trn_2");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"one").expect("a file");
        store.snapshot(&session, &first, &file).expect("kept");
        std::fs::write(&file, b"two").expect("an edit");
        store.snapshot(&session, &second, &file).expect("kept");
        let gone = dir.path().join("made.txt");
        store.snapshot(&session, &second, &gone).expect("kept");
        std::fs::write(&gone, b"new").expect("created in the turn");

        let plan = plan(&store, &session, &[first, second]).expect("a plan");
        assert_eq!(plan.put_back, vec![(file.clone(), b"one".to_vec())]);
        assert_eq!(plan.remove, vec![gone.clone()]);

        apply(&plan).expect("applied");
        assert_eq!(std::fs::read(&file).expect("read back"), b"one");
        assert!(!gone.exists());
    }

    #[test]
    fn a_snapshot_whose_bytes_are_gone_refuses_the_whole_plan() {
        let (dir, store) = store();
        let session = SessionId::from_raw("ses_one");
        let turn = TurnId::from_raw("trn_1");
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"one").expect("a file");
        store.snapshot(&session, &turn, &file).expect("kept");
        std::fs::remove_file(dir.path().join("checkpoints/ses_one/trn_1/1.snap"))
            .expect("the snapshot goes missing");

        let refused = plan(&store, &session, &[turn]).expect_err("nothing to put back");
        assert_eq!(refused.code, ErrorCode::Storage);
        assert!(refused.message.contains("nothing was rewound"));
    }
}

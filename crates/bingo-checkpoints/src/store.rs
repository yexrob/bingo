//! Where a file's bytes before a turn are kept.
//!
//! `<data_dir>/checkpoints/<session>/<turn>/` holds one numbered `.snap` per
//! file and one `index` naming them. A line is `<n> <state> <path>`: the path
//! is last because a path may contain a space and a state may not.
//!
//! One snapshot per file per turn — the fact is what the file was before the
//! turn, and the second edit of a turn is not a second fact.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use bingo_sdk::{ErrorCode, KernelError, SessionId, TurnId};

/// The most a file may be before its bytes are recorded rather than copied.
/// A checkpoint directory is not a backup and never grows past what a turn
/// could plausibly have edited.
pub const MOST: u64 = 8 * 1024 * 1024;

const INDEX: &str = "index";

/// What was at a path when the turn opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// A file, copied beside this entry.
    Present,
    /// Nothing was there; going back removes whatever is there now.
    Absent,
    /// Too big, or not a file: nothing was copied and nothing is put back.
    Skipped,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Present => "present",
            State::Absent => "absent",
            State::Skipped => "skipped",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        match word {
            "present" => Some(State::Present),
            "absent" => Some(State::Absent),
            "skipped" => Some(State::Skipped),
            _ => None,
        }
    }
}

/// One file, as one turn found it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub n: u32,
    pub state: State,
    pub path: PathBuf,
}

impl Entry {
    /// The index line: everything after the second space is the path.
    fn line(&self) -> String {
        format!(
            "{} {} {}\n",
            self.n,
            self.state.as_str(),
            self.path.display()
        )
    }

    fn parse(line: &str) -> Option<Self> {
        let (n, rest) = line.split_once(' ')?;
        let (state, path) = rest.split_once(' ')?;
        (!path.is_empty()).then_some(Entry {
            n: n.parse().ok()?,
            state: State::parse(state)?,
            path: PathBuf::from(path),
        })
    }
}

/// Where the nth snapshot of a turn's bytes lives.
fn snap(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("{n}.snap"))
}

fn storage(what: String) -> KernelError {
    KernelError::new(ErrorCode::Storage, what)
}

/// The directory of checkpoints, and the one lock that keeps two tools of one
/// turn from writing the same index line twice.
#[derive(Debug)]
pub struct Checkpoints {
    root: PathBuf,
    writing: Mutex<()>,
}

impl Checkpoints {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("checkpoints"),
            writing: Mutex::new(()),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn turn_dir(&self, session: &SessionId, turn: &TurnId) -> PathBuf {
        self.root.join(session.as_str()).join(turn.as_str())
    }

    /// What this turn has already been asked to keep, in the order it was
    /// asked. A turn nothing was written in has nothing.
    pub fn entries(&self, session: &SessionId, turn: &TurnId) -> Vec<Entry> {
        index(&self.turn_dir(session, turn))
    }

    /// The bytes an entry stands for. Only a `Present` entry has any.
    pub fn bytes(&self, session: &SessionId, turn: &TurnId, entry: &Entry) -> io::Result<Vec<u8>> {
        std::fs::read(snap(&self.turn_dir(session, turn), entry.n))
    }

    /// Keep `path` as it is now, unless this turn already kept it. Answers
    /// whether anything was written down.
    pub fn snapshot(
        &self,
        session: &SessionId,
        turn: &TurnId,
        path: &Path,
    ) -> Result<bool, KernelError> {
        // A path the index cannot write back byte for byte is one this cannot
        // put back either: `Display` replaces whatever is not UTF-8. Keeping
        // nothing is better than restoring a file nobody named.
        if path.to_str().is_none() {
            tracing::warn!(path = %path.display(), "no checkpoint: that path is not utf-8");
            return Ok(false);
        }
        let dir = self.turn_dir(session, turn);
        let held = self.writing.lock().unwrap_or_else(|held| held.into_inner());
        let entries = index(&dir);
        if entries.iter().any(|entry| entry.path == path) {
            return Ok(false);
        }
        std::fs::create_dir_all(&dir)
            .map_err(|e| storage(format!("create {}: {e}", dir.display())))?;
        let n = entries.len() as u32 + 1;
        let entry = Entry {
            n,
            state: keep(path, &snap(&dir, n))?,
            path: path.to_path_buf(),
        };
        append(&dir.join(INDEX), &entry.line())?;
        drop(held);
        Ok(true)
    }

    /// Every session with checkpoints here, by id.
    pub fn sessions(&self) -> Vec<String> {
        let Ok(read) = std::fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut out: Vec<String> = read
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        out.sort();
        out
    }

    /// Everything kept for one session, gone.
    pub fn forget(&self, session: &str) -> Result<(), KernelError> {
        let dir = self.root.join(session);
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(storage(format!("remove {}: {e}", dir.display()))),
        }
    }

    /// Collect the checkpoints of every session that is no longer there; the
    /// ids that went. A session outlives its snapshots, never the other way
    /// round (ADR-0045 §4).
    pub fn collect(&self, kept: &[String]) -> Vec<String> {
        let gone = condemned(&self.sessions(), kept);
        for session in &gone {
            if let Err(error) = self.forget(session) {
                tracing::warn!(%error, session, "checkpoints outlived their session");
            }
        }
        gone
    }
}

/// The sessions here that `kept` does not name.
fn condemned(present: &[String], kept: &[String]) -> Vec<String> {
    present
        .iter()
        .filter(|session| !kept.iter().any(|alive| alive == *session))
        .cloned()
        .collect()
}

/// Copy the file's bytes beside its entry, and say what was there. A file
/// over [`MOST`], and anything that is not a file, is recorded and not copied.
fn keep(path: &Path, snap: &Path) -> Result<State, KernelError> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(State::Absent),
        Err(e) => return Err(storage(format!("read {}: {e}", path.display()))),
    };
    if !meta.is_file() || meta.len() > MOST {
        return Ok(State::Skipped);
    }
    std::fs::copy(path, snap)
        .map(|_| State::Present)
        .map_err(|e| storage(format!("copy {}: {e}", path.display())))
}

/// The index as it stands. A line this crate cannot read is one file it
/// cannot put back, not a directory it refuses to read at all.
fn index(dir: &Path) -> Vec<Entry> {
    let Ok(text) = std::fs::read_to_string(dir.join(INDEX)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.is_empty())
        .filter_map(|line| match Entry::parse(line) {
            Some(entry) => Some(entry),
            None => {
                tracing::warn!(line, "a checkpoint index line nothing can read");
                None
            }
        })
        .collect()
}

fn append(path: &Path, line: &str) -> Result<(), KernelError> {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .map_err(|e| storage(format!("write {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, Checkpoints) {
        let dir = tempfile::tempdir().expect("a scratch data dir");
        let store = Checkpoints::new(dir.path());
        (dir, store)
    }

    fn session() -> SessionId {
        SessionId::from_raw("ses_one")
    }

    fn turn(id: &str) -> TurnId {
        TurnId::from_raw(id)
    }

    #[test]
    fn an_index_line_keeps_a_path_with_spaces_in_it() {
        let entry = Entry {
            n: 3,
            state: State::Present,
            path: PathBuf::from("/work/my notes/a b.md"),
        };
        assert_eq!(entry.line(), "3 present /work/my notes/a b.md\n");
        assert_eq!(Entry::parse(entry.line().trim_end()), Some(entry));
    }

    #[test]
    fn a_line_nothing_can_read_is_skipped_and_the_rest_stand() {
        let (_dir, store) = store();
        let dir = store.turn_dir(&session(), &turn("trn_1"));
        std::fs::create_dir_all(&dir).expect("a turn dir");
        std::fs::write(
            dir.join(INDEX),
            "1 present /work/a\nrubbish\n2 absent /work/b\n",
        )
        .expect("an index");
        let entries = store.entries(&session(), &turn("trn_1"));
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].state, State::Absent);
    }

    #[test]
    fn the_first_snapshot_of_a_turn_wins_and_the_second_is_not_taken() {
        let (dir, store) = store();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"before").expect("a file");

        assert!(
            store
                .snapshot(&session(), &turn("trn_1"), &file)
                .expect("a snapshot")
        );
        std::fs::write(&file, b"after").expect("an edit");
        assert!(
            !store
                .snapshot(&session(), &turn("trn_1"), &file)
                .expect("not again"),
            "the pre-turn state is the fact, and it is already kept"
        );

        let entries = store.entries(&session(), &turn("trn_1"));
        assert_eq!(entries.len(), 1);
        assert_eq!(
            store
                .bytes(&session(), &turn("trn_1"), &entries[0])
                .expect("the bytes"),
            b"before"
        );
    }

    #[test]
    fn a_file_that_is_not_there_yet_is_recorded_as_absent() {
        let (dir, store) = store();
        let file = dir.path().join("new.txt");
        store
            .snapshot(&session(), &turn("trn_1"), &file)
            .expect("a snapshot");
        let entries = store.entries(&session(), &turn("trn_1"));
        assert_eq!(entries[0].state, State::Absent);
        assert!(!snap(&store.turn_dir(&session(), &turn("trn_1")), entries[0].n).exists());
    }

    #[test]
    fn a_file_over_the_cap_is_recorded_and_not_copied() {
        let (dir, store) = store();
        let file = dir.path().join("big.bin");
        std::fs::write(&file, vec![0u8; MOST as usize + 1]).expect("a big file");
        store
            .snapshot(&session(), &turn("trn_1"), &file)
            .expect("a snapshot");
        assert_eq!(
            store.entries(&session(), &turn("trn_1"))[0].state,
            State::Skipped
        );
    }

    #[test]
    fn a_directory_where_a_file_was_asked_for_is_skipped_not_copied() {
        let (dir, store) = store();
        let target = dir.path().join("a-directory");
        std::fs::create_dir(&target).expect("a directory");
        store
            .snapshot(&session(), &turn("trn_1"), &target)
            .expect("a snapshot");
        assert_eq!(
            store.entries(&session(), &turn("trn_1"))[0].state,
            State::Skipped
        );
    }

    #[test]
    fn a_session_that_is_gone_takes_its_checkpoints_with_it() {
        let (dir, store) = store();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, b"x").expect("a file");
        for id in ["ses_one", "ses_two"] {
            store
                .snapshot(&SessionId::from_raw(id), &turn("trn_1"), &file)
                .expect("a snapshot");
        }
        assert_eq!(store.sessions(), ["ses_one", "ses_two"]);
        assert_eq!(store.collect(&["ses_two".to_string()]), ["ses_one"]);
        assert_eq!(store.sessions(), ["ses_two"]);
    }

    /// Only a unix path can hold bytes that are not UTF-8; on Windows every
    /// path is UTF-16 and this cannot arise. The file need not exist — the
    /// path is refused before anything on disk is looked at, because it is
    /// the *name* that could not be written back.
    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf_8_is_not_kept_rather_than_kept_wrongly() {
        use std::os::unix::ffi::OsStrExt;
        let (dir, store) = store();
        let path = dir.path().join(std::ffi::OsStr::from_bytes(b"bad\xff.txt"));
        assert!(
            !store
                .snapshot(&session(), &turn("trn_1"), &path)
                .expect("no snapshot, and no failure")
        );
        assert!(store.entries(&session(), &turn("trn_1")).is_empty());
    }

    #[test]
    fn forgetting_a_session_that_kept_nothing_is_not_a_failure() {
        let (_dir, store) = store();
        store.forget("ses_never").expect("nothing to remove");
    }
}

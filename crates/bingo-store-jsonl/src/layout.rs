//! Where a session lives on disk: one directory per session, named by the id
//! the kernel minted, holding the journal, its lock and the derived summary
//! (ADR-0005). Nothing here reads a file's contents.

use std::path::{Path, PathBuf};

use bingo_sdk::{KernelError, SessionId};
use jiff::Timestamp;

use crate::storage;

/// The session itself; everything else in the directory derives from it.
pub const JOURNAL: &str = "journal.jsonl";
/// The only claim of ownership. Data files are never locked.
pub const LOCK: &str = ".lock";
pub const SUMMARY: &str = "summary.json";
pub const SUMMARY_TMP: &str = "summary.json.tmp";
/// When collection last ran, in its mtime.
pub const GC_STAMP: &str = "gc.stamp";

pub fn session_dir(root: &Path, session: &SessionId) -> PathBuf {
    root.join(session.as_str())
}

pub fn journal(dir: &Path) -> PathBuf {
    dir.join(JOURNAL)
}

pub fn lock(dir: &Path) -> PathBuf {
    dir.join(LOCK)
}

pub fn summary(dir: &Path) -> PathBuf {
    dir.join(SUMMARY)
}

pub fn summary_tmp(dir: &Path) -> PathBuf {
    dir.join(SUMMARY_TMP)
}

pub fn gc_stamp(root: &Path) -> PathBuf {
    root.join(GC_STAMP)
}

/// A session directory and when its journal was last appended to — the one
/// fact `updated_at` and collection both read.
#[derive(Clone, Debug)]
pub struct Session {
    pub id: SessionId,
    pub dir: PathBuf,
    pub updated_at: Timestamp,
}

/// Every session on disk, in id order — minting order, since an id is a ULID.
/// A directory without a journal is not a session; a root that no run has
/// created yet holds none.
pub fn sessions(root: &Path) -> Result<Vec<Session>, KernelError> {
    let mut out = Vec::new();
    for (id, dir) in directories(root)? {
        let path = journal(&dir);
        if !path.is_file() {
            continue;
        }
        out.push(Session {
            id,
            updated_at: modified(&path)?,
            dir,
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

fn directories(root: &Path) -> Result<Vec<(SessionId, PathBuf)>, KernelError> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(storage(format!("read {}: {e}", root.display()))),
    };
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| storage(format!("read {}: {e}", root.display())))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            out.push((SessionId::from_raw(name), path));
        }
    }
    Ok(out)
}

/// A file's mtime as the kernel's clock reads it.
pub fn modified(path: &Path) -> Result<Timestamp, KernelError> {
    let system = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .map_err(|e| storage(format!("stat {}: {e}", path.display())))?;
    Timestamp::try_from(system).map_err(|e| storage(format!("mtime of {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_is_a_directory_named_by_its_id() {
        let root = Path::new("/data/sessions");
        let dir = session_dir(root, &SessionId::from_raw("ses_01H"));
        assert_eq!(dir, Path::new("/data/sessions/ses_01H"));
        assert_eq!(
            journal(&dir),
            Path::new("/data/sessions/ses_01H/journal.jsonl")
        );
        assert_eq!(lock(&dir), Path::new("/data/sessions/ses_01H/.lock"));
        assert_eq!(
            summary(&dir),
            Path::new("/data/sessions/ses_01H/summary.json")
        );
    }

    #[test]
    fn a_root_that_does_not_exist_holds_no_sessions() {
        let sessions = sessions(Path::new("/nowhere/bingo/sessions")).expect("no root, no error");
        assert!(sessions.is_empty());
    }

    #[test]
    fn a_directory_without_a_journal_is_not_a_session() {
        let root = tempfile::tempdir().expect("temp root");
        std::fs::create_dir(root.path().join("ses_a")).expect("mkdir");
        std::fs::create_dir(root.path().join("ses_b")).expect("mkdir");
        std::fs::write(root.path().join("ses_b").join(JOURNAL), b"{}\n").expect("write");
        let found = sessions(root.path()).expect("list");
        assert_eq!(
            found.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["ses_b"]
        );
    }
}

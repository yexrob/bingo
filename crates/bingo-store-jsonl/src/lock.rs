//! The `.lock` sidecar: an advisory exclusive lock on an empty file beside
//! the journal, and the only claim of ownership over a session (ADR-0005).
//! Data files are never locked, so a session directory stays readable and
//! copyable while a process owns it.

use std::collections::BTreeMap;
use std::fs::{File, TryLockError};
use std::path::Path;
use std::sync::Mutex;

use bingo_sdk::{ErrorCode, KernelError, SessionId};

use crate::storage;

/// The locks this store holds. The open file is what the OS watches, so it is
/// kept here until `release` drops it or the process exits.
#[derive(Debug, Default)]
pub struct Locks {
    held: Mutex<BTreeMap<SessionId, File>>,
}

impl Locks {
    pub fn acquire(&self, session: &SessionId, path: &Path) -> Result<(), KernelError> {
        let mut held = self.held();
        if held.contains_key(session) {
            return Err(taken(session));
        }
        match take(path)? {
            Some(file) => {
                held.insert(session.clone(), file);
                Ok(())
            }
            None => Err(taken(session)),
        }
    }

    /// Closing the file releases the lock; so does exiting, which is why a
    /// crash never leaves a session claimed.
    pub fn release(&self, session: &SessionId) {
        self.held().remove(session);
    }

    fn held(&self) -> std::sync::MutexGuard<'_, BTreeMap<SessionId, File>> {
        // The map holds open files; a poisoned lock has nothing to protect.
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Try the exclusive lock: `None` when another holder — in this process or
/// another — has it. The caller owns the returned file and, with it, the lock.
pub fn take(path: &Path) -> Result<Option<File>, KernelError> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| storage(format!("open {}: {e}", path.display())))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(e)) => Err(storage(format!("lock {}: {e}", path.display()))),
    }
}

fn taken(session: &SessionId) -> KernelError {
    KernelError::new(
        ErrorCode::SessionLocked,
        format!("session {session} is open elsewhere"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionId {
        SessionId::from_raw("ses_locked")
    }

    #[test]
    fn a_second_holder_is_told_the_session_is_open() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(".lock");
        let mine = Locks::default();
        let theirs = Locks::default();

        mine.acquire(&session(), &path).expect("the first holder");
        let err = theirs
            .acquire(&session(), &path)
            .expect_err("the second holder waits for nobody");
        assert_eq!(err.code, ErrorCode::SessionLocked);
        assert!(err.message.contains("open elsewhere"), "{err}");

        mine.release(&session());
        theirs.acquire(&session(), &path).expect("released");
    }

    #[test]
    fn one_store_does_not_take_a_session_twice() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(".lock");
        let locks = Locks::default();
        locks.acquire(&session(), &path).expect("the first take");
        assert_eq!(
            locks
                .acquire(&session(), &path)
                .expect_err("already mine")
                .code,
            ErrorCode::SessionLocked
        );
    }

    #[test]
    fn a_free_lock_can_be_taken_and_given_back() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join(".lock");
        let file = take(&path).expect("no error").expect("free");
        assert!(take(&path).expect("no error").is_none(), "held");
        drop(file);
        assert!(take(&path).expect("no error").is_some(), "free again");
    }

    #[test]
    fn releasing_what_nobody_holds_is_not_an_error() {
        Locks::default().release(&session());
    }
}

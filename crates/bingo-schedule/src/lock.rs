//! One runner per store, held by the OS rather than a process-id sentinel.
//!
//! The published inode is permanent: unlinking it would let another runner
//! lock a replacement while the first still owns the original. Its immutable
//! nonnumeric marker also keeps older gateway doctors from removing it.

mod publish;

use std::fs::{File, TryLockError};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const LOCK: &str = "runner.lock";

/// The on-disk protocol marker, not evidence that any runner is alive.
pub const MARKER: &str = "bingo-schedule-runner-v1\n";

#[derive(Debug, thiserror::Error)]
pub enum ClaimError {
    #[error("another runner holds {}", path.display())]
    WouldBlock { path: PathBuf },
    #[error(
        "legacy or unrecognized runner lock ({}); stop all bingo processes sharing this store, run one `bingo gateway doctor --fix` for a dead PID sentinel, then restart; otherwise inspect the file before removing it",
        path.display()
    )]
    Legacy { path: PathBuf },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl ClaimError {
    fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Closing this file releases ownership, including after a process crash.
#[derive(Debug)]
pub struct Claim {
    path: PathBuf,
    _file: File,
}

impl Claim {
    /// Take the OS lock only on a recognized modern inode. Older binaries do
    /// not honor OS locks, so their sentinels must never be adopted in place.
    pub fn take(dir: &Path) -> Result<Self, ClaimError> {
        std::fs::create_dir_all(dir).map_err(|e| ClaimError::io(dir, e))?;
        let path = dir.join(LOCK);
        let mut file = publish::open(&path).map_err(|e| ClaimError::io(&path, e))?;
        acquire(&file, &path)?;
        recognize(&mut file, &path)?;
        Ok(Self { path, _file: file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn recognize(file: &mut File, path: &Path) -> Result<(), ClaimError> {
    let mut bytes = Vec::new();
    file.take(MARKER.len() as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ClaimError::io(path, e))?;
    match bytes == MARKER.as_bytes() {
        true => Ok(()),
        false => Err(ClaimError::Legacy {
            path: path.to_path_buf(),
        }),
    }
}

fn acquire(file: &File, path: &Path) -> Result<(), ClaimError> {
    match file.try_lock() {
        Ok(()) => Ok(()),
        Err(TryLockError::WouldBlock) => Err(ClaimError::WouldBlock {
            path: path.to_path_buf(),
        }),
        Err(TryLockError::Error(error)) => Err(ClaimError::io(path, error)),
    }
}

/// Inspect ownership without creating, changing or unlinking anything.
/// `true` means an OS lock is held; `false` means missing or unlocked.
/// An unlocked file is briefly locked, then closed before this returns.
pub fn probe(dir: &Path) -> Result<bool, ClaimError> {
    let path = dir.join(LOCK);
    let mut file = match File::options().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(ClaimError::io(&path, e)),
    };
    match acquire(&file, &path) {
        Ok(()) => {
            recognize(&mut file, &path)?;
            Ok(false)
        }
        Err(ClaimError::WouldBlock { .. }) => Ok(true),
        Err(e) => Err(e),
    }
}

/// The same ownership line for commands, tools and runner notices.
pub fn holder(dir: &Path, held: bool) -> String {
    if held {
        return "held by this process".into();
    }
    match probe(dir) {
        Ok(true) => format!(
            "standby — another runner holds this store ({})",
            dir.join(LOCK).display()
        ),
        Ok(false) => "standby — no runner holds this store; waiting to take over".into(),
        Err(error @ ClaimError::Legacy { .. }) => format!("standby — {error}"),
        Err(error) => format!("standby — cannot inspect the runner lock: {error}"),
    }
}

#[cfg(test)]
mod tests;

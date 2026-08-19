//! Rewind (D91): the file snapshots a turn takes before it changes anything,
//! and the machinery that puts code and conversation back where they were.
//!
//! A checkpoint is a turn-opening user message, addressed by the transcript
//! line it was written on — the only identity a history of `{role, content}`
//! offers, and a stable one, because the transcript is append-only and rewind
//! is the single operation that ever shortens it.
//!
//! Snapshots live under the session's own directory, one directory per
//! checkpoint, and are bounded: a session that edits for long enough evicts its
//! oldest checkpoints rather than growing without limit. Nothing here is git —
//! a repository may not exist, and the working tree is not ours to commit.
//!
//! What is *not* covered: anything a `Bash` command writes. A shell can change
//! any file in any way, and there is no pre-image to take before it does.
//! Restoring code therefore restores what `Edit` and `Write` did, which is what
//! the snapshot store saw.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use thiserror::Error;

use crate::error::ErrorCode;

/// A checkpoint's identity: the transcript line its user message was written on.
pub type CheckpointId = usize;

/// Total snapshot bytes kept per session. Past it, whole checkpoints are
/// evicted oldest first — an old pre-image is the one whose turn the user is
/// least likely to still be reconsidering.
pub const MAX_STORE_BYTES: u64 = 50 * 1024 * 1024;

/// Checkpoints kept per session, whatever they weigh.
pub const MAX_CHECKPOINTS: usize = 200;

/// Largest single file snapshotted. A pre-image bigger than this costs more to
/// keep than the rewind it would enable is worth; the edit still happens, and
/// the file is reported as unsnapshotted.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum RewindError {
    #[error("rewind io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0} is too large to snapshot ({1} bytes)")]
    TooLarge(PathBuf, u64),
}

impl ErrorCode for RewindError {
    fn error_code(&self) -> &'static str {
        "STORAGE_ERROR"
    }
}

/// This session's snapshot root: `~/.local/share/bingo/rewind/<session>`.
pub fn session_dir(home: &Path, session: &str) -> PathBuf {
    crate::storage::rewind_dir(home).join(session)
}

/// The open checkpoint and where its snapshots go. Shared by the session and
/// every tool call in it; a recorder with nothing open snapshots nothing, which
/// is what every host without a transcript gets.
#[derive(Debug, Default)]
pub struct Recorder {
    open: Mutex<Option<Open>>,
}

#[derive(Debug, Clone)]
struct Open {
    dir: PathBuf,
    checkpoint: CheckpointId,
}

impl Recorder {
    /// Start recording against a new checkpoint, and bring the store back
    /// inside its bounds while we are here (once a turn, not once an edit).
    pub fn open(&self, dir: PathBuf, checkpoint: CheckpointId) {
        evict(&dir);
        *self.open.lock().unwrap_or_else(|error| error.into_inner()) =
            Some(Open { dir, checkpoint });
    }

    /// Stop recording: snapshots become no-ops until the next checkpoint opens.
    pub fn close(&self) {
        *self.open.lock().unwrap_or_else(|error| error.into_inner()) = None;
    }

    fn current(&self) -> Option<Open> {
        self.open
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    /// Record `path` as it is right now, once per (checkpoint, path): the first
    /// pre-image of a turn is the one the turn started from, and the second
    /// edit of the same file must not overwrite it with an already-changed one.
    ///
    /// A recorder with no open checkpoint does nothing: snapshotting is a
    /// service to the user, not a precondition of editing. Nor does a failure
    /// stop the edit — it is written down as a miss, so the rewind selector can
    /// say which files it will not be able to put back before the user picks.
    pub fn snapshot(&self, path: &Path) {
        let Some(open) = self.current() else {
            return;
        };
        if snapshot(&open.dir, open.checkpoint, path).is_err() {
            let _ = miss(&open.dir, open.checkpoint, path);
        }
    }
}

/// Checkpoint directory name: zero-padded so a plain directory listing sorts
/// the way the turns happened.
fn checkpoint_name(checkpoint: CheckpointId) -> String {
    format!("{checkpoint:012}")
}

/// FNV-1a over the path, so a snapshot's file name is short and stable whatever
/// the path's length. Collisions are resolved by probing, not assumed away —
/// the `.path` sidecar carries the real path and is the record of truth.
fn path_hash(path: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in path.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Take one pre-image. Writing the bytes first and the `.path` sidecar second
/// makes the sidecar the commit: a crash between them leaves an orphan `.pre`
/// that no restore reads, never a file wrongly marked as created-by-a-tool.
pub fn snapshot(dir: &Path, checkpoint: CheckpointId, path: &Path) -> Result<(), RewindError> {
    let target = path.to_string_lossy().to_string();
    let dir = dir.join(checkpoint_name(checkpoint));
    std::fs::create_dir_all(&dir)?;
    let Some(stem) = free_stem(&dir, &target)? else {
        return Ok(());
    };
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > MAX_FILE_BYTES => {
            return Err(RewindError::TooLarge(path.to_path_buf(), meta.len()));
        }
        // The file exists: its bytes are the pre-image.
        Ok(_) => {
            let bytes = std::fs::read(path)?;
            std::fs::write(dir.join(format!("{stem}.pre")), bytes)?;
        }
        // It does not: the marker is the absence of a `.pre`, and restoring it
        // means removing the file the tool is about to create.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(RewindError::Io(error)),
    }
    std::fs::write(dir.join(format!("{stem}.path")), &target)?;
    Ok(())
}

/// Every checkpoint directory in the store, oldest first.
fn checkpoints(dir: &Path) -> Vec<(CheckpointId, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<(CheckpointId, PathBuf)> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let path = entry.path();
            let id = path.file_name()?.to_str()?.parse::<CheckpointId>().ok()?;
            path.is_dir().then_some((id, path))
        })
        .collect();
    found.sort_by_key(|(id, _)| *id);
    found
}

/// What one sidecar records about a file at a checkpoint.
enum Pre {
    /// The file existed; these are its bytes.
    Bytes(PathBuf, PathBuf),
    /// The file did not exist — the tool created it, so putting it back means
    /// removing it.
    Created(PathBuf),
    /// The pre-image could not be taken (unreadable, or past
    /// [`MAX_FILE_BYTES`]). The edit went ahead; this file cannot be restored.
    Missed(PathBuf),
}

impl Pre {
    fn path(&self) -> &Path {
        match self {
            Pre::Bytes(path, _) | Pre::Created(path) | Pre::Missed(path) => path,
        }
    }
}

/// The pre-images one checkpoint directory holds.
fn pre_images(dir: &Path) -> Vec<Pre> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<Pre> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "path"))
        .filter_map(|sidecar| {
            let target = PathBuf::from(std::fs::read_to_string(&sidecar).ok()?);
            let bytes = sidecar.with_extension("pre");
            if sidecar.with_extension("miss").exists() {
                return Some(Pre::Missed(target));
            }
            Some(match bytes.exists() {
                true => Pre::Bytes(target, bytes),
                false => Pre::Created(target),
            })
        })
        .collect();
    found.sort_by(|a, b| a.path().cmp(b.path()));
    found
}

/// Record that a file changed at this checkpoint without a pre-image. The
/// sidecar is written the same way and probed the same way, so a second edit of
/// the same file in the same turn does not retry a snapshot that already failed.
fn miss(dir: &Path, checkpoint: CheckpointId, path: &Path) -> Result<(), RewindError> {
    let target = path.to_string_lossy().to_string();
    let dir = dir.join(checkpoint_name(checkpoint));
    std::fs::create_dir_all(&dir)?;
    let stem = free_stem(&dir, &target)?;
    let Some(stem) = stem else {
        return Ok(());
    };
    std::fs::write(dir.join(format!("{stem}.miss")), [])?;
    std::fs::write(dir.join(format!("{stem}.path")), &target)?;
    Ok(())
}

/// The slot this (checkpoint, path) owns: `None` when one already exists, which
/// is what makes a pre-image once-per-turn.
fn free_stem(dir: &Path, target: &str) -> Result<Option<String>, RewindError> {
    let hash = path_hash(target);
    for suffix in 0u32.. {
        let stem = if suffix == 0 {
            hash.clone()
        } else {
            format!("{hash}-{suffix}")
        };
        match std::fs::read_to_string(dir.join(format!("{stem}.path"))) {
            // Already recorded this turn: the pre-image on disk is older, and
            // older is the one the checkpoint means.
            Ok(existing) if existing == target => return Ok(None),
            // A different path landed on the same hash; try the next slot.
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Some(stem));
            }
            Err(error) => return Err(RewindError::Io(error)),
        }
    }
    Ok(None)
}

/// What a restore from `from` would do: distinct files it can put back, and
/// distinct files it cannot because their pre-image was never taken.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Coverage {
    pub files: usize,
    pub missed: usize,
}

/// What `restore(dir, from)` would cover. A file that was missed at one
/// checkpoint and snapshotted at another counts as restorable: the older
/// pre-image is the one a restore lands on.
pub fn changed_files(dir: &Path, from: CheckpointId) -> Coverage {
    let mut restorable: Vec<PathBuf> = Vec::new();
    let mut missed: Vec<PathBuf> = Vec::new();
    for (_, checkpoint) in checkpoints(dir).into_iter().filter(|(id, _)| *id >= from) {
        for pre in pre_images(&checkpoint) {
            match pre {
                Pre::Missed(path) => missed.push(path),
                Pre::Bytes(path, _) | Pre::Created(path) => restorable.push(path),
            }
        }
    }
    restorable.sort();
    restorable.dedup();
    missed.sort();
    missed.dedup();
    missed.retain(|path| !restorable.contains(path));
    Coverage {
        files: restorable.len(),
        missed: missed.len(),
    }
}

/// A file put back by a restore.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restored {
    pub path: PathBuf,
    /// The tool had created it, so putting it back means it is gone again.
    pub removed: bool,
}

/// Put every file back to what it was at `from`.
///
/// Checkpoints are replayed newest first, so where two turns edited the same
/// file the oldest pre-image is written last and wins — which is exactly the
/// state the chosen checkpoint began in. Nothing outside the recorded paths is
/// touched, and a created file is removed by name, never by clearing a
/// directory: the store knows which files a tool made, and only those.
pub fn restore(dir: &Path, from: CheckpointId) -> Result<Vec<Restored>, RewindError> {
    let mut restored: Vec<Restored> = Vec::new();
    let mut checkpoints: Vec<(CheckpointId, PathBuf)> = checkpoints(dir)
        .into_iter()
        .filter(|(id, _)| *id >= from)
        .collect();
    checkpoints.reverse();
    for (_, checkpoint) in checkpoints {
        for pre in pre_images(&checkpoint) {
            let (path, removed) = match pre {
                Pre::Bytes(path, bytes) => {
                    if let Some(parent) = path.parent()
                        && !parent.as_os_str().is_empty()
                    {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&path, std::fs::read(bytes)?)?;
                    (path, false)
                }
                Pre::Created(path) => {
                    match std::fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(RewindError::Io(error)),
                    }
                    (path, true)
                }
                // No pre-image was ever taken: there is nothing to put back,
                // and guessing would be worse than leaving it alone.
                Pre::Missed(_) => continue,
            };
            match restored.iter_mut().find(|done| done.path == path) {
                // A later checkpoint already reported it; the older pre-image
                // just overwrote that one, so its verdict is the one to report.
                Some(done) => done.removed = removed,
                None => restored.push(Restored { path, removed }),
            }
        }
    }
    restored.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(restored)
}

/// Forget every checkpoint at or after `from`: the turns they belong to are no
/// longer in the conversation, so their pre-images address nothing.
pub fn drop_from(dir: &Path, from: CheckpointId) {
    for (_, checkpoint) in checkpoints(dir).into_iter().filter(|(id, _)| *id >= from) {
        remove_checkpoint(&checkpoint);
    }
}

/// Remove one checkpoint directory by naming its own files. Never a recursive
/// delete: this code deletes only what it wrote.
fn remove_checkpoint(dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for path in entries.filter_map(|entry| entry.ok()).map(|e| e.path()) {
            if path
                .extension()
                .is_some_and(|ext| ext == "path" || ext == "pre")
            {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    // Fails harmlessly if anything else is in there — better a stray directory
    // than a delete that reaches past what we put in it.
    let _ = std::fs::remove_dir(dir);
}

/// Bring the store back inside [`MAX_CHECKPOINTS`] and [`MAX_STORE_BYTES`],
/// oldest checkpoint first.
fn evict(dir: &Path) {
    let mut found = checkpoints(dir);
    while found.len() > MAX_CHECKPOINTS {
        remove_checkpoint(&found.remove(0).1);
    }
    let mut total: u64 = found.iter().map(|(_, dir)| dir_bytes(dir)).sum();
    while total > MAX_STORE_BYTES && !found.is_empty() {
        let (_, oldest) = found.remove(0);
        total = total.saturating_sub(dir_bytes(&oldest));
        remove_checkpoint(&oldest);
    }
}

fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.metadata().ok())
        .map(|meta| meta.len())
        .sum()
}

/// The message a summarized tail leaves behind.
///
/// Deliberately *not* compaction's wording: that string is a byte contract
/// about the prefix of a request, reproduced identically by the in-memory
/// splice and by every later projection. This message is a tail, written once,
/// at a different time and for a different reason, and borrowing the contract's
/// words for it would make a reload unable to tell the two apart.
pub fn summary_message(summary: &str) -> crate::api::types::Message {
    crate::api::types::Message::user_text(format!(
        "(summary of the turns rewound from here)\n{summary}"
    ))
}

/// Replace the turns from the checkpoint onward with a summary of them: cut the
/// session at `cut` — the message the turn opened after — and append the
/// summary in their place.
pub fn write_summary(
    transcript: &crate::transcript::Transcript,
    cut: usize,
    summary: &str,
) -> Result<(), crate::transcript::TranscriptError> {
    transcript.truncate_at_line(cut)?;
    transcript.append(&summary_message(summary))
}

/// One rewind point as the selector shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    /// Transcript line of the user message — the checkpoint's identity.
    pub line: usize,
    /// Its index in the projected history (where truncation lands).
    pub index: usize,
    /// First line of what the user wrote, for the list.
    pub label: String,
    /// The whole message, for the composer on restore.
    pub text: String,
    /// Wall clock of the turn.
    pub at: u64,
    /// What restoring code from this point would cover.
    pub coverage: Coverage,
}

/// Longest label the selector shows before eliding.
const LABEL_CHARS: usize = 60;

/// How far back a rewind list goes. A cap rather than the whole history: past
/// fifty turns the list is a scroll, not a choice.
pub const REWIND_MAX: usize = 50;

/// The rewind points a projected history offers, newest first.
///
/// Only turn-opening messages qualify, and only ones still present verbatim: a
/// message the last compaction folded into its summary is not in the projection
/// at all, so it cannot be offered — which is the whole guarantee that rewind
/// never cuts across a compact boundary.
pub fn checkpoints_of(
    entries: &[crate::transcript::Entry],
    dir: &Path,
    max: usize,
) -> Vec<Checkpoint> {
    let mut found: Vec<Checkpoint> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let at = entry.opens_turn?;
            let line = entry.line?;
            let text = entry
                .message
                .content
                .iter()
                .filter_map(|block| match block {
                    crate::api::types::ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return None;
            }
            Some(Checkpoint {
                line,
                index,
                label: elide(trimmed.lines().next().unwrap_or_default(), LABEL_CHARS),
                text,
                at,
                coverage: changed_files(dir, line),
            })
        })
        .collect();
    found.reverse();
    found.truncate(max);
    found
}

fn elide(text: &str, limit: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= limit {
        return flat;
    }
    flat.chars().take(limit).chain(['…']).collect()
}

#[cfg(test)]
#[path = "rewind_tests.rs"]
mod tests;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use crate::api::types::Message;
use crate::error::ErrorCode;

#[derive(Debug, Error)]
pub enum TranscriptError {
    #[error("transcript io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse transcript line: {0}")]
    Parse(#[from] serde_json::Error),
}

impl ErrorCode for TranscriptError {
    fn error_code(&self) -> &'static str {
        match self {
            TranscriptError::Io(_) | TranscriptError::Parse(_) => "STORAGE_ERROR",
        }
    }
}

type ActiveFiles = Option<(std::fs::File, std::fs::File)>;
type ActiveLock = Arc<Mutex<ActiveFiles>>;
type ActiveLockMap = Mutex<HashMap<PathBuf, Weak<Mutex<ActiveFiles>>>>;

/// Session transcript: JSONL, one Message per line (D11).
#[derive(Debug, Clone)]
pub struct Transcript {
    path: PathBuf,
    active_lock: ActiveLock,
}

impl Transcript {
    fn at(path: PathBuf) -> Self {
        static ACTIVE_LOCKS: OnceLock<ActiveLockMap> = OnceLock::new();
        let mut active_locks = ACTIVE_LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let active_lock = active_locks
            .get(&path)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let active_lock = Arc::new(Mutex::new(None));
                active_locks.insert(path.clone(), Arc::downgrade(&active_lock));
                active_lock
            });
        Self { path, active_lock }
    }
}

fn slugify(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "root".to_string()
    } else {
        cleaned
    }
}

/// transcripts dir: ~/.local/share/bingo/transcripts.
pub fn transcripts_dir(home: &Path) -> PathBuf {
    crate::storage::transcripts_dir(home)
}

/// New session file: <project-slug>-<unix-ts>.jsonl.
pub fn create(home: &Path, cwd: &Path) -> Result<Transcript, TranscriptError> {
    let dir = transcripts_dir(home);
    std::fs::create_dir_all(&dir)?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = cwd
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let slug = slugify(&name);
    let path = dir.join(format!("{slug}-{ts}.jsonl"));
    Ok(Transcript::at(path))
}

/// All sessions (/resume list), most recently modified first.
pub fn list(home: &Path) -> Result<Vec<Transcript>, TranscriptError> {
    let dir = transcripts_dir(home);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut entries: Vec<(SystemTime, Transcript)> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .map(|p| {
            let modified = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .ok()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (modified, Transcript::at(p))
        })
        .collect();
    entries.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    Ok(entries.into_iter().map(|(_, t)| t).collect())
}

/// Resume the latest session (--continue).
pub fn latest(home: &Path) -> Result<Option<Transcript>, TranscriptError> {
    Ok(list(home)?.into_iter().next())
}

impl Transcript {
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Session display name: file stem (`{slug}-{ts}` / `{slug}-{ts}-{name}`).
    pub fn name(&self) -> String {
        self.path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// Rename session (/rename): `{slug}-{ts}` → `{slug}-{ts}-{name}.jsonl`.
    /// Returns a Transcript pointing at the new path.
    pub fn rename(&self, name: &str) -> Result<Transcript, TranscriptError> {
        let slug = slugify(name);
        if slug.is_empty() || slug == "root" {
            return Err(TranscriptError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "empty session name",
            )));
        }
        let stem = self
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let new_path = self.path.with_file_name(format!("{stem}-{slug}.jsonl"));
        let old_lock_path = self.path.with_extension("jsonl.lock");
        let new_lock_path = new_path.with_extension("jsonl.lock");
        let mut active_lock = self
            .active_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        std::fs::rename(&self.path, &new_path)?;
        if old_lock_path.exists()
            && let Err(error) = std::fs::rename(&old_lock_path, &new_lock_path)
        {
            if let Err(rollback) = std::fs::rename(&new_path, &self.path) {
                return Err(TranscriptError::Io(std::io::Error::other(format!(
                    "failed to rename transcript lock ({error}); data-file rollback failed: {rollback}"
                ))));
            }
            return Err(TranscriptError::Io(error));
        }
        let old_lock = active_lock.take();
        drop(active_lock);
        let renamed = Transcript::at(new_path);
        if let Some((lock_file, file)) = old_lock {
            *renamed
                .active_lock
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some((lock_file, file));
        }
        Ok(renamed)
    }

    fn ensure_active_lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, ActiveFiles>, TranscriptError> {
        let mut active_lock = self
            .active_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if active_lock.is_none() {
            let lock_path = self.path.with_extension("jsonl.lock");
            let lock_file = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(&lock_path)?;
            lock_file.try_lock().map_err(|error| match error {
                std::fs::TryLockError::Error(error) => TranscriptError::Io(error),
                std::fs::TryLockError::WouldBlock => TranscriptError::Io(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "transcript is active in another process: {}",
                        self.path.display()
                    ),
                )),
            })?;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(&self.path)?;
            file.try_lock().map_err(|error| match error {
                std::fs::TryLockError::Error(error) => TranscriptError::Io(error),
                std::fs::TryLockError::WouldBlock => TranscriptError::Io(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    format!(
                        "transcript is active in another process: {}",
                        self.path.display()
                    ),
                )),
            })?;
            *active_lock = Some((lock_file, file));
        }
        Ok(active_lock)
    }

    pub fn activate(&self) -> Result<(), TranscriptError> {
        drop(self.ensure_active_lock()?);
        Ok(())
    }

    /// Full-file rewrite (persisted after a manual /compact).
    pub fn replace_messages(&self, messages: &[Message]) -> Result<(), TranscriptError> {
        use std::io::{Seek, Write};
        let mut active_lock = self.ensure_active_lock()?;
        let file = active_lock.as_mut().map(|(_, file)| file).ok_or_else(|| {
            TranscriptError::Io(std::io::Error::other("transcript active lock missing"))
        })?;
        file.set_len(0)?;
        file.seek(std::io::SeekFrom::Start(0))?;
        for message in messages {
            let line = serde_json::to_string(message)?;
            writeln!(file, "{line}")?;
        }
        Ok(())
    }

    /// Append one message.
    pub fn append(&self, message: &Message) -> Result<(), TranscriptError> {
        use std::io::{Seek, Write};
        let mut active_lock = self.ensure_active_lock()?;
        let file = active_lock.as_mut().map(|(_, file)| file).ok_or_else(|| {
            TranscriptError::Io(std::io::Error::other("transcript active lock missing"))
        })?;
        file.seek(std::io::SeekFrom::End(0))?;
        let line = serde_json::to_string(message)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Load all history messages (for --continue resume).
    /// Bad lines are skipped and counted with a warning: one truncated JSONL line must
    /// not make the whole session unrecoverable.
    pub fn load_messages(&self) -> Result<Vec<Message>, TranscriptError> {
        let content = std::fs::read_to_string(&self.path)?;
        let mut messages = Vec::new();
        let mut skipped = 0usize;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Message>(line) {
                Ok(message) => messages.push(message),
                Err(_) => skipped += 1,
            }
        }
        if skipped > 0 {
            eprintln!(
                "[bingo] warning: skipped {skipped} unreadable line(s) in {}",
                self.path.display()
            );
        }
        drop_contentless(&mut messages);
        Ok(messages)
    }
}

/// A message carrying nothing is not history. A model turn that streamed no block lands
/// here as `content: []`, and a resumed turn built from it is rejected by the endpoints
/// ("content: at least one item required") — so the session it poisons can never be
/// resumed. Blank text blocks go the same way, then messages left with no block at all.
/// Only blocks that carry nothing are removed, so no tool_use ever loses its tool_result.
fn drop_contentless(messages: &mut Vec<Message>) {
    use crate::api::types::ContentBlock;
    for message in messages.iter_mut() {
        message.content.retain(
            |block| !matches!(block, ContentBlock::Text { text } if text.trim().is_empty()),
        );
    }
    messages.retain(|message| !message.content.is_empty());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::Role;

    #[test]
    fn roundtrip_append_and_load() {
        let tmp = std::env::temp_dir().join("bingo-transcript-test");
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let transcript = create(&home, &tmp).unwrap();
        let msg = Message {
            role: Role::User,
            content: vec![crate::api::types::ContentBlock::Text { text: "hi".into() }],
        };
        transcript.append(&msg).unwrap();
        let messages = transcript.load_messages().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0], msg);

        let latest = latest(&home).unwrap().unwrap();
        assert_eq!(latest.load_messages().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// One bad JSONL line must not make the whole session unrecoverable: skip the bad
    /// line, load the rest as usual.
    #[test]
    fn load_skips_corrupt_lines() {
        let tmp = std::env::temp_dir().join(format!("bingo-transcript-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let transcript = create(&home, &tmp).unwrap();
        let good = Message {
            role: Role::User,
            content: vec![crate::api::types::ContentBlock::Text { text: "hi".into() }],
        };
        transcript.append(&good).unwrap();
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(transcript.path())
                .unwrap();
            writeln!(file, "{{\"role\":\"user\",\"content\":[{{\"type\":").unwrap();
            writeln!(file).unwrap();
        }
        transcript.append(&good).unwrap();

        let messages = transcript.load_messages().unwrap();
        assert_eq!(messages.len(), 2, "bad lines skipped, good lines kept");
        assert_eq!(messages[0], good);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A turn that streamed nothing is persisted as `content: []`, and the endpoints reject
    /// a content-free message on the next request — leaving it in history means the session
    /// can never be resumed. Blocks carrying nothing go the same way; a tool_result never
    /// does, so no tool_use is orphaned.
    #[test]
    fn load_drops_messages_that_carry_nothing() {
        use crate::api::types::ContentBlock;
        let tmp =
            std::env::temp_dir().join(format!("bingo-transcript-void-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();

        let transcript = create(&home, &tmp).unwrap();
        let good = Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: "hi".into() }],
        };
        let call = Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::Text { text: "  ".into() },
                ContentBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "Bash".into(),
                    input: serde_json::json!({ "command": "ls" }),
                },
            ],
        };
        transcript.append(&good).unwrap();
        transcript
            .append(&Message {
                role: Role::Assistant,
                content: Vec::new(),
            })
            .unwrap();
        transcript
            .append(&Message {
                role: Role::User,
                content: vec![ContentBlock::Text { text: "".into() }],
            })
            .unwrap();
        transcript.append(&call).unwrap();

        let messages = transcript.load_messages().unwrap();

        assert_eq!(messages.len(), 2, "both content-free messages are dropped");
        assert_eq!(messages[0], good);
        assert_eq!(
            messages[1].content.len(),
            1,
            "the blank text block goes, the tool_use stays"
        );
        assert!(matches!(
            &messages[1].content[0],
            ContentBlock::ToolUse { id, .. } if id == "toolu_1"
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn rename_moves_the_active_lock_sidecar() {
        let tmp =
            std::env::temp_dir().join(format!("bingo-transcript-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let transcript = create(&home, &tmp).unwrap();
        transcript.append(&Message::user_text("active")).unwrap();
        let old_lock_path = transcript.path().with_extension("jsonl.lock");

        let renamed = transcript.rename("named").unwrap();
        let new_lock_path = renamed.path().with_extension("jsonl.lock");
        let competing_lock = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&new_lock_path)
            .unwrap();

        assert!(!old_lock_path.exists());
        assert!(new_lock_path.exists());
        assert!(matches!(
            competing_lock.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn slugifies_odd_names() {
        assert_eq!(slugify("bingo"), "bingo");
        assert_eq!(slugify("a b/c"), "a_b_c");
        assert_eq!(slugify("café"), "caf_");
    }
}

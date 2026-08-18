use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
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

/// `(sidecar lock, transcript data file)`. Only the sidecar is ever locked (D72);
/// the data file handle is held open for appends.
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

    /// Claim the session for this process. The sidecar `.jsonl.lock` is the whole mutex:
    /// the transcript itself is never locked, because Windows file locks are mandatory —
    /// a lock held for the session's lifetime would fail every other handle opened on the
    /// same file (`load_messages`, /resume, /share) with ERROR_LOCK_VIOLATION, where the
    /// advisory Unix locks let those reads through unnoticed (D72).
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
            *active_lock = Some((lock_file, file));
        }
        Ok(active_lock)
    }

    pub fn activate(&self) -> Result<(), TranscriptError> {
        drop(self.ensure_active_lock()?);
        Ok(())
    }

    /// Append one message.
    pub fn append(&self, message: &Message) -> Result<(), TranscriptError> {
        self.append_line(&serde_json::to_string(message)?)
    }

    /// Append a turn marker: the next message line opens a user turn, and is
    /// therefore a rewind checkpoint (D91). `at` is wall-clock unix seconds, so
    /// the rewind list can stamp a turn the messages themselves never dated.
    pub fn append_turn(&self, at: u64) -> Result<(), TranscriptError> {
        self.append_line(&serde_json::to_string(&TurnLine {
            tag: TurnTag::Turn,
            at,
        })?)
    }

    /// Raw line count — the index the next appended line lands on. A transcript
    /// that has never been written is at zero.
    pub fn line_count(&self) -> Result<usize, TranscriptError> {
        match std::fs::read_to_string(&self.path) {
            Ok(content) => Ok(content.lines().count()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(error) => Err(TranscriptError::Io(error)),
        }
    }

    /// Append a compaction marker: the lines above it stay canonical, loads
    /// project through it (D74). The summary is written once and reused every
    /// load until the next threshold crossing, so compaction is the only point
    /// where the request prefix changes bytes.
    pub fn append_compact(&self, summary: &str, kept: usize) -> Result<(), TranscriptError> {
        self.append_line(&serde_json::to_string(&CompactLine {
            tag: CompactTag::Compact,
            summary: summary.to_string(),
            kept,
        })?)
    }

    fn append_line(&self, line: &str) -> Result<(), TranscriptError> {
        use std::io::{Seek, Write};
        let mut active_lock = self.ensure_active_lock()?;
        let file = active_lock.as_mut().map(|(_, file)| file).ok_or_else(|| {
            TranscriptError::Io(std::io::Error::other("transcript active lock missing"))
        })?;
        file.seek(std::io::SeekFrom::End(0))?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// The model-facing history (for --continue resume and every turn's
    /// context): canonical lines projected through the latest compact marker.
    /// A session without markers loads exactly as written.
    pub fn load_messages(&self) -> Result<Vec<Message>, TranscriptError> {
        Ok(project(self.load_lines()?)
            .into_iter()
            .map(|entry| entry.message)
            .collect())
    }

    /// The same history, with the transcript line each message came from and
    /// the turn markers that make some of them rewind checkpoints (D91).
    pub fn load_projection(&self) -> Result<Vec<Entry>, TranscriptError> {
        Ok(project(self.load_lines()?))
    }

    /// Every message ever written, ignoring compact markers — the full
    /// conversation for human-facing export (/share).
    pub fn load_canonical(&self) -> Result<Vec<Message>, TranscriptError> {
        let mut entries = message_entries(self.load_lines()?);
        drop_contentless(&mut entries);
        Ok(entries.into_iter().map(|entry| entry.message).collect())
    }

    /// Bad lines are skipped and counted with a warning: one truncated JSONL line must
    /// not make the whole session unrecoverable.
    fn load_lines(&self) -> Result<Vec<(usize, Line)>, TranscriptError> {
        let content = std::fs::read_to_string(&self.path)?;
        let mut lines = Vec::new();
        let mut skipped = 0usize;
        for (raw, line) in content.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match parse_line(line) {
                Some(parsed) => lines.push((raw, parsed)),
                None => skipped += 1,
            }
        }
        if skipped > 0 {
            eprintln!(
                "[bingo] warning: skipped {skipped} unreadable line(s) in {}",
                self.path.display()
            );
        }
        Ok(lines)
    }
}

impl Transcript {
    /// Cut the session so its projected history ends at (and includes) the
    /// message written on raw line `line` (D91 rewind).
    ///
    /// The surviving prefix is copied byte for byte — never re-serialized — so
    /// the request prefix the provider has cached is the one it gets back. Only
    /// one line can ever be new: when the cut drops the last compaction marker,
    /// the same summary is re-emitted with a `kept` count narrowed to the part
    /// of its window that survived, because the marker's whole meaning is
    /// positional. Without that, a cut into the kept tail would resurrect the
    /// messages the summary already stands for.
    ///
    /// The replacement is atomic (temp + rename), and the append handle is
    /// closed across the rename and reopened after it: on Unix it would
    /// otherwise keep writing into the unlinked inode, and on Windows the
    /// rename would fail outright.
    pub fn truncate_at_line(&self, line: usize) -> Result<(), TranscriptError> {
        let content = std::fs::read_to_string(&self.path)?;
        let raw: Vec<&str> = content.lines().collect();
        if line >= raw.len() {
            return Err(TranscriptError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("rewind line {line} is past the end of the transcript"),
            )));
        }
        let parsed: Vec<(usize, Line)> = raw
            .iter()
            .enumerate()
            .filter(|(_, text)| !text.trim().is_empty())
            .filter_map(|(index, text)| parse_line(text).map(|parsed| (index, parsed)))
            .collect();
        if !matches!(
            parsed.iter().find(|(index, _)| *index == line),
            Some((_, Line::Message(_)))
        ) {
            return Err(TranscriptError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("rewind line {line} is not a message"),
            )));
        }

        let mut body: String = raw[..=line].join("\n");
        body.push('\n');

        // The last marker, and whether the cut takes it with it.
        if let Some((marker, compact)) =
            parsed
                .iter()
                .rev()
                .find_map(|(index, parsed)| match parsed {
                    Line::Compact(compact) => Some((*index, compact)),
                    _ => None,
                })
            && marker > line
        {
            let window: Vec<usize> = parsed
                .iter()
                .filter(|(index, parsed)| *index < marker && matches!(parsed, Line::Message(_)))
                .map(|(index, _)| *index)
                .collect();
            let tail = window.len().saturating_sub(compact.kept);
            let kept = window[tail..].iter().filter(|kept| **kept <= line).count();
            if kept == 0 {
                // The cut lands in the span the summary already covers: honouring
                // it would mean re-summarizing, which is not this operation.
                return Err(TranscriptError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("rewind line {line} is inside a compacted span"),
                )));
            }
            body.push_str(&serde_json::to_string(&CompactLine {
                tag: CompactTag::Compact,
                summary: compact.summary.clone(),
                kept,
            })?);
            body.push('\n');
        }

        self.replace_contents(&body)
    }

    /// Swap the transcript's bytes under the active lock. The sidecar lock is
    /// held throughout; only the data handle is closed and reopened.
    fn replace_contents(&self, body: &str) -> Result<(), TranscriptError> {
        let mut active = self.ensure_active_lock()?;
        let Some((lock_file, data)) = active.take() else {
            return Err(TranscriptError::Io(std::io::Error::other(
                "transcript active lock missing",
            )));
        };
        drop(data);
        let tmp = self.path.with_extension("jsonl.rewind");
        let swap = std::fs::write(&tmp, body).and_then(|()| std::fs::rename(&tmp, &self.path));
        if swap.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        let reopened = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&self.path);
        match reopened {
            // The lock file goes back with the new handle, so the claim on this
            // session never lapses.
            Ok(file) => *active = Some((lock_file, file)),
            Err(error) => return Err(TranscriptError::Io(error)),
        }
        swap.map_err(TranscriptError::Io)
    }
}

/// Marker value distinguishing a compact line from a bare `Message` line.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum CompactTag {
    #[serde(rename = "compact")]
    Compact,
}

/// A compaction event (D74): every message line above is covered by `summary`,
/// except the last `kept`, which stay verbatim. A later marker supersedes an
/// earlier one — markers are appended, canonical lines are never rewritten.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompactLine {
    #[serde(rename = "type")]
    tag: CompactTag,
    summary: String,
    kept: usize,
}

/// Marker value distinguishing a turn line from the others.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
enum TurnTag {
    #[serde(rename = "turn")]
    Turn,
}

/// A turn boundary (D91): the next message line opens a user turn, which makes
/// it a rewind checkpoint. Every projection skips it, so it changes no request
/// bytes and no compact `kept` accounting — and a session recorded before D91
/// simply offers no checkpoints rather than guessing at them from message text,
/// which the harness's own injections (reminders, notifications, resume
/// prompts) are indistinguishable from.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TurnLine {
    #[serde(rename = "type")]
    tag: TurnTag,
    /// Wall-clock unix seconds. Absent in nothing yet, but defaulted so a
    /// hand-written or future marker never costs the whole line.
    #[serde(default)]
    at: u64,
}

#[derive(Debug)]
enum Line {
    Message(Message),
    Compact(CompactLine),
    Turn(u64),
}

/// One message of the projected history, with what rewind needs to address it.
#[derive(Debug, Clone)]
pub struct Entry {
    pub message: Message,
    /// The transcript line it was written on. `None` for the compaction
    /// summary, which is synthesized by the projection and was never a line.
    pub line: Option<usize>,
    /// A turn marker preceded it: this message opens a turn (D91), stamped
    /// with the marker's wall clock. `None` means it does not open a turn.
    pub opens_turn: Option<u64>,
}

fn parse_line(line: &str) -> Option<Line> {
    if let Ok(message) = serde_json::from_str::<Message>(line) {
        return Some(Line::Message(message));
    }
    if let Ok(compact) = serde_json::from_str::<CompactLine>(line) {
        return Some(Line::Compact(compact));
    }
    serde_json::from_str::<TurnLine>(line)
        .ok()
        .map(|turn| Line::Turn(turn.at))
}

/// Every message line as an entry, markers dropped — the shape `load_canonical`
/// wants and the no-marker case of `project`.
fn message_entries(lines: Vec<(usize, Line)>) -> Vec<Entry> {
    let mut opens = None;
    let mut entries = Vec::new();
    for (raw, line) in lines {
        match line {
            Line::Turn(at) => opens = Some(at),
            Line::Message(message) => entries.push(Entry {
                message,
                line: Some(raw),
                opens_turn: std::mem::take(&mut opens),
            }),
            Line::Compact(_) => {}
        }
    }
    entries
}

/// The summary's message form — shared by the in-memory splice
/// (`compact::compact`) and this projection so both produce the same bytes:
/// a reloaded session must hand the provider the prefix it already cached.
pub(crate) const COMPACT_SUMMARY_PREFIX: &str =
    "(summary of the earlier conversation, from automatic compaction)";

pub(crate) fn summary_message(summary: &str) -> Message {
    Message::user_text(format!("{COMPACT_SUMMARY_PREFIX}\n{summary}"))
}

/// Apply the last compact marker: [summary] + the kept tail before it + every
/// message after it. `kept` counts physical message lines, so it is applied
/// before `drop_contentless` (the splice at compaction time counted the same
/// in-memory list that `record` had persisted line by line).
fn project(lines: Vec<(usize, Line)>) -> Vec<Entry> {
    use crate::api::types::ContentBlock;
    let marker = lines
        .iter()
        .rposition(|(_, line)| matches!(line, Line::Compact(_)));
    let Some(marker) = marker else {
        let mut entries = message_entries(lines);
        drop_contentless(&mut entries);
        return entries;
    };
    let mut summary = String::new();
    let mut kept = 0usize;
    let mut opens = None;
    let mut before: Vec<Entry> = Vec::new();
    let mut after: Vec<Entry> = Vec::new();
    for (index, (raw, line)) in lines.into_iter().enumerate() {
        match line {
            Line::Turn(at) => opens = Some(at),
            Line::Message(message) => {
                let entry = Entry {
                    message,
                    line: Some(raw),
                    opens_turn: std::mem::take(&mut opens),
                };
                if index < marker {
                    before.push(entry);
                } else {
                    after.push(entry);
                }
            }
            Line::Compact(compact) if index == marker => {
                summary = compact.summary;
                kept = compact.kept;
            }
            // Superseded by the later marker: its span is inside this one's.
            Line::Compact(_) => {}
        }
    }
    let tail = before.len().saturating_sub(kept);
    let mut messages = Vec::with_capacity(1 + before.len() - tail + after.len());
    messages.push(Entry {
        message: summary_message(&summary),
        line: None,
        opens_turn: None,
    });
    messages.extend(before.into_iter().skip(tail));
    messages.extend(after);
    drop_contentless(&mut messages);
    // The splice cut at a safe boundary, but a crash-truncated line inside the
    // tail window shifts the count — an orphan tool_result surfacing as the
    // first kept message would 400 every later request, so advance past it
    // (the same invariant compact::safe_split maintains).
    while messages.len() > 1
        && messages[1]
            .message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
    {
        messages.remove(1);
    }
    messages
}

/// A message carrying nothing is not history. A model turn that streamed no block lands
/// here as `content: []`, and a resumed turn built from it is rejected by the endpoints
/// ("content: at least one item required") — so the session it poisons can never be
/// resumed. Blank text blocks go the same way, then messages left with no block at all.
/// Only blocks that carry nothing are removed, so no tool_use ever loses its tool_result.
fn drop_contentless(entries: &mut Vec<Entry>) {
    use crate::api::types::ContentBlock;
    for entry in entries.iter_mut() {
        entry.message.content.retain(
            |block| !matches!(block, ContentBlock::Text { text } if text.trim().is_empty()),
        );
    }
    entries.retain(|entry| !entry.message.content.is_empty());
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

    /// D74: a compact marker projects — the canonical lines stay on disk, the
    /// load shows [summary, kept tail, everything appended after].
    #[test]
    fn compact_marker_projects_summary_plus_kept_tail() {
        let tmp = std::env::temp_dir().join(format!("bingo-transcript-cpj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let transcript = create(&home, &tmp).unwrap();
        for i in 0..5 {
            transcript
                .append(&Message::user_text(format!("m{i}")))
                .unwrap();
        }
        transcript.append_compact("the gist", 2).unwrap();
        transcript.append(&Message::user_text("m5")).unwrap();

        let projected = transcript.load_messages().unwrap();
        let texts: Vec<String> = projected
            .iter()
            .map(|m| match &m.content[0] {
                crate::api::types::ContentBlock::Text { text } => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(projected.len(), 4, "summary + 2 kept + 1 appended");
        assert!(texts[0].contains("the gist"));
        assert!(
            texts[0].starts_with("(summary of the earlier conversation"),
            "projection and in-memory splice must share the same bytes"
        );
        assert_eq!(texts[1..], ["m3", "m4", "m5"]);

        let canonical = transcript.load_canonical().unwrap();
        assert_eq!(canonical.len(), 6, "canonical keeps every message line");

        // A later marker supersedes: its span covers the earlier marker's.
        transcript.append_compact("newer gist", 1).unwrap();
        let projected = transcript.load_messages().unwrap();
        assert_eq!(projected.len(), 2, "summary + 1 kept");
        assert!(matches!(
            &projected[0].content[0],
            crate::api::types::ContentBlock::Text { text } if text.contains("newer gist")
        ));
        assert!(matches!(
            &projected[1].content[0],
            crate::api::types::ContentBlock::Text { text } if text == "m5"
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// A kept count larger than what exists floors at the full history, and a
    /// kept tail that would begin with an orphan tool_result advances past it
    /// (otherwise every later request 400s).
    #[test]
    fn compact_projection_is_safe_at_the_edges() {
        use crate::api::types::ContentBlock;
        let tmp = std::env::temp_dir().join(format!("bingo-transcript-cpe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let transcript = create(&home, &tmp).unwrap();
        transcript.append(&Message::user_text("only")).unwrap();
        transcript.append_compact("gist", 99).unwrap();
        assert_eq!(
            transcript.load_messages().unwrap().len(),
            2,
            "oversized kept keeps everything"
        );

        let orphan = create(&home, &tmp).unwrap();
        orphan.append(&Message::user_text("early")).unwrap();
        orphan
            .append(&Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tu_lost".into(),
                    content: serde_json::Value::String("ok".into()),
                    is_error: false,
                }],
            })
            .unwrap();
        orphan.append(&Message::user_text("late")).unwrap();
        orphan.append_compact("gist", 2).unwrap();
        let projected = orphan.load_messages().unwrap();
        assert_eq!(projected.len(), 2, "the orphan tool_result is dropped");
        assert!(matches!(
            &projected[1].content[0],
            ContentBlock::Text { text } if text == "late"
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

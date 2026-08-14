use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::api::types::{ContentBlock, Message, Role};
use crate::error::ErrorCode;

pub const TURN_INDEX_SCHEMA_VERSION: u8 = 1;
pub const SESSION_SCHEMA_VERSION: u8 = 1;
pub const INTERRUPTED_TOOL_RESULT: &str =
    "Interrupted by a runtime failure before this tool produced a result.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMetadata {
    pub schema_version: u8,
    pub cwd: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionRecord {
    #[serde(rename = "type")]
    record_type: String,
    schema_version: u8,
    cwd: String,
}

enum TranscriptLine {
    Message(Message),
    Session(Option<SessionMetadata>),
}

#[derive(Debug, Error)]
pub enum TranscriptError {
    #[error("transcript io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse transcript line: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("fork point unavailable: {0}")]
    ForkPointUnavailable(String),
    #[error("session changed since it was loaded: {0}")]
    SessionStale(String),
}

impl ErrorCode for TranscriptError {
    fn error_code(&self) -> &'static str {
        match self {
            TranscriptError::Io(_) | TranscriptError::Parse(_) => "STORAGE_ERROR",
            TranscriptError::ForkPointUnavailable(_) => "FORK_POINT_UNAVAILABLE",
            TranscriptError::SessionStale(_) => "SESSION_STALE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForkReason {
    EditLastPrompt,
    RecoverInterrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TurnStatus {
    Started,
    Completed,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRecord {
    pub turn_id: String,
    /// One-based physical JSONL line occupied by the top-level user prompt.
    pub prompt_line: u64,
    pub status: TurnStatus,
    pub content_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnIndex {
    pub schema_version: u8,
    pub transcript_revision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_reason: Option<ForkReason>,
    #[serde(default)]
    pub turns: Vec<TurnRecord>,
}

#[derive(Debug, Clone)]
pub struct ForkResult {
    pub transcript: Transcript,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditForkPoint<'a> {
    pub turn_id: Option<&'a str>,
    pub content_revision: Option<&'a str>,
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
    initial_session: Option<SessionMetadata>,
}

impl Transcript {
    fn at(path: PathBuf) -> Self {
        Self::with_initial_session(path, None)
    }

    fn with_initial_session(path: PathBuf, initial_session: Option<SessionMetadata>) -> Self {
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
        Self {
            path,
            active_lock,
            initial_session,
        }
    }
}

fn canonical_session_metadata(cwd: &Path) -> Result<SessionMetadata, TranscriptError> {
    let absolute = if cwd.is_absolute() {
        cwd.to_path_buf()
    } else {
        std::env::current_dir()?.join(cwd)
    };
    let canonical = std::fs::canonicalize(&absolute).unwrap_or_else(|_| {
        let mut normalized = PathBuf::new();
        for component in absolute.components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    normalized.pop();
                }
                _ => normalized.push(component.as_os_str()),
            }
        }
        normalized
    });
    let cwd = canonical.to_str().ok_or_else(|| {
        TranscriptError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "session workspace path is not valid UTF-8",
        ))
    })?;
    #[cfg(windows)]
    let cwd = cwd
        .strip_prefix(r"\\?\UNC\")
        .map(|path| format!(r"\\{path}"))
        .or_else(|| cwd.strip_prefix(r"\\?\").map(str::to_string))
        .unwrap_or_else(|| cwd.to_string());
    #[cfg(not(windows))]
    let cwd = cwd.to_string();
    Ok(SessionMetadata {
        schema_version: SESSION_SCHEMA_VERSION,
        cwd,
    })
}

fn session_record(metadata: &SessionMetadata) -> SessionRecord {
    SessionRecord {
        record_type: "session".to_string(),
        schema_version: metadata.schema_version,
        cwd: metadata.cwd.clone(),
    }
}

fn parse_transcript_line(line: &str) -> Result<TranscriptLine, serde_json::Error> {
    let value = serde_json::from_str::<serde_json::Value>(line)?;
    if value.get("type").and_then(serde_json::Value::as_str) == Some("session") {
        let metadata = serde_json::from_value::<SessionRecord>(value)
            .ok()
            .filter(|record| {
                record.record_type == "session"
                    && record.schema_version == SESSION_SCHEMA_VERSION
                    && Path::new(&record.cwd).is_absolute()
            })
            .map(|record| SessionMetadata {
                schema_version: record.schema_version,
                cwd: record.cwd,
            });
        return Ok(TranscriptLine::Session(metadata));
    }
    serde_json::from_value(value).map(TranscriptLine::Message)
}

fn latest_session_metadata(source: &[u8]) -> Option<SessionMetadata> {
    let text = std::str::from_utf8(source).ok()?;
    text.lines()
        .filter_map(|line| match parse_transcript_line(line) {
            Ok(TranscriptLine::Session(Some(metadata))) => Some(metadata),
            _ => None,
        })
        .next_back()
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
    let session = canonical_session_metadata(cwd)?;
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
    Ok(Transcript::with_initial_session(path, Some(session)))
}

pub fn create_reserved(home: &Path, cwd: &Path) -> Result<Transcript, TranscriptError> {
    use std::io::Write;

    let session = canonical_session_metadata(cwd)?;
    let line = serde_json::to_string(&session_record(&session))?;
    let dir = transcripts_dir(home);
    std::fs::create_dir_all(&dir)?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let name = cwd
        .file_name()
        .map(|value| value.to_string_lossy().to_string())
        .unwrap_or_default();
    let slug = slugify(&name);
    for suffix in 0u64.. {
        let stem = if suffix == 0 {
            format!("{slug}-{ts}")
        } else {
            format!("{slug}-{ts}-{suffix}")
        };
        let path = dir.join(format!("{stem}.jsonl"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                writeln!(file, "{line}")?;
                drop(file);
                let transcript = Transcript::at(path);
                transcript.activate()?;
                return Ok(transcript);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(TranscriptError::Io(error)),
        }
    }
    unreachable!()
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

    pub fn turn_index_path(&self) -> PathBuf {
        self.path.with_extension("turns.json")
    }

    pub fn transcript_revision(&self) -> Result<String, TranscriptError> {
        Ok(revision(&self.read_source()?))
    }

    pub fn session_metadata(&self) -> Result<Option<SessionMetadata>, TranscriptError> {
        let source = self.read_source()?;
        Ok(latest_session_metadata(&source))
    }

    pub fn bind_workspace(&self, cwd: &Path) -> Result<SessionMetadata, TranscriptError> {
        use std::io::{Read, Seek, Write};

        let metadata = canonical_session_metadata(cwd)?;
        let mut active_lock = self.ensure_active_lock()?;
        let file = active_lock.as_mut().map(|(_, file)| file).ok_or_else(|| {
            TranscriptError::Io(std::io::Error::other("transcript active lock missing"))
        })?;
        let length = file.metadata()?.len();
        if length > 0 {
            file.seek(std::io::SeekFrom::End(-1))?;
            let mut tail = [0u8; 1];
            file.read_exact(&mut tail)?;
            file.seek(std::io::SeekFrom::End(0))?;
            if tail[0] != b'\n' {
                writeln!(file)?;
            }
        } else {
            file.seek(std::io::SeekFrom::Start(0))?;
        }
        writeln!(
            file,
            "{}",
            serde_json::to_string(&session_record(&metadata))?
        )?;
        file.flush()?;
        file.seek(std::io::SeekFrom::Start(0))?;
        let mut source = Vec::new();
        file.read_to_end(&mut source)?;
        if let Some(mut index) = self.turn_index()? {
            index.transcript_revision = revision(&source);
            self.write_turn_index(&index)?;
        }
        Ok(metadata)
    }

    pub fn turn_index(&self) -> Result<Option<TurnIndex>, TranscriptError> {
        let path = self.turn_index_path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let index: TurnIndex = serde_json::from_str(&raw)?;
        if index.schema_version != TURN_INDEX_SCHEMA_VERSION {
            return Err(TranscriptError::ForkPointUnavailable(format!(
                "unsupported turn index schemaVersion {}",
                index.schema_version
            )));
        }
        Ok(Some(index))
    }

    pub fn begin_turn(&self, turn_id: &str, prompt: &str) -> Result<String, TranscriptError> {
        self.ensure_initial_session_record()?;
        let source = self.read_source()?;
        let content_revision = revision(prompt.as_bytes());
        let mut index = self.turn_index()?.unwrap_or_else(|| TurnIndex {
            schema_version: TURN_INDEX_SCHEMA_VERSION,
            transcript_revision: revision(&source),
            parent_session_id: None,
            fork_reason: None,
            turns: Vec::new(),
        });
        if index.turns.iter().any(|turn| turn.turn_id == turn_id) {
            return Err(TranscriptError::SessionStale(format!(
                "turnId {turn_id:?} was already recorded"
            )));
        }
        index.turns.push(TurnRecord {
            turn_id: turn_id.to_string(),
            prompt_line: physical_line_count(&source).saturating_add(1) as u64,
            status: TurnStatus::Started,
            content_revision: content_revision.clone(),
        });
        index.transcript_revision = revision(&source);
        self.write_turn_index(&index)?;
        Ok(content_revision)
    }

    pub fn finish_turn(&self, turn_id: &str, status: TurnStatus) -> Result<(), TranscriptError> {
        let Some(mut index) = self.turn_index()? else {
            return Ok(());
        };
        let source = self.read_source()?;
        let lines = parsed_lines(&source, false)?.0;
        if let Some(position) = index.turns.iter().position(|turn| turn.turn_id == turn_id) {
            let prompt_line = index.turns[position].prompt_line;
            if lines
                .iter()
                .any(|(line, message)| *line == prompt_line && is_prompt_message(message))
            {
                index.turns[position].status = status;
            } else {
                index.turns.remove(position);
            }
            index.transcript_revision = revision(&source);
            self.write_turn_index(&index)?;
        }
        Ok(())
    }

    pub fn fork_edit_last_prompt(
        &self,
        home: &Path,
        cwd: &Path,
        point: EditForkPoint<'_>,
    ) -> Result<ForkResult, TranscriptError> {
        let source = self.read_source()?;
        let (parsed, _) = parsed_lines(&source, false)?;
        let source_index = self.turn_index()?;
        let (prompt_line, inherited_turns) = match source_index.as_ref() {
            Some(index) => {
                if index.transcript_revision != revision(&source) {
                    return Err(TranscriptError::SessionStale(
                        "the transcript no longer matches its turn index".to_string(),
                    ));
                }
                let Some(last) = index.turns.last() else {
                    return Err(TranscriptError::ForkPointUnavailable(
                        "turn index does not contain a top-level prompt".to_string(),
                    ));
                };
                if last.status == TurnStatus::Started {
                    return Err(TranscriptError::ForkPointUnavailable(
                        "the last prompt belongs to an interrupted turn; recover it before editing"
                            .to_string(),
                    ));
                }
                if Some(last.turn_id.as_str()) != point.turn_id
                    || Some(last.content_revision.as_str()) != point.content_revision
                {
                    return Err(TranscriptError::SessionStale(
                        "the editable prompt revision no longer matches".to_string(),
                    ));
                }
                if !parsed
                    .iter()
                    .any(|(line, message)| *line == last.prompt_line && is_prompt_message(message))
                {
                    return Err(TranscriptError::ForkPointUnavailable(
                        "indexed prompt line is missing or no longer a top-level prompt"
                            .to_string(),
                    ));
                }
                (
                    last.prompt_line,
                    index
                        .turns
                        .iter()
                        .filter(|turn| turn.prompt_line < last.prompt_line)
                        .cloned()
                        .collect(),
                )
            }
            None => {
                let prompts = parsed
                    .iter()
                    .filter(|(_, message)| is_prompt_message(message))
                    .collect::<Vec<_>>();
                if prompts.len() != 1 {
                    return Err(TranscriptError::ForkPointUnavailable(
                        "legacy transcript has no unambiguous final prompt boundary".to_string(),
                    ));
                }
                if point.turn_id.is_some() || point.content_revision.is_some() {
                    return Err(TranscriptError::SessionStale(
                        "legacy transcript does not have indexed prompt metadata".to_string(),
                    ));
                }
                (prompts[0].0, Vec::new())
            }
        };
        let copied = parsed
            .into_iter()
            .filter(|(line, _)| *line < prompt_line)
            .collect::<Vec<_>>();
        let inherited_turns = remap_turn_lines(inherited_turns, &copied);
        let messages = copied
            .into_iter()
            .map(|(_, message)| message)
            .collect::<Vec<_>>();
        self.create_fork(
            home,
            cwd,
            ForkReason::EditLastPrompt,
            messages,
            inherited_turns,
            Vec::new(),
        )
    }

    pub fn fork_recover_interrupted(
        &self,
        home: &Path,
        cwd: &Path,
    ) -> Result<ForkResult, TranscriptError> {
        let source = self.read_source()?;
        let (parsed, mut warnings) = parsed_lines(&source, true)?;
        let mut tool_results = std::collections::HashSet::new();
        for (_, message) in &parsed {
            for block in &message.content {
                if let ContentBlock::ToolResult { tool_use_id, .. } = block {
                    tool_results.insert(tool_use_id.clone());
                }
            }
        }
        let mut parsed = parsed;
        let mut repaired = Vec::new();
        let mut repaired_count = 0usize;
        for index in 0..parsed.len() {
            let (line, message) = &parsed[index];
            repaired.push((Some(*line), message.clone()));
            let missing = message
                .content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::ToolUse { id, .. } if !tool_results.contains(id) => {
                        Some(id.clone())
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            if missing.is_empty() {
                continue;
            }
            repaired_count += missing.len();
            let replacements = missing
                .into_iter()
                .map(|tool_use_id| ContentBlock::ToolResult {
                    tool_use_id,
                    content: serde_json::Value::String(INTERRUPTED_TOOL_RESULT.to_string()),
                    is_error: true,
                })
                .collect::<Vec<_>>();
            let merged_into_next = parsed.get_mut(index + 1).is_some_and(|(_, next)| {
                if next.role != Role::User
                    || !next
                        .content
                        .iter()
                        .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
                {
                    return false;
                }
                next.content.extend(replacements.clone());
                true
            });
            if !merged_into_next {
                repaired.push((
                    None,
                    Message {
                        role: Role::User,
                        content: replacements,
                    },
                ));
            }
        }
        if repaired_count > 0 {
            warnings.push(format!(
                "repaired {} interrupted tool call(s)",
                repaired_count
            ));
        }
        let inherited_turns = self
            .turn_index()?
            .map(|index| {
                index
                    .turns
                    .into_iter()
                    .filter(|turn| {
                        parsed.iter().any(|(line, message)| {
                            *line == turn.prompt_line && is_prompt_message(message)
                        })
                    })
                    .map(|mut turn| {
                        if turn.status == TurnStatus::Started {
                            turn.status = TurnStatus::Error;
                        }
                        turn
                    })
                    .collect()
            })
            .unwrap_or_default();
        let inherited_turns = remap_turn_lines_optional(inherited_turns, &repaired);
        let messages = repaired
            .into_iter()
            .map(|(_, message)| message)
            .collect::<Vec<_>>();
        self.create_fork(
            home,
            cwd,
            ForkReason::RecoverInterrupted,
            messages,
            inherited_turns,
            warnings,
        )
    }

    fn create_fork(
        &self,
        home: &Path,
        cwd: &Path,
        reason: ForkReason,
        messages: Vec<Message>,
        turns: Vec<TurnRecord>,
        warnings: Vec<String>,
    ) -> Result<ForkResult, TranscriptError> {
        let child = create_reserved(home, cwd)?;
        child.replace_messages(&messages)?;
        let index = TurnIndex {
            schema_version: TURN_INDEX_SCHEMA_VERSION,
            transcript_revision: child.transcript_revision()?,
            parent_session_id: Some(self.name()),
            fork_reason: Some(reason),
            turns,
        };
        child.write_turn_index(&index)?;
        Ok(ForkResult {
            transcript: child,
            warnings,
        })
    }

    fn read_source(&self) -> Result<Vec<u8>, TranscriptError> {
        use std::io::{Read, Seek};

        let mut active_lock = self
            .active_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some((_, file)) = active_lock.as_mut() {
            file.seek(std::io::SeekFrom::Start(0))?;
            let mut content = Vec::new();
            file.read_to_end(&mut content)?;
            Ok(content)
        } else {
            Ok(std::fs::read(&self.path)?)
        }
    }

    fn write_turn_index(&self, index: &TurnIndex) -> Result<(), TranscriptError> {
        let path = self.turn_index_path();
        let mut body = serde_json::to_vec_pretty(index)?;
        body.push(b'\n');
        replace_file(&path, &body)?;
        Ok(())
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
        let old_index_path = self.turn_index_path();
        let new_index_path = new_path.with_extension("turns.json");
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
        if old_index_path.exists()
            && let Err(error) = std::fs::rename(&old_index_path, &new_index_path)
        {
            let _ = std::fs::rename(&new_lock_path, &old_lock_path);
            if let Err(rollback) = std::fs::rename(&new_path, &self.path) {
                return Err(TranscriptError::Io(std::io::Error::other(format!(
                    "failed to rename turn index ({error}); transcript rollback failed: {rollback}"
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

    /// Release this process' file handles before handing the transcript to another process.
    pub(crate) fn release_active_lock(&self) {
        let files = self
            .active_lock
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        drop(files);
    }

    fn ensure_initial_session_record(&self) -> Result<(), TranscriptError> {
        use std::io::{Seek, Write};

        let Some(metadata) = self.initial_session.as_ref() else {
            return Ok(());
        };
        let mut active_lock = self.ensure_active_lock()?;
        let file = active_lock.as_mut().map(|(_, file)| file).ok_or_else(|| {
            TranscriptError::Io(std::io::Error::other("transcript active lock missing"))
        })?;
        if file.metadata()?.len() == 0 {
            file.seek(std::io::SeekFrom::Start(0))?;
            writeln!(
                file,
                "{}",
                serde_json::to_string(&session_record(metadata))?
            )?;
            file.flush()?;
        }
        Ok(())
    }

    /// Remove this session's transcript. A never-written transcript is already deleted.
    pub fn delete(&self) -> Result<(), TranscriptError> {
        self.release_active_lock();
        match std::fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(TranscriptError::Io(error)),
        }
        for sidecar in [
            self.turn_index_path(),
            self.path.with_extension("jsonl.lock"),
        ] {
            match std::fs::remove_file(sidecar) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(TranscriptError::Io(error)),
            }
        }
        Ok(())
    }

    /// Full-file rewrite (persisted after a manual /compact).
    pub fn replace_messages(&self, messages: &[Message]) -> Result<(), TranscriptError> {
        use std::io::{Read, Seek, Write};
        let mut active_lock = self.ensure_active_lock()?;
        let file = active_lock.as_mut().map(|(_, file)| file).ok_or_else(|| {
            TranscriptError::Io(std::io::Error::other("transcript active lock missing"))
        })?;
        file.seek(std::io::SeekFrom::Start(0))?;
        let mut previous_source = Vec::new();
        file.read_to_end(&mut previous_source)?;
        let session =
            latest_session_metadata(&previous_source).or_else(|| self.initial_session.clone());
        file.set_len(0)?;
        file.seek(std::io::SeekFrom::Start(0))?;
        let mut source_lines = Vec::new();
        if let Some(metadata) = session {
            let line = serde_json::to_string(&session_record(&metadata))?;
            writeln!(file, "{line}")?;
            source_lines.push(line);
        }
        for message in messages {
            let line = serde_json::to_string(message)?;
            writeln!(file, "{line}")?;
            source_lines.push(line);
        }
        let mut source = source_lines.join("\n");
        if !source.is_empty() {
            source.push('\n');
        }
        if let Some(mut index) = self.turn_index()? {
            index.turns.clear();
            index.transcript_revision = revision(source.as_bytes());
            self.write_turn_index(&index)?;
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
        if file.metadata()?.len() == 0
            && let Some(metadata) = self.initial_session.as_ref()
        {
            file.seek(std::io::SeekFrom::Start(0))?;
            writeln!(
                file,
                "{}",
                serde_json::to_string(&session_record(metadata))?
            )?;
        }
        file.seek(std::io::SeekFrom::End(0))?;
        let line = serde_json::to_string(message)?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Load all history messages (for --continue resume).
    /// Bad lines are skipped and counted with a warning: one truncated JSONL line must
    /// not make the whole session unrecoverable.
    pub fn load_messages(&self) -> Result<Vec<Message>, TranscriptError> {
        let content = String::from_utf8(self.read_source()?).map_err(|error| {
            TranscriptError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })?;
        let mut messages = Vec::new();
        let mut skipped = 0usize;
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match parse_transcript_line(line) {
                Ok(TranscriptLine::Message(message)) => messages.push(message),
                Ok(TranscriptLine::Session(Some(_))) => {}
                Ok(TranscriptLine::Session(None)) | Err(_) => skipped += 1,
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

fn revision(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn physical_line_count(source: &[u8]) -> usize {
    source.iter().filter(|byte| **byte == b'\n').count()
        + usize::from(!source.is_empty() && !source.ends_with(b"\n"))
}

type ParsedTranscriptLines = (Vec<(u64, Message)>, Vec<String>);

fn parsed_lines(
    source: &[u8],
    allow_truncated_tail: bool,
) -> Result<ParsedTranscriptLines, TranscriptError> {
    let text = std::str::from_utf8(source).map_err(|error| {
        TranscriptError::ForkPointUnavailable(format!("transcript is not UTF-8: {error}"))
    })?;
    let physical_count = physical_line_count(source);
    let mut messages = Vec::new();
    let mut warnings = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_transcript_line(line) {
            Ok(TranscriptLine::Message(message)) => {
                messages.push(((index + 1) as u64, message));
            }
            Ok(TranscriptLine::Session(Some(_))) => {}
            Ok(TranscriptLine::Session(None)) => {
                warnings.push(format!(
                    "discarded invalid session metadata on line {}",
                    index + 1
                ));
            }
            Err(_error)
                if allow_truncated_tail
                    && index + 1 == physical_count
                    && !source.ends_with(b"\n") =>
            {
                warnings.push(format!(
                    "discarded truncated transcript tail on line {}",
                    index + 1
                ));
            }
            Err(error) => {
                return Err(TranscriptError::ForkPointUnavailable(format!(
                    "transcript line {} is corrupt: {error}",
                    index + 1
                )));
            }
        }
    }
    Ok((messages, warnings))
}

fn is_prompt_message(message: &Message) -> bool {
    message.role == Role::User
        && message.content.iter().any(|block| {
            matches!(
                block,
                ContentBlock::Text { .. } | ContentBlock::Image { .. }
            )
        })
        && !message
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
}

fn remap_turn_lines(turns: Vec<TurnRecord>, messages: &[(u64, Message)]) -> Vec<TurnRecord> {
    let messages = messages
        .iter()
        .map(|(line, message)| (Some(*line), message.clone()))
        .collect::<Vec<_>>();
    remap_turn_lines_optional(turns, &messages)
}

fn remap_turn_lines_optional(
    turns: Vec<TurnRecord>,
    messages: &[(Option<u64>, Message)],
) -> Vec<TurnRecord> {
    let line_map = messages
        .iter()
        .enumerate()
        .filter_map(|(index, (source_line, _))| {
            source_line.map(|source_line| (source_line, (index + 2) as u64))
        })
        .collect::<HashMap<_, _>>();
    turns
        .into_iter()
        .filter_map(|mut turn| {
            turn.prompt_line = *line_map.get(&turn.prompt_line)?;
            Some(turn)
        })
        .collect()
}

fn replace_file(path: &Path, content: &[u8]) -> Result<(), std::io::Error> {
    let tmp = path.with_extension(format!(
        "{}.tmp-{}",
        path.extension()
            .map(|extension| extension.to_string_lossy())
            .unwrap_or_default(),
        std::process::id()
    ));
    std::fs::write(&tmp, content)?;
    if !path.exists() {
        return std::fs::rename(tmp, path);
    }
    let backup = path.with_extension(format!(
        "{}.bak-{}",
        path.extension()
            .map(|extension| extension.to_string_lossy())
            .unwrap_or_default(),
        std::process::id()
    ));
    std::fs::rename(path, &backup)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => {
            let _ = std::fs::remove_file(backup);
            Ok(())
        }
        Err(error) => {
            let rollback = std::fs::rename(&backup, path);
            let _ = std::fs::remove_file(tmp);
            match rollback {
                Ok(()) => Err(error),
                Err(rollback) => Err(std::io::Error::other(format!(
                    "failed to replace {} ({error}); rollback failed: {rollback}",
                    path.display()
                ))),
            }
        }
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

    #[test]
    fn reserved_session_writes_workspace_before_messages() {
        let tmp = std::env::temp_dir().join(format!(
            "bingo-transcript-session-record-{}",
            std::process::id()
        ));
        let home = tmp.join("home");
        let project = tmp.join("project");
        std::fs::create_dir_all(&project).unwrap();

        let transcript = create_reserved(&home, &project).unwrap();
        let initial = String::from_utf8(transcript.read_source().unwrap()).unwrap();
        let lines = initial.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);
        let record: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(record["type"], "session");
        assert_eq!(record["schemaVersion"], SESSION_SCHEMA_VERSION);
        assert!(Path::new(record["cwd"].as_str().unwrap()).is_absolute());

        transcript.append(&Message::user_text("hello")).unwrap();
        assert_eq!(
            transcript.load_messages().unwrap(),
            vec![Message::user_text("hello")]
        );
        let source = String::from_utf8(transcript.read_source().unwrap()).unwrap();
        assert!(
            source
                .lines()
                .next()
                .unwrap()
                .contains("\"type\":\"session\"")
        );
    }

    #[test]
    fn legacy_messages_load_without_workspace_metadata() {
        let tmp = std::env::temp_dir().join(format!(
            "bingo-transcript-legacy-session-record-{}",
            std::process::id()
        ));
        let path = tmp.join("legacy.jsonl");
        std::fs::create_dir_all(&tmp).unwrap();
        let message = Message::user_text("legacy");
        std::fs::write(
            &path,
            format!("{}\n", serde_json::to_string(&message).unwrap()),
        )
        .unwrap();
        let transcript = Transcript::at(path);

        assert_eq!(transcript.session_metadata().unwrap(), None);
        assert_eq!(transcript.load_messages().unwrap(), vec![message]);
    }

    #[test]
    fn binding_appends_without_rewriting_legacy_prefix_and_last_valid_record_wins() {
        let tmp = std::env::temp_dir().join(format!(
            "bingo-transcript-bind-session-record-{}",
            std::process::id()
        ));
        let first_project = tmp.join("first");
        let second_project = tmp.join("second");
        std::fs::create_dir_all(&first_project).unwrap();
        std::fs::create_dir_all(&second_project).unwrap();
        let path = tmp.join("legacy.jsonl");
        let message = Message::user_text("keep these bytes");
        let prefix = format!("{}\n", serde_json::to_string(&message).unwrap()).into_bytes();
        std::fs::write(&path, &prefix).unwrap();
        let transcript = Transcript::at(path);

        transcript.bind_workspace(&first_project).unwrap();
        transcript.bind_workspace(&second_project).unwrap();
        {
            use std::io::Write;
            let mut lock = transcript.ensure_active_lock().unwrap();
            let file = lock.as_mut().unwrap_or_else(|| unreachable!()).1.by_ref();
            writeln!(
                file,
                "{{\"type\":\"session\",\"schemaVersion\":2,\"cwd\":\"relative\"}}"
            )
            .unwrap();
        }

        let source = transcript.read_source().unwrap();
        assert!(source.starts_with(&prefix));
        assert_eq!(transcript.load_messages().unwrap(), vec![message]);
        assert_eq!(
            transcript.session_metadata().unwrap().unwrap().cwd,
            canonical_session_metadata(&second_project).unwrap().cwd
        );
    }

    #[test]
    fn binding_refreshes_turn_index_and_keeps_edit_forks_available() {
        let tmp = std::env::temp_dir().join(format!(
            "bingo-transcript-bind-index-{}",
            std::process::id()
        ));
        let home = tmp.join("home");
        let first_project = tmp.join("first");
        let second_project = tmp.join("second");
        std::fs::create_dir_all(&first_project).unwrap();
        std::fs::create_dir_all(&second_project).unwrap();
        let transcript = create_reserved(&home, &first_project).unwrap();
        let prompt_revision = transcript.begin_turn("turn-1", "first").unwrap();
        transcript.append(&Message::user_text("first")).unwrap();
        transcript
            .append(&Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "answer".to_string(),
                }],
            })
            .unwrap();
        transcript
            .finish_turn("turn-1", TurnStatus::Completed)
            .unwrap();
        let prefix = transcript.read_source().unwrap();

        transcript.bind_workspace(&second_project).unwrap();

        assert!(transcript.read_source().unwrap().starts_with(&prefix));
        assert_eq!(
            transcript
                .turn_index()
                .unwrap()
                .unwrap()
                .transcript_revision,
            transcript.transcript_revision().unwrap()
        );
        let fork = transcript
            .fork_edit_last_prompt(
                &home,
                &second_project,
                EditForkPoint {
                    turn_id: Some("turn-1"),
                    content_revision: Some(&prompt_revision),
                },
            )
            .unwrap();
        assert_eq!(
            fork.transcript.load_messages().unwrap(),
            Vec::<Message>::new()
        );
    }

    #[test]
    fn replace_messages_keeps_only_latest_valid_workspace_record() {
        let tmp = std::env::temp_dir().join(format!(
            "bingo-transcript-rewrite-session-record-{}",
            std::process::id()
        ));
        let home = tmp.join("home");
        let first_project = tmp.join("first");
        let second_project = tmp.join("second");
        std::fs::create_dir_all(&first_project).unwrap();
        std::fs::create_dir_all(&second_project).unwrap();
        let transcript = create_reserved(&home, &first_project).unwrap();
        transcript.append(&Message::user_text("before")).unwrap();
        transcript.bind_workspace(&second_project).unwrap();

        transcript
            .replace_messages(&[Message::user_text("after")])
            .unwrap();

        let source = String::from_utf8(transcript.read_source().unwrap()).unwrap();
        let session_records = source
            .lines()
            .filter(|line| line.contains("\"type\":\"session\""))
            .count();
        assert_eq!(session_records, 1);
        assert_eq!(
            transcript.load_messages().unwrap(),
            vec![Message::user_text("after")]
        );
        assert_eq!(
            transcript.session_metadata().unwrap().unwrap().cwd,
            canonical_session_metadata(&second_project).unwrap().cwd
        );
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
            let mut active_lock = transcript.ensure_active_lock().unwrap();
            let file = active_lock
                .as_mut()
                .map(|(_, file)| file)
                .unwrap_or_else(|| unreachable!("active transcript has a file handle"));
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
        let prompt_revision = transcript.begin_turn("turn-1", "active").unwrap();
        transcript.append(&Message::user_text("active")).unwrap();
        transcript
            .finish_turn("turn-1", TurnStatus::Completed)
            .unwrap();
        let old_lock_path = transcript.path().with_extension("jsonl.lock");
        let old_index_path = transcript.turn_index_path();

        let renamed = transcript.rename("named").unwrap();
        let new_lock_path = renamed.path().with_extension("jsonl.lock");
        let new_index_path = renamed.turn_index_path();
        let competing_lock = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&new_lock_path)
            .unwrap();

        assert!(!old_lock_path.exists());
        assert!(!old_index_path.exists());
        assert!(new_lock_path.exists());
        assert!(new_index_path.exists());
        assert_eq!(
            renamed.session_metadata().unwrap().unwrap().cwd,
            canonical_session_metadata(&tmp).unwrap().cwd
        );
        assert_eq!(
            renamed.turn_index().unwrap().unwrap().turns[0].content_revision,
            prompt_revision
        );
        assert!(matches!(
            competing_lock.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn releasing_active_lock_makes_a_transcript_available_to_another_process() {
        let tmp = std::env::temp_dir().join(format!(
            "bingo-transcript-release-lock-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let transcript = create_reserved(&home, &tmp).unwrap();
        let lock_path = transcript.path().with_extension("jsonl.lock");
        let competing_lock = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)
            .unwrap();

        assert!(matches!(
            competing_lock.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));

        transcript.release_active_lock();

        competing_lock
            .try_lock()
            .expect("the handoff must release both active file handles");
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn edit_fork_preserves_source_bytes_and_copies_only_history_before_last_prompt() {
        let tmp =
            std::env::temp_dir().join(format!("bingo-transcript-edit-fork-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let source = create_reserved(&home, &tmp).unwrap();
        let first_revision = source.begin_turn("turn-1", "first").unwrap();
        source.append(&Message::user_text("first")).unwrap();
        source.finish_turn("turn-1", TurnStatus::Completed).unwrap();
        source
            .append(&Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "answer".to_string(),
                }],
            })
            .unwrap();
        let second_revision = source.begin_turn("turn-2", "second\n\n#[image 0]").unwrap();
        source
            .append(&Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Text {
                        text: "second".to_string(),
                    },
                    ContentBlock::Image {
                        source: crate::api::types::ImageSource::base64("image/png", "aA=="),
                    },
                ],
            })
            .unwrap();
        source
            .append(&Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "old answer".to_string(),
                }],
            })
            .unwrap();
        source.finish_turn("turn-2", TurnStatus::Completed).unwrap();
        let source_bytes = source.read_source().unwrap();

        let fork = source
            .fork_edit_last_prompt(
                &home,
                &tmp,
                EditForkPoint {
                    turn_id: Some("turn-2"),
                    content_revision: Some(&second_revision),
                },
            )
            .unwrap();

        assert_eq!(source.read_source().unwrap(), source_bytes);
        assert_eq!(
            fork.transcript.load_messages().unwrap(),
            vec![
                Message::user_text("first"),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "answer".to_string()
                    }]
                }
            ]
        );
        let index = fork.transcript.turn_index().unwrap().unwrap();
        assert_eq!(
            index.parent_session_id.as_deref(),
            Some(source.name().as_str())
        );
        assert_eq!(index.fork_reason, Some(ForkReason::EditLastPrompt));
        assert_eq!(index.turns.len(), 1);
        assert_eq!(index.turns[0].content_revision, first_revision);
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn recovery_fork_drops_only_truncated_tail_and_repairs_orphan_tool_use() {
        let tmp = std::env::temp_dir().join(format!(
            "bingo-transcript-recovery-fork-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let source = create_reserved(&home, &tmp).unwrap();
        source.begin_turn("turn-1", "run it").unwrap();
        source.append(&Message::user_text("run it")).unwrap();
        source
            .append(&Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "tool-1".to_string(),
                    name: "Bash".to_string(),
                    input: serde_json::json!({"command": "build"}),
                }],
            })
            .unwrap();
        {
            use std::io::{Seek, Write};
            let mut lock = source.ensure_active_lock().unwrap();
            let file = lock.as_mut().unwrap_or_else(|| unreachable!()).1.by_ref();
            file.seek(std::io::SeekFrom::End(0)).unwrap();
            write!(file, "{{\"role\":\"assistant\"").unwrap();
        }
        let source_bytes = source.read_source().unwrap();

        let fork = source.fork_recover_interrupted(&home, &tmp).unwrap();

        assert_eq!(fork.warnings.len(), 2);
        let messages = fork.transcript.load_messages().unwrap();
        assert_eq!(messages.len(), 3);
        assert!(matches!(
            &messages[2].content[0],
            ContentBlock::ToolResult { tool_use_id, is_error: true, .. } if tool_use_id == "tool-1"
        ));
        assert_eq!(source.read_source().unwrap(), source_bytes);
        assert_eq!(
            fork.transcript.turn_index().unwrap().unwrap().turns[0].status,
            TurnStatus::Error
        );
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn edit_fork_remaps_turn_lines_after_extra_workspace_records() {
        let tmp = std::env::temp_dir().join(format!(
            "bingo-transcript-edit-remap-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let first_project = tmp.join("first");
        let second_project = tmp.join("second");
        std::fs::create_dir_all(&first_project).unwrap();
        std::fs::create_dir_all(&second_project).unwrap();
        let source = create_reserved(&home, &first_project).unwrap();
        source.bind_workspace(&second_project).unwrap();

        let first_revision = source.begin_turn("turn-1", "first").unwrap();
        source.append(&Message::user_text("first")).unwrap();
        source
            .append(&Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "answer".to_string(),
                }],
            })
            .unwrap();
        source.finish_turn("turn-1", TurnStatus::Completed).unwrap();
        let second_revision = source.begin_turn("turn-2", "second").unwrap();
        source.append(&Message::user_text("second")).unwrap();
        source.finish_turn("turn-2", TurnStatus::Completed).unwrap();

        let fork = source
            .fork_edit_last_prompt(
                &home,
                &second_project,
                EditForkPoint {
                    turn_id: Some("turn-2"),
                    content_revision: Some(&second_revision),
                },
            )
            .unwrap();
        let child_index = fork.transcript.turn_index().unwrap().unwrap();

        assert_eq!(child_index.turns.len(), 1);
        assert_eq!(child_index.turns[0].turn_id, "turn-1");
        assert_eq!(child_index.turns[0].prompt_line, 2);
        assert_eq!(child_index.turns[0].content_revision, first_revision);
        assert_eq!(
            fork.transcript.load_messages().unwrap(),
            vec![
                Message::user_text("first"),
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: "answer".to_string(),
                    }],
                },
            ]
        );
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn recovery_fork_merges_missing_tool_results_and_remaps_turn_lines() {
        let tmp = std::env::temp_dir().join(format!(
            "bingo-transcript-recovery-remap-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        let first_project = tmp.join("first");
        let second_project = tmp.join("second");
        std::fs::create_dir_all(&first_project).unwrap();
        std::fs::create_dir_all(&second_project).unwrap();
        let source = create_reserved(&home, &first_project).unwrap();
        source.bind_workspace(&second_project).unwrap();
        source.begin_turn("turn-1", "run both").unwrap();
        source.append(&Message::user_text("run both")).unwrap();
        source
            .append(&Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::ToolUse {
                        id: "tool-1".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"path": "one"}),
                    },
                    ContentBlock::ToolUse {
                        id: "tool-2".to_string(),
                        name: "Read".to_string(),
                        input: serde_json::json!({"path": "two"}),
                    },
                ],
            })
            .unwrap();
        source
            .append(&Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "tool-1".to_string(),
                    content: serde_json::Value::String("done".to_string()),
                    is_error: false,
                }],
            })
            .unwrap();
        {
            use std::io::{Seek, Write};
            let mut lock = source.ensure_active_lock().unwrap();
            let file = lock.as_mut().unwrap_or_else(|| unreachable!()).1.by_ref();
            file.seek(std::io::SeekFrom::End(0)).unwrap();
            write!(file, "{{\"role\":\"assistant\"").unwrap();
        }

        let fork = source
            .fork_recover_interrupted(&home, &second_project)
            .unwrap();
        let messages = fork.transcript.load_messages().unwrap();
        let results = messages[2]
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::ToolResult {
                    tool_use_id,
                    is_error,
                    ..
                } => Some((tool_use_id.as_str(), *is_error)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            messages.len(),
            3,
            "repair reuses the existing tool-result message"
        );
        assert_eq!(results, vec![("tool-1", false), ("tool-2", true)]);
        assert!(
            fork.warnings
                .iter()
                .any(|warning| warning.contains("truncated"))
        );
        assert!(
            fork.warnings
                .iter()
                .any(|warning| warning.contains("repaired 1"))
        );
        let child_index = fork.transcript.turn_index().unwrap().unwrap();
        assert_eq!(child_index.turns[0].prompt_line, 2);
        assert_eq!(child_index.turns[0].status, TurnStatus::Error);
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn edit_fork_rejects_a_transcript_changed_after_indexing() {
        let tmp = std::env::temp_dir().join(format!(
            "bingo-transcript-edit-stale-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let source = create_reserved(&home, &tmp).unwrap();
        let prompt_revision = source.begin_turn("turn-1", "prompt").unwrap();
        source.append(&Message::user_text("prompt")).unwrap();
        source.finish_turn("turn-1", TurnStatus::Completed).unwrap();
        source
            .append(&Message {
                role: Role::Assistant,
                content: vec![ContentBlock::Text {
                    text: "changed after indexing".to_string(),
                }],
            })
            .unwrap();

        assert!(matches!(
            source.fork_edit_last_prompt(
                &home,
                &tmp,
                EditForkPoint {
                    turn_id: Some("turn-1"),
                    content_revision: Some(&prompt_revision),
                },
            ),
            Err(TranscriptError::SessionStale(_))
        ));
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn replace_and_delete_keep_the_turn_index_lifecycle_in_sync() {
        let tmp = std::env::temp_dir().join(format!(
            "bingo-transcript-index-lifecycle-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let transcript = create_reserved(&home, &tmp).unwrap();
        transcript.begin_turn("turn-1", "before").unwrap();
        transcript.append(&Message::user_text("before")).unwrap();
        transcript
            .finish_turn("turn-1", TurnStatus::Completed)
            .unwrap();

        transcript
            .replace_messages(&[Message::user_text("compacted")])
            .unwrap();
        let index = transcript.turn_index().unwrap().unwrap();
        assert!(index.turns.is_empty());
        assert_eq!(
            index.transcript_revision,
            transcript.transcript_revision().unwrap()
        );

        let transcript_path = transcript.path().to_path_buf();
        let index_path = transcript.turn_index_path();
        let lock_path = transcript.path().with_extension("jsonl.lock");
        assert!(lock_path.exists());
        transcript.delete().unwrap();
        assert!(!transcript_path.exists());
        assert!(!index_path.exists());
        assert!(!lock_path.exists());
        let _ = std::fs::remove_dir_all(tmp);
    }

    #[test]
    fn legacy_edit_requires_one_unambiguous_top_level_prompt() {
        let tmp = std::env::temp_dir().join(format!(
            "bingo-transcript-legacy-fork-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let home = tmp.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let source = create_reserved(&home, &tmp).unwrap();
        source.append(&Message::user_text("first")).unwrap();
        source.append(&Message::user_text("second")).unwrap();
        assert!(matches!(
            source.fork_edit_last_prompt(
                &home,
                &tmp,
                EditForkPoint {
                    turn_id: None,
                    content_revision: None
                }
            ),
            Err(TranscriptError::ForkPointUnavailable(_))
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

//! Share data source: the session's persisted snapshot (the input for `bingo share`).
//!
//! At runtime the session incrementally writes subagent instances and channel logs into `ShareDoc` (JSON, single file,
//! atomic write: tmp + rename); `bingo share` reads that document + the transcript to generate
//! a self-contained HTML page. Share is an enhancement, not a contract: storage failures only warn and never block the session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agents::AgentState;
use crate::api::types::{ContentBlock, Message};
use crate::channels::{ChannelMessage, ChannelMode};
use crate::error::ErrorCode;
use crate::transcript::Transcript;

#[derive(Debug, Error)]
pub enum ShareError {
    #[error("share io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("share json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("transcript error: {0}")]
    Transcript(#[from] crate::transcript::TranscriptError),
    #[error("no transcript sessions found")]
    NoSessions,
    #[error("no transcript matches '{0}'")]
    SessionNotFound(String),
    #[error("share upload failed: {0}")]
    Upload(String),
}

impl ErrorCode for ShareError {
    fn error_code(&self) -> &'static str {
        match self {
            ShareError::Io(_)
            | ShareError::Json(_)
            | ShareError::Transcript(_)
            | ShareError::NoSessions
            | ShareError::SessionNotFound(_)
            | ShareError::Upload(_) => "STORAGE_ERROR",
        }
    }
}

/// A shared snapshot of a subagent instance (history = the full resume history, i.e. the DM view).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentShare {
    pub name: String,
    /// Named definition (AgentDef name; None for unnamed instances).
    pub def: Option<String>,
    pub description: String,
    /// Instance state (AgentState in string form: running / idle / stopped).
    pub state: String,
    pub history: Vec<Message>,
}

/// A shared snapshot of a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelShare {
    pub name: String,
    /// Speak mode (ChannelMode in string form: serial / free).
    pub mode: String,
    pub members: Vec<String>,
    pub messages: Vec<ChannelMessage>,
}

/// The whole session's shared document (single JSON file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareDoc {
    /// Session key (transcript file-name stem `{slug}-{ts}`).
    pub session: String,
    /// First-created time (unix seconds).
    pub created_at: u64,
    pub agents: Vec<AgentShare>,
    pub channels: Vec<ChannelShare>,
}

impl ShareDoc {
    pub fn new(session: String) -> Self {
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            session,
            created_at,
            agents: Vec::new(),
            channels: Vec::new(),
        }
    }
}

/// The shares dir: ~/.local/share/bingo/shares (sibling of transcripts).
pub fn shares_dir(home: &Path) -> PathBuf {
    crate::storage::shares_dir(home)
}

pub fn rename_session_sidecars(home: &Path, old: &str, new: &str) -> Result<(), ShareError> {
    let dir = shares_dir(home);
    std::fs::create_dir_all(&dir)?;
    let old_lock_path = dir.join(format!("{old}.json.lock"));
    let old_lock = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&old_lock_path)?;
    old_lock.lock()?;
    for suffix in ["json", "json.bak", "json.tmp"] {
        let old_path = dir.join(format!("{old}.{suffix}"));
        if !old_path.exists() {
            continue;
        }
        let new_path = dir.join(format!("{new}.{suffix}"));
        std::fs::rename(old_path, new_path)?;
    }
    let new_lock_path = dir.join(format!("{new}.json.lock"));
    let _ = std::fs::rename(&old_lock_path, &new_lock_path);
    let path = dir.join(format!("{new}.json"));
    if path.exists() {
        let mut doc: ShareDoc = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        doc.session = new.to_string();
        std::fs::write(&path, serde_json::to_string_pretty(&doc)?)?;
    }
    drop(old_lock);
    Ok(())
}

/// Per-session shared-document store (Session holds an Arc; sub-sessions share it via the registry).
pub struct ShareStore {
    path: PathBuf,
    inner: Mutex<ShareDoc>,
    save_lock: Mutex<()>,
}

impl ShareStore {
    /// Read an existing document; when missing, create an empty one keyed by the path stem.
    /// A corrupt file must not block the session: back it up as `<stem>.json.bak` and start fresh.
    pub fn load_or_create(path: &Path) -> Result<Arc<Self>, ShareError> {
        let session = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let doc = if path.exists() {
            let content = std::fs::read_to_string(path)?;
            match serde_json::from_str::<ShareDoc>(&content) {
                Ok(doc) => doc,
                Err(_) => {
                    let _ = std::fs::rename(path, path.with_extension("json.bak"));
                    ShareDoc::new(session)
                }
            }
        } else {
            ShareDoc::new(session)
        };
        Ok(Arc::new(Self {
            path: path.to_path_buf(),
            inner: Mutex::new(doc),
            save_lock: Mutex::new(()),
        }))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ShareDoc> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Snapshot of the current document (for `bingo share` reads).
    pub fn snapshot(&self) -> ShareDoc {
        self.lock().clone()
    }

    /// Create/update a subagent instance entry (history and state follow the latest).
    pub fn upsert_agent(
        &self,
        name: &str,
        def: Option<String>,
        description: String,
        state: AgentState,
        history: Vec<Message>,
    ) {
        let mut doc = self.lock();
        match doc.agents.iter_mut().find(|a| a.name == name) {
            Some(a) => {
                a.def = def;
                a.description = description;
                a.state = state.label().to_string();
                a.history = history;
            }
            None => doc.agents.push(AgentShare {
                name: name.to_string(),
                def,
                description,
                state: state.label().to_string(),
                history,
            }),
        }
    }

    /// Create/update a channel entry (mode and members follow the latest; messages are kept).
    pub fn upsert_channel_meta(&self, name: &str, mode: ChannelMode, members: Vec<String>) {
        let mut doc = self.lock();
        match doc.channels.iter_mut().find(|c| c.name == name) {
            Some(c) => {
                c.mode = mode.label().to_string();
                c.members = members;
            }
            None => doc.channels.push(ChannelShare {
                name: name.to_string(),
                mode: mode.label().to_string(),
                members,
                messages: Vec::new(),
            }),
        }
    }

    /// Append a channel message (ignored when the channel does not exist — metadata lands before messages).
    pub fn append_channel_message(&self, name: &str, msg: ChannelMessage) {
        if let Some(c) = self.lock().channels.iter_mut().find(|c| c.name == name) {
            c.messages.push(msg);
        }
    }

    /// Atomic write: tmp file + rename (readers see either the old or the new document).
    pub fn save(&self) -> Result<(), ShareError> {
        let _save_guard = self.save_lock.lock().unwrap_or_else(|e| e.into_inner());
        let doc = self.lock();
        let json = serde_json::to_string_pretty(&*doc)?;
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let lock_path = self.path.with_extension("json.lock");
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)?;
        lock_file.lock()?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// Write to disk and swallow errors (share is an enhancement, not a contract: a failure warns once).
    pub fn persist(&self) {
        if let Err(e) = self.save() {
            eprintln!("[bingo] warning: share save failed: {e}");
        }
    }
}

/// Where a share document's disk writes happen.
///
/// The session actor updates the document in memory — a lock held for the length
/// of a `push` — and then asks for a save. The write itself happens here, on a
/// thread of its own, because the actor is the process's one ordering point and
/// must never be found waiting on a file. A burst of changes coalesces into one
/// write, which is also why this is a queue rather than a spawn per change.
pub struct ShareSaver {
    requests: std::sync::mpsc::Sender<SaveRequest>,
}

enum SaveRequest {
    Save,
    // Nothing in a running session asks; the tests that assert on the file do.
    #[cfg_attr(not(test), allow(dead_code))]
    /// Answered once the write that follows it has happened. The actor hands
    /// this channel over rather than waiting on it — a barrier for whoever wants
    /// one, never a pause in the session.
    Flush(tokio::sync::oneshot::Sender<()>),
}

impl ShareSaver {
    pub fn spawn(store: Arc<ShareStore>) -> Self {
        let (requests, pending) = std::sync::mpsc::channel::<SaveRequest>();
        // Named for the same reason the actor's thread is: a stack in a crash
        // report should say whose work it was.
        let _ = std::thread::Builder::new()
            .name("bingo-share".to_string())
            .spawn(move || {
                while let Ok(request) = pending.recv() {
                    // Everything asked for while the last write was running is
                    // answered by the next one: the document is a snapshot, not
                    // a log.
                    let mut waiting = Vec::new();
                    let mut queued = Some(request);
                    while let Some(request) = queued.take() {
                        if let SaveRequest::Flush(ack) = request {
                            waiting.push(ack);
                        }
                        queued = pending.try_recv().ok();
                    }
                    store.persist();
                    for ack in waiting {
                        let _ = ack.send(());
                    }
                }
            });
        Self { requests }
    }

    /// Ask for the document as it now stands to reach the disk.
    pub fn save(&self) {
        let _ = self.requests.send(SaveRequest::Save);
    }

    /// Ask, and be told when it has.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn flush(&self, ack: tokio::sync::oneshot::Sender<()>) {
        if self.requests.send(SaveRequest::Flush(ack)).is_err() {
            // The writer is gone; nothing more will be written and the caller
            // learns it from the dropped channel.
        }
    }
}

/// Atomic file write (tmp + rename; the unified share-output entry, shared by the CLI and /share).
pub fn write_html_atomic(path: &Path, content: &str) -> Result<(), ShareError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("html.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Default base URL of the official upload service (overridable via settings.share.baseUrl).
pub const DEFAULT_SHARE_BASE: &str = "https://bingo.ruobin.dev";

/// Share id: the session stem's ts part + 6 random [a-z0-9] chars (no rand dependency;
/// splitmix64 mixes time and a counter).
pub fn share_id(stem: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let ts = stem
        .rsplit('-')
        .next()
        .unwrap_or("0")
        .chars()
        .filter(|c| c.is_ascii_digit())
        .take(10)
        .collect::<String>();
    let ts = if ts.is_empty() { "0".to_string() } else { ts };
    let mix = |mut z: u64| {
        z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let n = COUNTER
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        .wrapping_add(now);
    let mut z = mix(n);
    let mut suffix = String::with_capacity(6);
    for _ in 0..6 {
        suffix.push(ALPHABET[(z % 36) as usize] as char);
        z /= 36;
    }
    format!("{ts}{suffix}")
}

/// Upload HTML to the official share service (public, no token):
/// POST `{base}/share/u/{id}`, body = HTML.
pub async fn upload_share(base: &str, id: &str, html: &str) -> Result<String, ShareError> {
    let url = format!("{base}/share/u/{id}");
    let resp = reqwest::Client::new()
        .post(&url)
        .header("Content-Type", "text/html; charset=utf-8")
        .body(html.to_string())
        .send()
        .await
        .map_err(|e| ShareError::Upload(format!("{e}")))?;
    if !resp.status().is_success() {
        return Err(ShareError::Upload(format!("HTTP {}", resp.status())));
    }
    Ok(url)
}

/// Open the target in the system default browser (file path or URL; macOS open / Linux xdg-open / Windows cmd start).
pub fn open_in_browser(target: &str) -> Result<(), ShareError> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(target);
        c
    } else if cfg!(target_os = "linux") {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(target);
        c
    } else {
        let mut c = std::process::Command::new("cmd");
        c.arg("/c").arg("start").arg("").arg(target);
        c
    };
    cmd.spawn()?;
    Ok(())
}

/// Derive agents/channels from the main transcript's messages (legacy-session fallback: when the process started before share
/// landed and no share document exists, the export still contains the Team/DM/channel views instead of an empty state).
///
/// Derivation rules (best-effort; sender identity cannot be recovered exactly from the transcript):
/// - `Agent` tool_use → an AgentShare entry (name = instance name or agent-definition name,
///   description = the description or a prompt excerpt, state=idle, history empty)
/// - `SendMessage` → appends a user message to that agent's history
/// - `AgentControl stop/delete` → state=stopped
/// - `Channel create` → ChannelShare metadata (members include main/user)
/// - `Post` (pre-D98; `SendMessage(to: "#room")` since) → a channel message
///   (from=main, seq increments)
pub fn derive_share_doc(session: &str, messages: &[Message]) -> ShareDoc {
    let mut doc = ShareDoc::new(session.to_string());
    let mut agent_index: HashMap<String, usize> = HashMap::new();
    let mut channel_index: HashMap<String, usize> = HashMap::new();
    let mut next_agent = 1usize;
    for msg in messages {
        for block in &msg.content {
            let ContentBlock::ToolUse { name, input, .. } = block else {
                continue;
            };
            match name.as_str() {
                "Agent" => {
                    let instance = input
                        .get("name")
                        .and_then(|v| v.as_str())
                        .or_else(|| input.get("agent").and_then(|v| v.as_str()))
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            let n = next_agent;
                            next_agent += 1;
                            format!("agent-{n}")
                        });
                    if agent_index.contains_key(&instance) {
                        continue;
                    }
                    let def = input
                        .get("agent")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let description = input
                        .get("description")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            input
                                .get("prompt")
                                .and_then(|v| v.as_str())
                                .map(|s| s.chars().take(40).collect::<String>())
                                .unwrap_or_default()
                        });
                    agent_index.insert(instance.clone(), doc.agents.len());
                    doc.agents.push(AgentShare {
                        name: instance,
                        def,
                        description,
                        state: "idle".to_string(),
                        history: Vec::new(),
                    });
                }
                "SendMessage" => {
                    let (Some(agent), Some(message)) = (
                        input.get("agent").and_then(|v| v.as_str()),
                        input.get("message").and_then(|v| v.as_str()),
                    ) else {
                        continue;
                    };
                    if let Some(&idx) = agent_index.get(agent) {
                        doc.agents[idx].history.push(Message::user_text(message));
                    }
                }
                "AgentControl" => {
                    let (Some(action), Some(agent)) = (
                        input.get("action").and_then(|v| v.as_str()),
                        input.get("agent").and_then(|v| v.as_str()),
                    ) else {
                        continue;
                    };
                    if matches!(action, "stop" | "delete")
                        && let Some(&idx) = agent_index.get(agent)
                    {
                        doc.agents[idx].state = "stopped".to_string();
                    }
                }
                "Channel" => {
                    let Some(channel) = input
                        .get("channel")
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim_start_matches('#').to_string())
                    else {
                        continue;
                    };
                    if input.get("action").and_then(|v| v.as_str()) != Some("create")
                        || channel.is_empty()
                        || channel_index.contains_key(&channel)
                    {
                        continue;
                    }
                    let mode = input
                        .get("mode")
                        .and_then(|v| v.as_str())
                        .unwrap_or("serial")
                        .to_string();
                    let mut members = vec!["main".to_string(), "user".to_string()];
                    if let Some(list) = input.get("members").and_then(|v| v.as_array()) {
                        for m in list {
                            if let Some(m) = m.as_str()
                                && m != "main"
                                && m != "user"
                                && !members.iter().any(|x| x == m)
                            {
                                members.push(m.to_string());
                            }
                        }
                    }
                    channel_index.insert(channel.clone(), doc.channels.len());
                    doc.channels.push(ChannelShare {
                        name: channel,
                        mode,
                        members,
                        messages: Vec::new(),
                    });
                }
                "Post" => {
                    let (Some(channel), Some(text)) = (
                        input
                            .get("channel")
                            .and_then(|v| v.as_str())
                            .map(|s| s.trim_start_matches('#').to_string()),
                        input.get("message").and_then(|v| v.as_str()),
                    ) else {
                        continue;
                    };
                    if let Some(&idx) = channel_index.get(&channel) {
                        let seq = doc.channels[idx].messages.len() as u64 + 1;
                        doc.channels[idx].messages.push(ChannelMessage {
                            seq,
                            from: "main".to_string(),
                            text: text.to_string(),
                            at: 0,
                            kind: crate::channels::MessageKind::Said,
                        });
                    }
                }
                _ => {}
            }
        }
    }
    doc
}

/// Resolve the transcript by session key (/resume semantics: substring match, newest first);
/// without a key, take the newest session. On a miss, the error lists the available sessions (first 5, to avoid spam).
pub fn resolve_transcript(home: &Path, key: Option<&str>) -> Result<Transcript, ShareError> {
    let all = crate::transcript::list(home)?;
    match key {
        None => all.into_iter().next().ok_or(ShareError::NoSessions),
        Some(key) => {
            let names: Vec<String> = all.iter().map(|t| t.name()).collect();
            all.into_iter()
                .find(|t| t.name().contains(key))
                .ok_or_else(|| {
                    if names.is_empty() {
                        ShareError::SessionNotFound(key.to_string())
                    } else {
                        let shown: Vec<&str> = names.iter().map(String::as_str).take(5).collect();
                        let suffix = if names.len() > 5 { "…" } else { "" };
                        ShareError::SessionNotFound(format!(
                            "{key}; available sessions: {}{suffix}",
                            shown.join(", ")
                        ))
                    }
                })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::{ContentBlock, Role};

    fn msg(text: &str) -> Message {
        Message {
            role: Role::Assistant,
            content: vec![ContentBlock::Text { text: text.into() }],
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bingo-share-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn sample_store(path: &Path) -> Arc<ShareStore> {
        let store = ShareStore::load_or_create(path).unwrap_or_else(|e| panic!("{e}"));
        store.upsert_agent(
            "scout",
            Some("scout".to_string()),
            "research".to_string(),
            AgentState::Idle,
            vec![msg("hi")],
        );
        store.upsert_channel_meta(
            "table",
            ChannelMode::Free,
            vec!["main".into(), "user".into(), "scout".into()],
        );
        store.append_channel_message(
            "table",
            ChannelMessage {
                seq: 1,
                from: "scout".into(),
                text: "hello everyone".into(),
                at: 0,
                kind: crate::channels::MessageKind::Said,
            },
        );
        store
    }

    #[test]
    fn rename_session_sidecars_moves_snapshot_and_updates_key() {
        let home = temp_dir("rename-sidecars");
        let old_path = shares_dir(&home).join("old.json");
        let store = sample_store(&old_path);
        store.persist();
        let old_backup = shares_dir(&home).join("old.json.bak");
        std::fs::write(&old_backup, "backup").unwrap();

        rename_session_sidecars(&home, "old", "new").unwrap();

        assert!(!old_path.exists());
        assert!(!old_backup.exists());
        assert!(shares_dir(&home).join("new.json.bak").exists());
        let reloaded = ShareStore::load_or_create(&shares_dir(&home).join("new.json"))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(reloaded.snapshot().session, "new");
        assert_eq!(reloaded.snapshot().agents.len(), 1);
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn roundtrip_save_and_load() {
        let root = temp_dir("roundtrip");
        let path = root.join("shares").join("bingo-1.json");
        let store = sample_store(&path);
        store.persist();
        assert!(path.exists());

        let reloaded = ShareStore::load_or_create(&path).unwrap_or_else(|e| panic!("{e}"));
        let doc = reloaded.snapshot();
        assert_eq!(doc.session, "bingo-1");
        assert_eq!(doc.agents.len(), 1);
        assert_eq!(doc.agents[0].name, "scout");
        assert_eq!(doc.agents[0].def.as_deref(), Some("scout"));
        assert_eq!(doc.agents[0].state, "idle");
        assert_eq!(doc.agents[0].history.len(), 1);
        assert_eq!(doc.channels.len(), 1);
        assert_eq!(doc.channels[0].mode, "free");
        assert_eq!(doc.channels[0].members, vec!["main", "user", "scout"]);
        assert_eq!(doc.channels[0].messages.len(), 1);
        assert_eq!(doc.channels[0].messages[0].seq, 1);
        assert_eq!(doc.channels[0].messages[0].from, "scout");
        assert_eq!(doc.channels[0].messages[0].text, "hello everyone");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_or_create_missing_uses_stem_as_session() {
        let root = temp_dir("missing");
        let path = root.join("shares").join("proj-1700000000.json");
        let store = ShareStore::load_or_create(&path).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(store.snapshot().session, "proj-1700000000");
        assert!(store.snapshot().agents.is_empty());
        assert!(store.snapshot().channels.is_empty());
        assert!(!path.exists(), "read-only, nothing written");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_file_is_backed_up_and_recreated() {
        let root = temp_dir("corrupt");
        let path = root.join("shares").join("bingo-1.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "not json {{{").unwrap();
        let store = ShareStore::load_or_create(&path).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(store.snapshot().session, "bingo-1");
        assert!(
            store.snapshot().agents.is_empty(),
            "corrupt file starts fresh"
        );
        assert!(
            path.with_extension("json.bak").exists(),
            "corrupt file backup is kept"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn upsert_updates_in_place() {
        let root = temp_dir("upsert");
        let path = root.join("shares").join("bingo-1.json");
        let store = ShareStore::load_or_create(&path).unwrap_or_else(|e| panic!("{e}"));
        store.upsert_agent(
            "a",
            None,
            "first version".into(),
            AgentState::Running,
            Vec::new(),
        );
        store.upsert_agent(
            "a",
            None,
            "new version".into(),
            AgentState::Idle,
            vec![msg("x")],
        );
        store.upsert_agent(
            "b",
            None,
            "another person".into(),
            AgentState::Stopped,
            Vec::new(),
        );
        let doc = store.snapshot();
        assert_eq!(
            doc.agents.len(),
            2,
            "same-name updates do not create duplicate entries"
        );
        assert_eq!(doc.agents[0].description, "new version");
        assert_eq!(doc.agents[0].state, "idle");
        assert_eq!(doc.agents[0].history.len(), 1);
        assert_eq!(doc.agents[1].state, "stopped");

        store.upsert_channel_meta("c1", ChannelMode::Serial, vec!["main".into()]);
        store.upsert_channel_meta("c1", ChannelMode::Free, vec!["main".into(), "a".into()]);
        store.append_channel_message(
            "c1",
            ChannelMessage {
                seq: 1,
                from: "a".into(),
                text: "t".into(),
                at: 0,
                kind: crate::channels::MessageKind::Said,
            },
        );
        let doc = store.snapshot();
        assert_eq!(
            doc.channels.len(),
            1,
            "same-name channel updates do not duplicate"
        );
        assert_eq!(doc.channels[0].mode, "free");
        assert_eq!(doc.channels[0].members, vec!["main", "a"]);
        assert_eq!(doc.channels[0].messages.len(), 1);
        // Messages do not create a channel before metadata lands (create-before-post is the caller's contract).
        store.append_channel_message(
            "ghost",
            ChannelMessage {
                seq: 1,
                from: "x".into(),
                text: "y".into(),
                at: 0,
                kind: crate::channels::MessageKind::Said,
            },
        );
        assert_eq!(store.snapshot().channels.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_is_atomic_no_tmp_leftover() {
        let root = temp_dir("atomic");
        let path = root.join("shares").join("bingo-1.json");
        let store = ShareStore::load_or_create(&path).unwrap_or_else(|e| panic!("{e}"));
        store.upsert_agent("a", None, "d".into(), AgentState::Running, vec![msg("m")]);
        store.save().unwrap_or_else(|e| panic!("{e}"));
        store.save().unwrap_or_else(|e| panic!("{e}"));
        assert!(path.exists());
        let tmp = path.with_extension("json.tmp");
        assert!(!tmp.exists(), "two saves leave no tmp leftovers");
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: ShareDoc = serde_json::from_str(&content).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parsed.agents.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn derive_share_doc_from_transcript_tools() {
        // Build a transcript containing Agent/SendMessage/AgentControl/Channel/Post,
        // and assert the legacy-session fallback derives Team/DM/channel data.
        let msgs = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "Agent".into(),
                    input: serde_json::json!({
                        "name": "scout",
                        "agent": "scout",
                        "description": "research",
                        "prompt": "go research it"
                    }),
                },
                ContentBlock::ToolUse {
                    id: "t2".into(),
                    name: "SendMessage".into(),
                    input: serde_json::json!({"agent": "scout", "message": "look at B again"}),
                },
                ContentBlock::ToolUse {
                    id: "t3".into(),
                    name: "Channel".into(),
                    input: serde_json::json!({
                        "action": "create",
                        "channel": "table",
                        "members": ["scout"],
                        "mode": "free"
                    }),
                },
                ContentBlock::ToolUse {
                    id: "t4".into(),
                    name: "Post".into(),
                    input: serde_json::json!({"channel": "table", "message": "hello everyone"}),
                },
                ContentBlock::ToolUse {
                    id: "t5".into(),
                    name: "AgentControl".into(),
                    input: serde_json::json!({"action": "stop", "agent": "scout"}),
                },
            ],
        }];
        let doc = derive_share_doc("proj-1700000000", &msgs);
        assert_eq!(doc.session, "proj-1700000000");
        // Agent entries: name/def/description/state.
        assert_eq!(doc.agents.len(), 1);
        assert_eq!(doc.agents[0].name, "scout");
        assert_eq!(doc.agents[0].def.as_deref(), Some("scout"));
        assert_eq!(doc.agents[0].description, "research");
        // SendMessage → appends a user message to history.
        assert_eq!(doc.agents[0].history.len(), 1);
        assert!(matches!(
            &doc.agents[0].history[0],
            Message {
                role: Role::User,
                ..
            }
        ));
        // AgentControl stop → stopped (after the send).
        assert_eq!(doc.agents[0].state, "stopped");
        // Channel create → metadata (main/user auto-join).
        assert_eq!(doc.channels.len(), 1);
        assert_eq!(doc.channels[0].name, "table");
        assert_eq!(doc.channels[0].mode, "free");
        assert_eq!(doc.channels[0].members, vec!["main", "user", "scout"]);
        // Post → a channel message (from=main, seq increments).
        assert_eq!(doc.channels[0].messages.len(), 1);
        assert_eq!(doc.channels[0].messages[0].seq, 1);
        assert_eq!(doc.channels[0].messages[0].from, "main");
        assert_eq!(doc.channels[0].messages[0].text, "hello everyone");
    }

    #[test]
    fn derive_share_doc_handles_duplicates_and_unknowns() {
        // Duplicate Agent spawns do not create duplicate entries; Post to unknown channels / SendMessage to unknown agents are silent.
        let msgs = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "Agent".into(),
                    input: serde_json::json!({"name": "w", "prompt": "do work"}),
                },
                ContentBlock::ToolUse {
                    id: "t2".into(),
                    name: "Agent".into(),
                    input: serde_json::json!({"name": "w", "prompt": "do more"}),
                },
                ContentBlock::ToolUse {
                    id: "t3".into(),
                    name: "SendMessage".into(),
                    input: serde_json::json!({"agent": "ghost", "message": "x"}),
                },
                ContentBlock::ToolUse {
                    id: "t4".into(),
                    name: "Post".into(),
                    input: serde_json::json!({"channel": "nope", "message": "y"}),
                },
            ],
        }];
        let doc = derive_share_doc("s", &msgs);
        assert_eq!(doc.agents.len(), 1, "same-name spawns do not duplicate");
        assert_eq!(doc.agents[0].name, "w");
        assert_eq!(
            doc.agents[0].description, "do work",
            "description falls back to a prompt excerpt"
        );
        assert!(
            doc.agents[0].history.is_empty(),
            "SendMessage to an unknown agent is silent"
        );
        assert!(
            doc.channels.is_empty(),
            "Post to an unknown channel is silent"
        );
        // A spawn without name/agent → auto-numbered agent-1.
        let msgs = vec![Message {
            role: Role::Assistant,
            content: vec![ContentBlock::ToolUse {
                id: "t1".into(),
                name: "Agent".into(),
                input: serde_json::json!({"prompt": "p"}),
            }],
        }];
        let doc = derive_share_doc("s", &msgs);
        assert_eq!(doc.agents[0].name, "agent-1");
    }

    #[test]
    fn share_id_format_has_ts_and_random_suffix() {
        let id = share_id("proj-1786092819");
        assert!(id.starts_with("1786092819"), "{id}");
        let suffix = &id[10..];
        assert_eq!(suffix.len(), 6, "{id}");
        assert!(
            suffix
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
            "suffix is [a-z0-9]: {id}"
        );
        // Two generations from the same stem differ (counter mixed in).
        assert_ne!(share_id("proj-1786092819"), share_id("proj-1786092819"));
    }

    /// Mock upload: a local TCP server receives the POST and asserts the request line / body / no-token-header
    /// as well as the returned link (the service is public).
    #[tokio::test]
    async fn upload_share_posts_html_without_token() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            use std::io::{BufRead, Read, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let mut headers = Vec::new();
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" {
                    break;
                }
                let trimmed = line.trim().to_string();
                if trimmed.to_ascii_lowercase().starts_with("content-length:") {
                    content_length = trimmed
                        .split_once(':')
                        .map(|(_, v)| v.trim().parse().unwrap_or(0))
                        .unwrap_or(0);
                }
                headers.push(trimmed);
            }
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).unwrap();
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            (request_line, headers, String::from_utf8(body).unwrap())
        });
        let base = format!("http://{addr}");
        let url = upload_share(&base, "abc123", "<html>hi</html>")
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(url, format!("{base}/share/u/abc123"));
        let (request_line, headers, body) = handle.join().unwrap();
        assert!(
            request_line.starts_with("POST /share/u/abc123 "),
            "{request_line}"
        );
        assert!(
            !headers
                .iter()
                .any(|h| h.to_ascii_lowercase().starts_with("x-share-token")),
            "public service must not require a token header: {headers:?}"
        );
        assert!(body.contains("<html>hi</html>"), "{body}");
    }

    /// Upload failure (HTTP 500) → Err.
    #[tokio::test]
    async fn upload_share_reports_server_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            use std::io::{BufRead, Write};
            let (mut stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let _ = stream
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n");
        });
        let base = format!("http://{addr}");
        let err = upload_share(&base, "abc123", "x").await.unwrap_err();
        assert!(err.to_string().contains("500"), "{err}");
        handle.join().unwrap();
    }

    #[test]
    fn resolve_transcript_matches_like_resume() {
        let root = temp_dir("resolve");
        let home = root.join("home");
        // create only makes the dir; the file lands on the first append (same convention as the transcript tests).
        let t_a = crate::transcript::create(&home, &root).unwrap_or_else(|e| panic!("{e}"));
        let _ = t_a.append(&msg("a"));
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let t_b = crate::transcript::create(&home, &root).unwrap_or_else(|e| panic!("{e}"));
        let _ = t_b.append(&msg("b"));
        // No key: take the newest (b was created later, so it is newer).
        let latest = resolve_transcript(&home, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(latest.name(), t_b.name());
        // Substring match (/resume semantics: list is newest-first by mtime; find takes the first hit).
        let hit = resolve_transcript(&home, Some(&t_a.name())).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(hit.name(), t_a.name());
        let fragment =
            resolve_transcript(&home, Some(&t_b.name()[..8])).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(fragment.name(), t_b.name());
        // A miss errors.
        assert!(matches!(
            resolve_transcript(&home, Some("nope")),
            Err(ShareError::SessionNotFound(_))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
}

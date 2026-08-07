//! share 数据源：会话的持久化快照（`bingo share` 的输入）。
//!
//! 会话运行时把子代理实例与频道日志增量写入 `ShareDoc`（JSON，单文件，
//! 原子写：tmp + rename），`bingo share` 读取该文档 + transcript 生成
//! 自包含 HTML 页面。share 是增强不是契约：存储失败只告警，不阻塞会话。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agents::AgentState;
use crate::api::types::Message;
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
}

impl ErrorCode for ShareError {
    fn error_code(&self) -> &'static str {
        match self {
            ShareError::Io(_)
            | ShareError::Json(_)
            | ShareError::Transcript(_)
            | ShareError::NoSessions
            | ShareError::SessionNotFound(_) => "STORAGE_ERROR",
        }
    }
}

/// 一个子代理实例的共享快照（history = 续话完整历史，即私聊视图数据）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentShare {
    pub name: String,
    /// 具名定义（AgentDef 名；无名实例 None）。
    pub def: Option<String>,
    pub description: String,
    /// 实例状态（AgentState 的字符串形态：running / idle / stopped）。
    pub state: String,
    pub history: Vec<Message>,
}

/// 一个频道的共享快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelShare {
    pub name: String,
    /// 发言模式（ChannelMode 的字符串形态：serial / free）。
    pub mode: String,
    pub members: Vec<String>,
    pub messages: Vec<ChannelMessage>,
}

/// 整个会话的共享文档（JSON 单文件）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareDoc {
    /// 会话 key（transcript 文件名 stem `{slug}-{ts}`）。
    pub session: String,
    /// 首次创建时间（unix 秒）。
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

/// shares 目录：~/.local/share/bingo/shares（与 transcripts 同级）。
pub fn shares_dir(home: &Path) -> PathBuf {
    home.join(".local").join("share").join("bingo").join("shares")
}

/// 会话级共享文档存储（Session 持有 Arc，子会话经 registry 共享）。
pub struct ShareStore {
    path: PathBuf,
    inner: Mutex<ShareDoc>,
}

impl ShareStore {
    /// 读取既有文档；不存在则按路径 stem 新建空文档。
    /// 损坏文件不阻塞会话：备份为 `<stem>.json.bak` 后从头开始。
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
        }))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ShareDoc> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 当前文档快照（bingo share 读取用）。
    pub fn snapshot(&self) -> ShareDoc {
        self.lock().clone()
    }

    /// 建/更新一个子代理实例条目（history 与 state 以最新为准）。
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

    /// 建/更新一个频道条目（模式与成员以最新为准；消息保留）。
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

    /// 追加一条频道消息（频道不存在时忽略——元数据先于消息落地）。
    pub fn append_channel_message(&self, name: &str, msg: ChannelMessage) {
        if let Some(c) = self.lock().channels.iter_mut().find(|c| c.name == name) {
            c.messages.push(msg);
        }
    }

    /// 原子写盘：tmp 文件 + rename（读侧要么看到旧文档要么看到新文档）。
    pub fn save(&self) -> Result<(), ShareError> {
        let json = serde_json::to_string_pretty(&*self.lock())?;
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }

    /// 写盘并吞掉错误（share 是增强不是契约：失败只告警一次）。
    pub fn persist(&self) {
        if let Err(e) = self.save() {
            eprintln!("[bingo] warning: share save failed: {e}");
        }
    }
}

/// 原子写文件（tmp + rename；share 输出统一入口，CLI 与 /share 共用）。
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

/// 用系统默认浏览器打开文件（macOS open / Linux xdg-open / Windows cmd start）。
pub fn open_in_browser(path: &Path) -> Result<(), ShareError> {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        c
    } else if cfg!(target_os = "linux") {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        c
    } else {
        let mut c = std::process::Command::new("cmd");
        c.arg("/c").arg("start").arg("").arg(path);
        c
    };
    cmd.spawn()?;
    Ok(())
}

/// 按会话 key 解析 transcript（/resume 语义：子串匹配，最新优先）；
/// 无 key 取最新会话。未命中时错误信息附可用会话列表（前 5 个，防刷屏）。
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
                            "{key}；可用会话：{}{suffix}",
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
            "调研".to_string(),
            AgentState::Idle,
            vec![msg("hi")],
        );
        store.upsert_channel_meta("table", ChannelMode::Free, vec!["main".into(), "user".into(), "scout".into()]);
        store.append_channel_message(
            "table",
            ChannelMessage { seq: 1, from: "scout".into(), text: "大家好".into() },
        );
        store
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
        assert_eq!(doc.channels[0].messages[0].text, "大家好");
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
        assert!(!path.exists(), "仅读不落盘");
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
        assert!(store.snapshot().agents.is_empty(), "损坏文件从头开始");
        assert!(path.with_extension("json.bak").exists(), "损坏文件备份保留");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn upsert_updates_in_place() {
        let root = temp_dir("upsert");
        let path = root.join("shares").join("bingo-1.json");
        let store = ShareStore::load_or_create(&path).unwrap_or_else(|e| panic!("{e}"));
        store.upsert_agent("a", None, "初版".into(), AgentState::Running, Vec::new());
        store.upsert_agent("a", None, "新版".into(), AgentState::Idle, vec![msg("x")]);
        store.upsert_agent("b", None, "另一个人".into(), AgentState::Stopped, Vec::new());
        let doc = store.snapshot();
        assert_eq!(doc.agents.len(), 2, "同名更新不重复建条目");
        assert_eq!(doc.agents[0].description, "新版");
        assert_eq!(doc.agents[0].state, "idle");
        assert_eq!(doc.agents[0].history.len(), 1);
        assert_eq!(doc.agents[1].state, "stopped");

        store.upsert_channel_meta("c1", ChannelMode::Serial, vec!["main".into()]);
        store.upsert_channel_meta("c1", ChannelMode::Free, vec!["main".into(), "a".into()]);
        store.append_channel_message("c1", ChannelMessage { seq: 1, from: "a".into(), text: "t".into() });
        let doc = store.snapshot();
        assert_eq!(doc.channels.len(), 1, "同名频道更新不重复");
        assert_eq!(doc.channels[0].mode, "free");
        assert_eq!(doc.channels[0].members, vec!["main", "a"]);
        assert_eq!(doc.channels[0].messages.len(), 1);
        // 元数据未落地前消息不建频道（create 先于 post 是调用方契约）。
        store.append_channel_message("ghost", ChannelMessage { seq: 1, from: "x".into(), text: "y".into() });
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
        assert!(!tmp.exists(), "两次保存不留 tmp 残留");
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: ShareDoc = serde_json::from_str(&content).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parsed.agents.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn resolve_transcript_matches_like_resume() {
        let root = temp_dir("resolve");
        let home = root.join("home");
        // create 只建目录，文件首条 append 才落盘（与 transcript 测试同约定）。
        let t_a = crate::transcript::create(&home, &root).unwrap_or_else(|e| panic!("{e}"));
        let _ = t_a.append(&msg("a"));
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let t_b = crate::transcript::create(&home, &root).unwrap_or_else(|e| panic!("{e}"));
        let _ = t_b.append(&msg("b"));
        // 无 key：取最新（b 后建，较新）。
        let latest = resolve_transcript(&home, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(latest.name(), t_b.name());
        // 子串匹配（/resume 同语义：list 按 mtime 新→旧，find 取第一个命中）。
        let hit = resolve_transcript(&home, Some(&t_a.name()))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(hit.name(), t_a.name());
        let fragment = resolve_transcript(&home, Some(&t_b.name()[..8]))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(fragment.name(), t_b.name());
        // 未命中报错。
        assert!(matches!(
            resolve_transcript(&home, Some("nope")),
            Err(ShareError::SessionNotFound(_))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
}

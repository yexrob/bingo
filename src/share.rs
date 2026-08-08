//! share 数据源：会话的持久化快照（`bingo share` 的输入）。
//!
//! 会话运行时把子代理实例与频道日志增量写入 `ShareDoc`（JSON，单文件，
//! 原子写：tmp + rename），`bingo share` 读取该文档 + transcript 生成
//! 自包含 HTML 页面。share 是增强不是契约：存储失败只告警，不阻塞会话。

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
    home.join(".local")
        .join("share")
        .join("bingo")
        .join("shares")
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

/// 官网上传服务缺省基址（settings.share.baseUrl 可覆盖）。
pub const DEFAULT_SHARE_BASE: &str = "https://bingo.ruobin.dev";

/// 分享 id：会话 stem 的 ts 部分 + 6 位随机 [a-z0-9]（无 rand 依赖，
/// splitmix64 混合时间与计数器）。
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

/// 上传 HTML 到官网分享服务（公开，无需 token）：
/// POST `{base}/share/u/{id}`，body = HTML。
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

/// 用系统默认浏览器打开目标（文件路径或 URL；macOS open / Linux xdg-open / Windows cmd start）。
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

/// 从主 transcript 消息推导 agents/channels（旧会话回退：进程启动于 share
/// 功能合入前、无 share 文档时，导出仍含 Team/DM/频道视图，而非空态）。
///
/// 推导规则（尽力而为，发送者身份从 transcript 不可精确还原）：
/// - `Agent` tool_use → AgentShare 条目（name=实例名或 agent 定义名，
///   description 取 description 或 prompt 摘要，state=idle，history 空）
/// - `SendMessage` → 向该 agent 的 history 追加 user 消息
/// - `AgentControl stop/delete` → state=stopped
/// - `Channel create` → ChannelShare 元数据（members 含 main/user）
/// - `Post` → 频道消息（from=main，seq 递增）
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
                        });
                    }
                }
                _ => {}
            }
        }
    }
    doc
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
                text: "大家好".into(),
                at: 0,
            },
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
        store.upsert_agent(
            "b",
            None,
            "另一个人".into(),
            AgentState::Stopped,
            Vec::new(),
        );
        let doc = store.snapshot();
        assert_eq!(doc.agents.len(), 2, "同名更新不重复建条目");
        assert_eq!(doc.agents[0].description, "新版");
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
            },
        );
        let doc = store.snapshot();
        assert_eq!(doc.channels.len(), 1, "同名频道更新不重复");
        assert_eq!(doc.channels[0].mode, "free");
        assert_eq!(doc.channels[0].members, vec!["main", "a"]);
        assert_eq!(doc.channels[0].messages.len(), 1);
        // 元数据未落地前消息不建频道（create 先于 post 是调用方契约）。
        store.append_channel_message(
            "ghost",
            ChannelMessage {
                seq: 1,
                from: "x".into(),
                text: "y".into(),
                at: 0,
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
        assert!(!tmp.exists(), "两次保存不留 tmp 残留");
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: ShareDoc = serde_json::from_str(&content).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(parsed.agents.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn derive_share_doc_from_transcript_tools() {
        // 构造含 Agent/SendMessage/AgentControl/Channel/Post 的 transcript，
        // 断言旧会话回退推导出 Team/DM/频道数据。
        let msgs = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "Agent".into(),
                    input: serde_json::json!({
                        "name": "scout",
                        "agent": "scout",
                        "description": "调研",
                        "prompt": "去调研一下"
                    }),
                },
                ContentBlock::ToolUse {
                    id: "t2".into(),
                    name: "SendMessage".into(),
                    input: serde_json::json!({"agent": "scout", "message": "再看 B"}),
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
                    input: serde_json::json!({"channel": "table", "message": "大家好"}),
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
        // Agent 条目：name/def/description/state。
        assert_eq!(doc.agents.len(), 1);
        assert_eq!(doc.agents[0].name, "scout");
        assert_eq!(doc.agents[0].def.as_deref(), Some("scout"));
        assert_eq!(doc.agents[0].description, "调研");
        // SendMessage → history 追加 user 消息。
        assert_eq!(doc.agents[0].history.len(), 1);
        assert!(matches!(
            &doc.agents[0].history[0],
            Message {
                role: Role::User,
                ..
            }
        ));
        // AgentControl stop → stopped（send 之后）。
        assert_eq!(doc.agents[0].state, "stopped");
        // Channel create → 元数据（main/user 自动入席）。
        assert_eq!(doc.channels.len(), 1);
        assert_eq!(doc.channels[0].name, "table");
        assert_eq!(doc.channels[0].mode, "free");
        assert_eq!(doc.channels[0].members, vec!["main", "user", "scout"]);
        // Post → 频道消息（from=main，seq 递增）。
        assert_eq!(doc.channels[0].messages.len(), 1);
        assert_eq!(doc.channels[0].messages[0].seq, 1);
        assert_eq!(doc.channels[0].messages[0].from, "main");
        assert_eq!(doc.channels[0].messages[0].text, "大家好");
    }

    #[test]
    fn derive_share_doc_handles_duplicates_and_unknowns() {
        // 重复 Agent 派生不重复建条目；Post 到未知频道/未知 agent 静默。
        let msgs = vec![Message {
            role: Role::Assistant,
            content: vec![
                ContentBlock::ToolUse {
                    id: "t1".into(),
                    name: "Agent".into(),
                    input: serde_json::json!({"name": "w", "prompt": "干活"}),
                },
                ContentBlock::ToolUse {
                    id: "t2".into(),
                    name: "Agent".into(),
                    input: serde_json::json!({"name": "w", "prompt": "再干"}),
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
        assert_eq!(doc.agents.len(), 1, "重名派生不重复");
        assert_eq!(doc.agents[0].name, "w");
        assert_eq!(
            doc.agents[0].description, "干活",
            "description 回落 prompt 摘要"
        );
        assert!(
            doc.agents[0].history.is_empty(),
            "未知 agent 的 SendMessage 静默"
        );
        assert!(doc.channels.is_empty(), "未知频道的 Post 静默");
        // 无 name/agent 的派生 → 自动编号 agent-1。
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
            "后缀 [a-z0-9]: {id}"
        );
        // 同 stem 两次生成不同（计数器混合）。
        assert_ne!(share_id("proj-1786092819"), share_id("proj-1786092819"));
    }

    /// mock 上传：本地 TCP 服务器接收 POST，断言请求行/body/无 token 头
    /// 与返回的链接（服务公开）。
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
            "公开服务无 token 头: {headers:?}"
        );
        assert!(body.contains("<html>hi</html>"), "{body}");
    }

    /// 上传失败（HTTP 500）→ Err。
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
        let hit = resolve_transcript(&home, Some(&t_a.name())).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(hit.name(), t_a.name());
        let fragment =
            resolve_transcript(&home, Some(&t_b.name()[..8])).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(fragment.name(), t_b.name());
        // 未命中报错。
        assert!(matches!(
            resolve_transcript(&home, Some("nope")),
            Err(ShareError::SessionNotFound(_))
        ));
        let _ = std::fs::remove_dir_all(&root);
    }
}

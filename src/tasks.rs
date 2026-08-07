use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use std::fmt;
use tokio::sync::Mutex;

use crate::error::ErrorCode;

/// Task 状态机：pending → in_progress → completed，deleted 为删除。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Pending,
    InProgress,
    Completed,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TaskStatus::Pending => "pending",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Completed => "completed",
        };
        f.write_str(s)
    }
}

/// 单个任务（磁盘每任务一 JSON 文件）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub subject: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    #[serde(default)]
    pub status: TaskStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default)]
    pub blocks: Vec<String>,
    #[serde(default)]
    pub blocked_by: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl Task {
    /// 内部任务标记：TaskList 隐藏（metadata._internal）。
    pub fn is_internal(&self) -> bool {
        self.metadata
            .get("_internal")
            .is_some_and(|v| v.as_bool().unwrap_or(false))
    }
}

/// 任务存储：`~/.local/share/bingo/tasks/<list_id>/<id>.json`。
/// 磁盘持久化 + 进程内互斥（bingo 单进程，无跨进程并发）。
pub struct TaskStore {
    dir: PathBuf,
    lock: Mutex<()>,
}

/// 项目级列表键（旧行为：同一工作目录跨会话共享任务；会话键的回落）。
pub fn project_task_key(cwd: &Path) -> String {
    cwd.canonicalize()
        .unwrap_or_else(|_| cwd.to_path_buf())
        .to_string_lossy()
        .replace(['/', '\\'], "_")
}

fn list_id_from_env_or(key: &str) -> String {
    if let Ok(id) = std::env::var("BINGO_TASK_LIST_ID")
        && !id.is_empty()
    {
        return id;
    }
    // 会话级列表：每个 session 独立 todo（key = transcript 文件 stem，
    // --continue 恢复同一会话）。进程内同一 key 即同一列表。
    key.to_string()
}

impl TaskStore {
    pub fn new(home: &Path, key: &str) -> Self {
        let dir = home
            .join(".local")
            .join("share")
            .join("bingo")
            .join("tasks")
            .join(list_id_from_env_or(key));
        Self {
            dir,
            lock: Mutex::new(()),
        }
    }

    /// TUI/查询共享同一 store 实例（Arc<TaskStore>）。
    fn task_path(&self, id: &str) -> Result<PathBuf, TaskError> {
        // id 为内部生成的数字串；防御路径穿越。
        if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(TaskError::InvalidId(id.to_string()));
        }
        Ok(self.dir.join(format!("{id}.json")))
    }

    fn read_file(path: &Path) -> Result<Option<Task>, TaskError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(TaskError::Io(e)),
        };
        // 解析失败曾吞成 Ok(None)：任务在列表里静默消失，用户无从知晓。
        serde_json::from_str(&raw).map(Some).map_err(|e| TaskError::Parse {
            path: path.display().to_string(),
            detail: e.to_string(),
        })
    }

    /// 原子写：先写临时文件再 rename，半写文件不会被读到。
    /// `.json.tmp` 不以 `.json` 结尾，不会被 list_ids 收进任务列表。
    fn write_file(path: &Path, task: &Task) -> Result<(), TaskError> {
        let body = serde_json::to_string_pretty(task).map_err(TaskError::Serialize)?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, body).map_err(TaskError::Io)?;
        match std::fs::rename(&tmp, path) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(TaskError::Io(e))
            }
        }
    }

    /// 锁内创建：id = max(现有 max, 0) + 1；ifAbsent 语义（已存在则报错）。
    pub async fn create(
        &self,
        task: &Task,
    ) -> Result<String, TaskError> {
        let _guard = self.lock.lock().await;
        std::fs::create_dir_all(&self.dir).map_err(TaskError::Io)?;
        let ids = self.list_ids_unlocked()?;
        let max = ids.iter().filter_map(|i| i.parse::<u64>().ok()).max().unwrap_or(0);
        let id = (max + 1).to_string();
        let mut t = task.clone();
        t.id = id.clone();
        let path = self.task_path(&id)?;
        if path.exists() {
            return Err(TaskError::CreateConflict(id));
        }
        Self::write_file(&path, &t)?;
        Ok(id)
    }

    pub async fn get(&self, id: &str) -> Result<Option<Task>, TaskError> {
        let _guard = self.lock.lock().await;
        let path = self.task_path(id)?;
        Self::read_file(&path)
    }

    /// patch 更新：仅写给出的字段（增量语义）。返回旧值。
    pub async fn update(&self, id: &str, patch: &TaskPatch) -> Result<Option<Task>, TaskError> {
        let _guard = self.lock.lock().await;
        let path = self.task_path(id)?;
        let Some(mut t) = Self::read_file(&path)? else {
            return Ok(None);
        };
        // 旧值必须在 patch 之前克隆：之后再克隆得到的是新值，
        // statusChange 事件会退化成 from == to。
        let old = t.clone();
        if let Some(subject) = &patch.subject {
            t.subject = subject.clone();
        }
        if let Some(description) = &patch.description {
            t.description = description.clone();
        }
        if let Some(active_form) = &patch.active_form {
            t.active_form = Some(active_form.clone());
        }
        if let Some(owner) = &patch.owner {
            t.owner = Some(owner.clone());
        }
        if let Some(status) = patch.status {
            t.status = status;
        }
        if let Some(metadata) = &patch.metadata {
            for (k, v) in metadata {
                if v.is_null() {
                    t.metadata.remove(k);
                } else {
                    t.metadata.insert(k.clone(), v.clone());
                }
            }
        }
        Self::write_file(&path, &t)?;
        Ok(Some(old))
    }

    /// 删除 + 清理其他任务的 blocks/blockedBy 引用。
    pub async fn delete(&self, id: &str) -> Result<bool, TaskError> {
        let _guard = self.lock.lock().await;
        let path = self.task_path(id)?;
        if !path.exists() {
            return Ok(false);
        }
        std::fs::remove_file(&path).map_err(TaskError::Io)?;
        for tid in self.list_ids_unlocked()? {
            if tid == id {
                continue;
            }
            let tpath = self.task_path(&tid)?;
            if let Some(t) = Self::read_file(&tpath)? {
                let blocks: Vec<String> = t.blocks.iter().filter(|b| *b != id).cloned().collect();
                let blocked_by: Vec<String> =
                    t.blocked_by.iter().filter(|b| *b != id).cloned().collect();
                if blocks != t.blocks || blocked_by != t.blocked_by {
                    let mut t2 = t;
                    t2.blocks = blocks;
                    t2.blocked_by = blocked_by;
                    Self::write_file(&tpath, &t2)?;
                }
            }
        }
        Ok(true)
    }

    /// 双向建立依赖：a.blocks += b, b.blocked_by += a。
    pub async fn link_block(&self, a: &str, b: &str) -> Result<bool, TaskError> {
        let _guard = self.lock.lock().await;
        let (pa, pb) = (self.task_path(a)?, self.task_path(b)?);
        let (Some(mut ta), Some(mut tb)) = (Self::read_file(&pa)?, Self::read_file(&pb)?) else {
            return Ok(false);
        };
        let mut changed = false;
        if !ta.blocks.contains(&b.to_string()) {
            ta.blocks.push(b.to_string());
            changed = true;
        }
        if !tb.blocked_by.contains(&a.to_string()) {
            tb.blocked_by.push(a.to_string());
            changed = true;
        }
        if changed {
            Self::write_file(&pa, &ta)?;
            Self::write_file(&pb, &tb)?;
        }
        Ok(changed)
    }

    fn list_ids_unlocked(&self) -> Result<Vec<String>, TaskError> {
        let mut ids = Vec::new();
        if !self.dir.exists() {
            return Ok(ids);
        }
        for entry in std::fs::read_dir(&self.dir).map_err(TaskError::Io)? {
            let entry = entry.map_err(TaskError::Io)?;
            let name = entry.file_name();
            let name = name.to_string_lossy().to_string();
            if let Some(stripped) = name.strip_suffix(".json") {
                ids.push(stripped.to_string());
            }
        }
        Ok(ids)
    }

    /// 全量列表：数字 id 排序；隐藏 _internal。
    pub async fn list(&self) -> Result<Vec<Task>, TaskError> {
        let _guard = self.lock.lock().await;
        let mut tasks = Vec::new();
        for id in self.list_ids_unlocked()? {
            if let Some(t) = Self::read_file(&self.task_path(&id)?)?
                && !t.is_internal()
            {
                tasks.push(t);
            }
        }
        tasks.sort_by_key(|t| t.id.parse::<u64>().unwrap_or(u64::MAX));
        Ok(tasks)
    }

    /// TUI 同步快照（每帧调用；与工具写并发时半写文件被跳过，容错）。
    pub fn list_ui(&self) -> Vec<Task> {
        let Ok(ids) = (|| -> Result<Vec<String>, TaskError> {
            let mut ids = Vec::new();
            if !self.dir.exists() {
                return Ok(ids);
            }
            for entry in std::fs::read_dir(&self.dir).map_err(TaskError::Io)? {
                let name = entry.map_err(TaskError::Io)?.file_name();
                let name = name.to_string_lossy().to_string();
                if let Some(stripped) = name.strip_suffix(".json") {
                    ids.push(stripped.to_string());
                }
            }
            Ok(ids)
        })() else {
            return Vec::new();
        };
        let mut tasks: Vec<Task> = ids
            .into_iter()
            .filter_map(|id| self.task_path(&id).ok().and_then(|p| Self::read_file(&p).ok().flatten()))
            .filter(|t| !t.is_internal())
            .collect();
        tasks.sort_by_key(|t| t.id.parse::<u64>().unwrap_or(u64::MAX));
        tasks
    }
}

/// 增量 patch（可选字段）。
#[derive(Debug, Default)]
pub struct TaskPatch {
    pub subject: Option<String>,
    pub description: Option<String>,
    pub active_form: Option<String>,
    pub owner: Option<String>,
    pub status: Option<TaskStatus>,
    pub metadata: Option<BTreeMap<String, serde_json::Value>>,
}

#[derive(Debug, thiserror::Error)]
pub enum TaskError {
    #[error("task store io: {0}")]
    Io(std::io::Error),
    #[error("task id invalid: {0}")]
    InvalidId(String),
    #[error("task serialize: {0}")]
    Serialize(serde_json::Error),
    #[error("task {0} already exists")]
    CreateConflict(String),
    #[error("task file {path} is not valid task JSON: {detail}")]
    Parse { path: String, detail: String },
}

impl ErrorCode for TaskError {
    fn error_code(&self) -> &'static str {
        match self {
            TaskError::Io(_)
            | TaskError::InvalidId(_)
            | TaskError::Serialize(_)
            | TaskError::CreateConflict(_)
            | TaskError::Parse { .. } => "STORAGE_ERROR",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn store_in(tmp: &Path) -> TaskStore {
        TaskStore::new(&tmp.join("home"), "test-session")
    }

    fn task(subject: &str) -> Task {
        Task {
            id: String::new(),
            subject: subject.to_string(),
            description: String::new(),
            active_form: None,
            status: TaskStatus::Pending,
            owner: None,
            blocks: Vec::new(),
            blocked_by: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn sessions_are_isolated_by_key() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tmp = env::temp_dir().join(format!("bingo-tasks-session-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            let s1 = TaskStore::new(&tmp.join("home"), "proj-1700000000");
            let s2 = TaskStore::new(&tmp.join("home"), "proj-1700000001");
            s1.create(&task("first")).await.unwrap();
            assert_eq!(s1.list().await.unwrap().len(), 1);
            assert!(
                s2.list().await.unwrap().is_empty(),
                "新会话列表独立，不继承上一会话 todo"
            );
            // --continue 同一会话：同 key 恢复同一列表。
            let s1b = TaskStore::new(&tmp.join("home"), "proj-1700000000");
            assert_eq!(s1b.list().await.unwrap().len(), 1, "同会话恢复同 todo");
            let _ = std::fs::remove_dir_all(&tmp);
        });
    }

    #[test]
    fn create_get_update_list_delete() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tmp = env::temp_dir().join(format!("bingo-tasks-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            let store = store_in(&tmp);

            let id1 = store.create(&task("first")).await.unwrap();
            let id2 = store.create(&task("second")).await.unwrap();
            assert_eq!(id1, "1");
            assert_eq!(id2, "2");

            let t = store.get("1").await.unwrap().unwrap();
            assert_eq!(t.subject, "first");
            assert_eq!(t.status, TaskStatus::Pending);

            store
                .update(
                    "1",
                    &TaskPatch {
                        status: Some(TaskStatus::InProgress),
                        active_form: Some("Running first".into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let t = store.get("1").await.unwrap().unwrap();
            assert_eq!(t.status, TaskStatus::InProgress);
            assert_eq!(t.active_form.as_deref(), Some("Running first"));

            let list = store.list().await.unwrap();
            assert_eq!(list.len(), 2);
            assert_eq!(list[0].id, "1");
            assert_eq!(list[1].id, "2");

            assert!(store.delete("1").await.unwrap());
            assert!(store.get("1").await.unwrap().is_none());
            assert!(!store.delete("1").await.unwrap());
            let _ = std::fs::remove_dir_all(&tmp);
        });
    }

    #[test]
    fn delete_cleans_references() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tmp = env::temp_dir().join(format!("bingo-tasks-ref-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            let store = store_in(&tmp);
            let a = store.create(&task("a")).await.unwrap();
            let b = store.create(&task("b")).await.unwrap();
            store.link_block(&a, &b).await.unwrap();
            let ta = store.get(&a).await.unwrap().unwrap();
            assert_eq!(ta.blocks, vec![b.clone()]);

            store.delete(&b).await.unwrap();
            let ta = store.get(&a).await.unwrap().unwrap();
            assert!(ta.blocks.is_empty(), "deleted task removed from blocks");
            let _ = std::fs::remove_dir_all(&tmp);
        });
    }

    #[test]
    fn internal_tasks_hidden_from_list() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tmp = env::temp_dir().join(format!("bingo-tasks-int-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            let store = store_in(&tmp);
            let mut hidden = task("hidden");
            hidden.metadata.insert("_internal".into(), serde_json::json!(true));
            store.create(&hidden).await.unwrap();
            store.create(&task("visible")).await.unwrap();
            let list = store.list().await.unwrap();
            assert_eq!(list.len(), 1);
            assert_eq!(list[0].subject, "visible");
            let _ = std::fs::remove_dir_all(&tmp);
        });
    }

    /// L1 回归：update 返回的旧值曾在 patch 之后克隆 → statusChange from == to。
    #[test]
    fn update_returns_pre_patch_snapshot() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tmp = env::temp_dir().join(format!("bingo-tasks-old-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            let store = store_in(&tmp);
            let id = store.create(&task("first")).await.unwrap();
            let old = store
                .update(
                    &id,
                    &TaskPatch {
                        status: Some(TaskStatus::InProgress),
                        subject: Some("renamed".into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap()
                .unwrap();
            assert_eq!(old.status, TaskStatus::Pending, "旧值是 patch 之前的状态");
            assert_eq!(old.subject, "first", "旧值是 patch 之前的标题");
            let now = store.get(&id).await.unwrap().unwrap();
            assert_eq!(now.status, TaskStatus::InProgress);
            assert_eq!(now.subject, "renamed");
            let _ = std::fs::remove_dir_all(&tmp);
        });
    }

    /// L5 回归：损坏的任务文件报错而非静默消失；写入走 tmp + rename。
    #[test]
    fn corrupt_task_file_reports_error_and_writes_are_atomic() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tmp = env::temp_dir().join(format!("bingo-tasks-corrupt-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&tmp);
            let store = store_in(&tmp);
            let id = store.create(&task("first")).await.unwrap();
            // 写入不留临时文件残骸。
            let leftovers = std::fs::read_dir(&store.dir)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
                .count();
            assert_eq!(leftovers, 0, "rename 后无 .tmp 残留");

            std::fs::write(store.dir.join(format!("{id}.json")), "{ not json").unwrap();
            assert!(
                matches!(store.get(&id).await, Err(TaskError::Parse { .. })),
                "解析失败必须报错"
            );
            assert!(store.list().await.is_err(), "列表也不吞错");
            let _ = std::fs::remove_dir_all(&tmp);
        });
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tmp = env::temp_dir().join(format!("bingo-tasks-traversal-{}", std::process::id()));
            let store = store_in(&tmp);
            let r = store.get("..%2fetc").await;
            assert!(r.is_err());
            let _ = std::fs::remove_dir_all(&tmp);
        });
    }
}

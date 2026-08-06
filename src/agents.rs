//! 具名 agent 定义与子代理实例注册表（D29）。
//!
//! 定义（AgentDef）：磁盘上的人格模板——frontmatter 元数据 + 正文 system
//! prompt，镜像 skills 的目录约定。实例（AgentRegistry 条目）：一次 spawn
//! 出来的活会话——持有子 Session 与完整消息历史，主 agent 经 SendMessage
//! 续话（hub-and-spoke：只有主会话有管理工具）。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::api::types::Message;
use crate::query::Session;

/// 一个具名 agent 定义：`<name>.md`（YAML frontmatter + 正文 system prompt）。
#[derive(Debug, Clone)]
pub struct AgentDef {
    pub name: String,
    /// 清单描述（模型据此选人）。
    pub description: String,
    /// 缺省模型（实例参数 > 定义 > 继承父会话）。
    pub model: Option<String>,
    /// 缺省 provider（同上优先级）。
    pub provider: Option<String>,
    /// 正文 = 子代理的 system prompt（替换父会话 system；空则继承）。
    pub system: String,
}

/// 用户级定义目录：`$XDG_CONFIG_HOME/bingo/agents`（镜像 skills 约定）。
fn user_agents_dir(home: &Path) -> PathBuf {
    let config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".config"));
    config.join("bingo").join("agents")
}

/// 从 cwd 向上逐层找 `.bingo/agents`。
fn project_agents_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        dirs.push(d.join(".bingo").join("agents"));
        dir = d.parent();
    }
    dirs
}

fn load_dir(dir: &Path, out: &mut Vec<AgentDef>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();
    for path in files {
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (pairs, body) = crate::skills::parse_frontmatter_pairs(&raw);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let mut def = AgentDef {
            name: stem,
            description: String::new(),
            model: None,
            provider: None,
            system: body.trim_end().to_string(),
        };
        for (key, value) in pairs {
            match key.as_str() {
                "name" => def.name = value,
                "description" => def.description = value,
                "model" => def.model = Some(value),
                "provider" => def.provider = Some(value),
                _ => {}
            }
        }
        if def.description.is_empty() {
            def.description = crate::skills::first_line(&def.system);
        }
        if !def.name.is_empty() {
            out.push(def);
        }
    }
}

/// 加载全部定义：项目层（近 cwd 优先）→ user 层，同名 first-wins
/// （项目覆盖用户）。定义通常个位数，不做 mtime 缓存。
pub fn load_agent_defs(home: &Path, cwd: &Path) -> Vec<AgentDef> {
    let mut defs = Vec::new();
    for dir in project_agents_dirs(cwd) {
        load_dir(&dir, &mut defs);
    }
    load_dir(&user_agents_dir(home), &mut defs);
    let mut seen = std::collections::HashSet::new();
    defs.retain(|d| seen.insert(d.name.clone()));
    defs
}

/// 实例生命周期。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// 回合进行中（新消息排队，回合结束自动送达）。
    Running,
    /// 等待指令（SendMessage 立即唤醒，历史保留）。
    Idle,
    /// 已停止（不再接收；delete 后名字释放）。
    Stopped,
}

impl AgentState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Idle => "idle",
            Self::Stopped => "stopped",
        }
    }
}

/// list 快照。
#[derive(Debug, Clone)]
pub struct AgentStatus {
    pub name: String,
    pub def: Option<String>,
    pub description: String,
    pub state: AgentState,
    pub pending: usize,
}

/// SendMessage 的投递结果。
pub enum Delivery {
    /// 实例忙：已排队，回合结束自动送达。
    Queued,
    /// 实例空闲：以历史副本立即开新回合（排队旧指令一并并入）。
    Start {
        session: Arc<Session>,
        history: Vec<Message>,
        prompt: String,
    },
}

struct Entry {
    def: Option<String>,
    description: String,
    state: AgentState,
    /// 最近一次完成回合后的完整消息历史（续话上下文）。
    history: Vec<Message>,
    /// 忙碌期间排队的指令。
    pending: Vec<String>,
    session: Arc<Session>,
    abort: Option<tokio::task::AbortHandle>,
    /// 累计回合数（watch 行标注 `#N`）。
    runs: u64,
    /// 当前回合的 watch 行（stop/delete 置 Cancelled 用）。
    watch_id: Option<crate::watch::WatchId>,
}

/// 会话级实例注册表（Session 持有 Arc，子会话共享）。
pub struct AgentRegistry {
    inner: Mutex<HashMap<String, Entry>>,
}

/// 多条排队指令并成一个提示（保持送达顺序）。
fn join_pending(msgs: &[String]) -> String {
    if msgs.len() == 1 {
        msgs[0].clone()
    } else {
        let mut out = String::from("收到多条追加指令（按序）：");
        for m in msgs {
            out.push_str("\n- ");
            out.push_str(m);
        }
        out
    }
}

impl AgentRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 认领实例名：空闲直接用，被占用追加 `-2`/`-3`…（并行同名可区分）。
    pub fn claim_name(&self, base: &str) -> String {
        let base = if base.trim().is_empty() { "agent" } else { base.trim() };
        let inner = self.lock();
        if !inner.contains_key(base) {
            return base.to_string();
        }
        let mut n = 2;
        loop {
            let candidate = format!("{base}-{n}");
            if !inner.contains_key(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// 登记新实例（state=Running）。名字须先经 claim_name。
    pub fn insert(
        &self,
        name: &str,
        def: Option<String>,
        description: String,
        session: Arc<Session>,
    ) {
        self.lock().insert(
            name.to_string(),
            Entry {
                def,
                description,
                state: AgentState::Running,
                history: Vec::new(),
                pending: Vec::new(),
                session,
                abort: None,
                runs: 0,
                watch_id: None,
            },
        );
    }

    pub fn set_abort(&self, name: &str, abort: tokio::task::AbortHandle) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.abort = Some(abort);
        }
    }

    /// 下一回合序号（1 起）。
    pub fn next_run(&self, name: &str) -> u64 {
        match self.lock().get_mut(name) {
            Some(entry) => {
                entry.runs += 1;
                entry.runs
            }
            None => 1,
        }
    }

    /// 记录当前回合的 watch 行。
    pub fn set_run_watch(&self, name: &str, id: crate::watch::WatchId) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.watch_id = Some(id);
        }
    }

    /// 回合完成：存入最新历史。有排队指令 → 保持 Running 并返回
    /// (历史副本, 并入的下一提示)；无 → 转 Idle。
    /// Stopped（回合中被停止）不复活、不返回续跑。
    pub fn finish(
        &self,
        name: &str,
        history: Vec<Message>,
    ) -> Option<(Vec<Message>, String)> {
        let mut inner = self.lock();
        let entry = inner.get_mut(name)?;
        entry.history = history;
        if entry.state == AgentState::Stopped {
            return None;
        }
        if entry.pending.is_empty() {
            entry.state = AgentState::Idle;
            return None;
        }
        let prompt = join_pending(&entry.pending);
        entry.pending.clear();
        entry.state = AgentState::Running;
        Some((entry.history.clone(), prompt))
    }

    /// 回合失败：保留失败前历史，转 Idle（可经 SendMessage 重试）。
    pub fn mark_idle(&self, name: &str) {
        if let Some(entry) = self.lock().get_mut(name)
            && entry.state != AgentState::Stopped
        {
            entry.state = AgentState::Idle;
        }
    }

    /// 投递消息：Running 排队；Idle 唤醒（返回续跑所需的会话与历史，
    /// 旧排队指令一并并入）；Stopped/未知报错。
    pub fn deliver(&self, name: &str, message: &str) -> Result<Delivery, String> {
        let mut inner = self.lock();
        let Some(entry) = inner.get_mut(name) else {
            let known: Vec<String> = inner.keys().cloned().collect();
            return Err(if known.is_empty() {
                format!("没有名为 {name} 的子代理（当前没有任何实例）")
            } else {
                format!(
                    "没有名为 {name} 的子代理；现有实例：{}",
                    known.join(", ")
                )
            });
        };
        match entry.state {
            AgentState::Running => {
                entry.pending.push(message.to_string());
                Ok(Delivery::Queued)
            }
            AgentState::Idle => {
                entry.pending.push(message.to_string());
                let prompt = join_pending(&entry.pending);
                entry.pending.clear();
                entry.state = AgentState::Running;
                Ok(Delivery::Start {
                    session: entry.session.clone(),
                    history: entry.history.clone(),
                    prompt,
                })
            }
            AgentState::Stopped => Err(format!(
                "{name} 已停止，不再接收指令（delete 可移除该实例）"
            )),
        }
    }

    /// 停止：中止运行中的回合（abort），不再接收指令；历史保留可 list。
    /// 返回被中止回合的 watch 行（调用方置 Cancelled）；
    /// 空闲/已停止时无进行中的行，返回 None（幂等）。
    pub fn stop(&self, name: &str) -> Result<Option<crate::watch::WatchId>, String> {
        let mut inner = self.lock();
        let Some(entry) = inner.get_mut(name) else {
            return Err(format!("没有名为 {name} 的子代理"));
        };
        if entry.state == AgentState::Stopped {
            return Ok(None);
        }
        let was_running = entry.state == AgentState::Running;
        entry.state = AgentState::Stopped;
        entry.pending.clear();
        if let Some(abort) = entry.abort.take() {
            abort.abort();
        }
        Ok(if was_running { entry.watch_id } else { None })
    }

    /// 删除：先停止，再移除条目（名字释放）。返回被中止回合的 watch 行。
    pub fn remove(&self, name: &str) -> Result<Option<crate::watch::WatchId>, String> {
        let id = self.stop(name)?;
        self.lock().remove(name);
        Ok(id)
    }

    /// 全部实例快照（按名字排序，list 输出稳定）。
    pub fn list(&self) -> Vec<AgentStatus> {
        let inner = self.lock();
        let mut out: Vec<AgentStatus> = inner
            .iter()
            .map(|(name, e)| AgentStatus {
                name: name.clone(),
                def: e.def.clone(),
                description: e.description.clone(),
                state: e.state,
                pending: e.pending.len(),
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    fn test_session() -> Arc<Session> {
        Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "http://x".into()),
            runtime: crate::query::Runtime::new("m".into(), None, Default::default()),
            permission_mode: crate::permission::PermissionMode::Default,
            settings: crate::settings::Settings::default(),
            system: Vec::new(),
            depth: 1,
            home: std::env::temp_dir(),
            quiet: true,
            compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "t")),
            last_task_reminder_turn: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: AgentRegistry::new(),
        })
    }

    #[test]
    fn loads_defs_with_project_over_user_precedence() {
        let root = std::env::temp_dir().join(format!("bingo-agents-{}-load", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let project = root.join("project");
        write(
            &home.join(".config/bingo/agents/reviewer.md"),
            "---\ndescription: user reviewer\nmodel: haiku\n---\n你是评审。\n",
        );
        write(
            &project.join(".bingo/agents/reviewer.md"),
            "---\ndescription: project reviewer\n---\n你是项目评审。\n",
        );
        write(
            &project.join(".bingo/agents/scout.md"),
            "调研专用。\n",
        );
        let defs = load_agent_defs(&home, &project);
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["reviewer", "scout"], "项目层同名覆盖用户层");
        let reviewer = &defs[0];
        assert_eq!(reviewer.description, "project reviewer");
        assert!(reviewer.system.contains("项目评审"));
        assert!(reviewer.model.is_none(), "被覆盖的 user 定义不渗透");
        // 无 frontmatter：名字取文件名，描述回落正文首行。
        assert_eq!(defs[1].description, "调研专用。");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn frontmatter_name_and_model_override() {
        let root = std::env::temp_dir().join(format!("bingo-agents-{}-fm", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        write(
            &home.join(".config/bingo/agents/x.md"),
            "---\nname: 深潜\ndescription: >-\n  多行\n  描述\nmodel: sub-model\nprovider: ds\n---\nsystem 正文\n",
        );
        let defs = load_agent_defs(&home, &root);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "深潜", "frontmatter name 覆盖文件名");
        assert_eq!(defs[0].description, "多行 描述", "折叠标量");
        assert_eq!(defs[0].model.as_deref(), Some("sub-model"));
        assert_eq!(defs[0].provider.as_deref(), Some("ds"));
        assert_eq!(defs[0].system, "system 正文");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn claim_name_dedupes_and_defaults() {
        let reg = AgentRegistry::new();
        assert_eq!(reg.claim_name(""), "agent", "空名回落");
        assert_eq!(reg.claim_name("reviewer"), "reviewer");
        reg.insert("reviewer", None, "r".into(), test_session());
        assert_eq!(reg.claim_name("reviewer"), "reviewer-2");
        reg.insert("reviewer-2", None, "r".into(), test_session());
        assert_eq!(reg.claim_name("reviewer"), "reviewer-3");
    }

    #[test]
    fn lifecycle_running_idle_queue_and_revive() {
        let reg = AgentRegistry::new();
        reg.insert("scout", None, "调研".into(), test_session());
        // Running：消息排队。
        match reg.deliver("scout", "补充 A").unwrap_or_else(|e| panic!("{e}")) {
            Delivery::Queued => {}
            Delivery::Start { .. } => panic!("running 应排队"),
        }
        // 回合完成 + 有排队 → 立即续跑（历史已存，提示为排队内容）。
        let next = reg.finish("scout", vec![Message::user_text("hi")]);
        let (history, prompt) = next.unwrap_or_else(|| panic!("应续跑"));
        assert_eq!(history.len(), 1, "续跑携带最新历史");
        assert_eq!(prompt, "补充 A");
        assert_eq!(reg.list()[0].state, AgentState::Running);
        // 再次完成、无排队 → Idle。
        assert!(reg.finish("scout", Vec::new()).is_none());
        assert_eq!(reg.list()[0].state, AgentState::Idle);
        // Idle：deliver 唤醒（Start 携带历史与提示）。
        match reg.deliver("scout", "再看 B").unwrap_or_else(|e| panic!("{e}")) {
            Delivery::Start { prompt, .. } => assert_eq!(prompt, "再看 B"),
            Delivery::Queued => panic!("idle 应唤醒"),
        }
        assert_eq!(reg.list()[0].state, AgentState::Running);
    }

    #[test]
    fn multiple_pending_messages_merge_in_order() {
        let reg = AgentRegistry::new();
        reg.insert("w", None, "w".into(), test_session());
        let _ = reg.deliver("w", "先做 1");
        let _ = reg.deliver("w", "再做 2");
        let (_, prompt) = reg.finish("w", Vec::new()).unwrap_or_else(|| panic!("续跑"));
        assert!(prompt.contains("- 先做 1\n- 再做 2"), "{prompt}");
    }

    #[test]
    fn stop_and_delete_semantics() {
        let reg = AgentRegistry::new();
        reg.insert("x", None, "x".into(), test_session());
        reg.set_run_watch("x", crate::watch::WatchId(7));
        assert_eq!(
            reg.stop("x").unwrap_or_else(|e| panic!("{e}")),
            Some(crate::watch::WatchId(7)),
            "运行中停止返回当前 watch 行"
        );
        assert!(reg.stop("x").unwrap_or_else(|e| panic!("{e}")).is_none(), "幂等");
        assert!(reg.deliver("x", "还在吗").is_err(), "停止后拒收");
        // 停止后回合完成：历史仍存档，不复活。
        assert!(reg.finish("x", vec![Message::user_text("h")]).is_none());
        assert_eq!(reg.list()[0].state, AgentState::Stopped);
        reg.remove("x").unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.list().is_empty());
        assert_eq!(reg.claim_name("x"), "x", "删除释放名字");
        assert!(reg.deliver("x", "hi").is_err(), "未知实例报错");
        // 空闲实例停止：无进行中的行。
        reg.insert("y", None, "y".into(), test_session());
        reg.set_run_watch("y", crate::watch::WatchId(9));
        assert!(reg.finish("y", Vec::new()).is_none());
        assert!(
            reg.stop("y").unwrap_or_else(|e| panic!("{e}")).is_none(),
            "idle 停止不取消已终态的行"
        );
    }
}

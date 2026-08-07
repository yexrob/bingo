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

/// 定义来源层（D31 `/team list` 徽标；同名跨层 first-wins 取项目层）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDefSource {
    Project,
    User,
    /// 旧数据/旧配置无 source 时的显式缺省（不猜）。
    Unknown,
}

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
    /// 第一出处（first-wins 去重前的加载层）。
    pub source: AgentDefSource,
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

fn load_dir(dir: &Path, source: AgentDefSource, out: &mut Vec<AgentDef>) {
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
            source,
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
        load_dir(&dir, AgentDefSource::Project, &mut defs);
    }
    load_dir(&user_agents_dir(home), AgentDefSource::User, &mut defs);
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

/// 信箱条目：hub 直接指令，或频道消息（醒来批量注入，同序）。
#[derive(Debug, Clone)]
pub enum InboxItem {
    Direct(String),
    Channel {
        channel: String,
        from: String,
        text: String,
        seq: u64,
    },
}

/// SendMessage 的投递结果。
pub enum Delivery {
    /// 实例忙：已排队，回合结束自动送达。
    Queued,
    /// 实例空闲：以历史副本立即开新回合（信箱一并排空）。
    Start {
        session: Arc<Session>,
        history: Vec<Message>,
        items: Vec<InboxItem>,
    },
}

/// 频道投递结果（deposit）：同 Delivery，另有 Stopped 静默丢弃。
pub enum DepositOutcome {
    Queued,
    Start {
        session: Arc<Session>,
        history: Vec<Message>,
        items: Vec<InboxItem>,
    },
    /// 实例已停止：丢弃（停止的成员不再被唤醒）。
    Dropped,
}

struct Entry {
    def: Option<String>,
    description: String,
    state: AgentState,
    /// 最近一次完成回合后的完整消息历史（续话上下文）。
    history: Vec<Message>,
    /// 忙碌期间累积的信箱（指令 + 频道消息，回合边界批量注入）。
    inbox: Vec<InboxItem>,
    session: Arc<Session>,
    abort: Option<tokio::task::AbortHandle>,
    /// 累计回合数（watch 行标注 `#N`）。
    runs: u64,
    /// 当前回合的 watch 行（stop/delete 置 Cancelled 用）。
    watch_id: Option<crate::watch::WatchId>,
    /// 当前回合的流式产出（与 subagent_hooks 共享同一 Arc；
    /// 回合结束清空——TUI 实例视图据此显示活尾）。
    live: Option<Arc<Mutex<String>>>,
}

/// 会话级实例注册表（Session 持有 Arc，子会话共享）。
/// 单锁承载状态机 + 信箱：投递（deposit/deliver）与回合收口（finish）
/// 的"检查-认领"在同一把锁下原子完成，不存在丢失唤醒。
pub struct AgentRegistry {
    inner: Mutex<HashMap<String, Entry>>,
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
    /// `main`/`user` 为 hub 与用户保留（频道成员名），永不下发。
    pub fn claim_name(&self, base: &str) -> String {
        let base = if base.trim().is_empty() { "agent" } else { base.trim() };
        let taken = |inner: &HashMap<String, Entry>, name: &str| {
            name == crate::channels::HUB_NAME
                || name == crate::channels::USER_NAME
                || inner.contains_key(name)
        };
        let inner = self.lock();
        if !taken(&inner, base) {
            return base.to_string();
        }
        let mut n = 2;
        loop {
            let candidate = format!("{base}-{n}");
            if !taken(&inner, &candidate) {
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
    ) {        self.lock().insert(
            name.to_string(),
            Entry {
                def,
                description,
                state: AgentState::Running,
                history: Vec::new(),
                inbox: Vec::new(),
                session,
                abort: None,
                runs: 0,
                watch_id: None,
                live: None,
            },
        );
    }

    /// 注入实例的初始/恢复历史（D31 team 记忆恢复：不唤醒，仅预载续话上下文）。
    pub fn set_history(&self, name: &str, history: Vec<Message>) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.history = history;
        }
    }

    /// 当前回合的流式产出缓冲（回合开始挂上，结束摘下）。
    pub fn set_live(&self, name: &str, live: Option<Arc<Mutex<String>>>) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.live = live;
        }
    }

    /// 实例视图数据：历史 + 活尾 + 状态（不存在返回 None）。
    pub fn view_of(&self, name: &str) -> Option<(Vec<Message>, Option<String>, AgentState)> {
        let inner = self.lock();
        let entry = inner.get(name)?;
        let live = entry.live.as_ref().map(|l| {
            l.lock().unwrap_or_else(|e| e.into_inner()).clone()
        });
        Some((entry.history.clone(), live, entry.state))
    }

    /// 实例深度（频道 cohort 校验：只允许 depth==1 的直接子代理入频道）。
    pub fn depth_of(&self, name: &str) -> Option<usize> {
        self.lock().get(name).map(|e| e.session.depth)
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

    /// 回合完成：存入最新历史。信箱非空 → 保持 Running 并返回
    /// (历史副本, 排空的信箱)；空 → 转 Idle。
    /// Stopped（回合中被停止）不复活、不返回续跑。
    pub fn finish(
        &self,
        name: &str,
        history: Vec<Message>,
    ) -> Option<(Vec<Message>, Vec<InboxItem>)> {
        let mut inner = self.lock();
        let entry = inner.get_mut(name)?;
        entry.history = history;
        if entry.state == AgentState::Stopped {
            return None;
        }
        if entry.inbox.is_empty() {
            entry.state = AgentState::Idle;
            return None;
        }
        let items = std::mem::take(&mut entry.inbox);
        entry.state = AgentState::Running;
        Some((entry.history.clone(), items))
    }

    /// 回合失败：保留失败前历史，转 Idle（可经 SendMessage 重试）。
    pub fn mark_idle(&self, name: &str) {
        if let Some(entry) = self.lock().get_mut(name)
            && entry.state != AgentState::Stopped
        {
            entry.state = AgentState::Idle;
        }
    }

    /// 投递 hub 指令：Running 排队；Idle 唤醒（返回续跑所需的会话、
    /// 历史与排空的信箱）；Stopped/未知报错。
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
                entry.inbox.push(InboxItem::Direct(message.to_string()));
                Ok(Delivery::Queued)
            }
            AgentState::Idle => {
                entry.inbox.push(InboxItem::Direct(message.to_string()));
                let items = std::mem::take(&mut entry.inbox);
                entry.state = AgentState::Running;
                Ok(Delivery::Start {
                    session: entry.session.clone(),
                    history: entry.history.clone(),
                    items,
                })
            }
            AgentState::Stopped => Err(format!(
                "{name} 已停止，不再接收指令（delete 可移除该实例）"
            )),
        }
    }

    /// 投递频道消息：与 deliver 同构，但停止的成员静默丢弃（不报错——
    /// 群发不因个别成员停止而失败）。
    pub fn deposit(&self, name: &str, item: InboxItem) -> DepositOutcome {
        let mut inner = self.lock();
        let Some(entry) = inner.get_mut(name) else {
            return DepositOutcome::Dropped;
        };
        match entry.state {
            AgentState::Running => {
                entry.inbox.push(item);
                DepositOutcome::Queued
            }
            AgentState::Idle => {
                entry.inbox.push(item);
                let items = std::mem::take(&mut entry.inbox);
                entry.state = AgentState::Running;
                DepositOutcome::Start {
                    session: entry.session.clone(),
                    history: entry.history.clone(),
                    items,
                }
            }
            AgentState::Stopped => DepositOutcome::Dropped,
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
        entry.inbox.clear();
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
                pending: e.inbox.len(),
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
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
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
        assert_eq!(reviewer.source, AgentDefSource::Project, "跨层同名覆盖 source 取项目层");
        // 无 frontmatter：名字取文件名，描述回落正文首行。
        assert_eq!(defs[1].description, "调研专用。");
        assert_eq!(defs[1].source, AgentDefSource::Project);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 仅 user 层有定义时 source=User（D31 徽标数据）。
    #[test]
    fn source_is_user_when_only_user_layer_has_def() {
        let root = std::env::temp_dir().join(format!("bingo-agents-{}-src", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        write(
            &home.join(".config/bingo/agents/only-user.md"),
            "user 层专用。\n",
        );
        let defs = load_agent_defs(&home, &root);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "only-user");
        assert_eq!(defs[0].source, AgentDefSource::User);
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
        // 回合完成 + 信箱非空 → 立即续跑（历史已存，信箱排空）。
        let next = reg.finish("scout", vec![Message::user_text("hi")]);
        let (history, items) = next.unwrap_or_else(|| panic!("应续跑"));
        assert_eq!(history.len(), 1, "续跑携带最新历史");
        assert!(
            matches!(&items[..], [InboxItem::Direct(m)] if m == "补充 A"),
            "信箱内容"
        );
        assert_eq!(reg.list()[0].state, AgentState::Running);
        // 再次完成、信箱空 → Idle。
        assert!(reg.finish("scout", Vec::new()).is_none());
        assert_eq!(reg.list()[0].state, AgentState::Idle);
        // Idle：deliver 唤醒（Start 携带历史与信箱）。
        match reg.deliver("scout", "再看 B").unwrap_or_else(|e| panic!("{e}")) {
            Delivery::Start { items, .. } => {
                assert!(matches!(&items[..], [InboxItem::Direct(m)] if m == "再看 B"));
            }
            Delivery::Queued => panic!("idle 应唤醒"),
        }
        assert_eq!(reg.list()[0].state, AgentState::Running);
    }

    #[test]
    fn inbox_accumulates_direct_and_channel_items_in_order() {
        let reg = AgentRegistry::new();
        reg.insert("w", None, "w".into(), test_session());
        let _ = reg.deliver("w", "先做 1");
        match reg.deposit(
            "w",
            InboxItem::Channel {
                channel: "t".into(),
                from: "a".into(),
                text: "报数".into(),
                seq: 3,
            },
        ) {
            DepositOutcome::Queued => {}
            _ => panic!("running 应排队"),
        }
        let (_, items) = reg.finish("w", Vec::new()).unwrap_or_else(|| panic!("续跑"));
        assert_eq!(items.len(), 2);
        assert!(matches!(&items[0], InboxItem::Direct(m) if m == "先做 1"), "同序");
        assert!(
            matches!(&items[1], InboxItem::Channel { seq: 3, from, .. } if from == "a"),
            "频道条目携带 seq/from"
        );
        // Idle 时 deposit 唤醒；Stopped/未知静默丢弃。
        assert!(reg.finish("w", Vec::new()).is_none());
        match reg.deposit(
            "w",
            InboxItem::Channel {
                channel: "t".into(),
                from: "b".into(),
                text: "x".into(),
                seq: 4,
            },
        ) {
            DepositOutcome::Start { items, .. } => assert_eq!(items.len(), 1),
            _ => panic!("idle 应唤醒"),
        }
        let _ = reg.stop("w");
        assert!(matches!(
            reg.deposit("w", InboxItem::Direct("y".into())),
            DepositOutcome::Dropped
        ));
        assert!(matches!(
            reg.deposit("ghost", InboxItem::Direct("y".into())),
            DepositOutcome::Dropped
        ));
    }

    #[test]
    fn hub_name_is_reserved() {
        let reg = AgentRegistry::new();
        assert_eq!(reg.claim_name("main"), "main-2", "main 为 hub 保留");
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

//! Named agent definitions and the subagent instance registry (D29).
//!
//! Definitions (AgentDef): on-disk persona templates — frontmatter metadata plus a
//! system prompt body, mirroring the directory convention of skills. Instances
//! (AgentRegistry entries): live sessions produced by one spawn — they hold a child
//! Session with the full message history, and the main agent resumes the conversation
//! via SendMessage (hub-and-spoke: only the main session has the management tools).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::api::types::Message;
use crate::query::Session;

/// Definition source layer (D31 `/team list` badge; same-name first-wins across layers picks the project layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentDefSource {
    Project,
    User,
    /// Explicit default for legacy data/config without a source (no guessing).
    Unknown,
}

/// A named agent definition: `<name>.md` (YAML frontmatter + body system prompt).
#[derive(Debug, Clone)]
pub struct AgentDef {
    pub name: String,
    /// Catalog description (models are chosen based on this).
    pub description: String,
    /// Default model (instance params > definition > inherited from parent session).
    pub model: Option<String>,
    /// Default provider (same precedence as above).
    pub provider: Option<String>,
    /// Default thinking level (same precedence; None = inherit the parent session's current level).
    pub thinking: Option<String>,
    /// Body = the subagent's system prompt (replaces the parent session's system; empty means inherit).
    pub system: String,
    /// First origin (the loading layer before first-wins dedup).
    pub source: AgentDefSource,
}

/// User-level definitions directory: `$XDG_CONFIG_HOME/bingo/agents` (mirrors the skills convention).
/// Tests must not depend on the ambient XDG_CONFIG_HOME (CI runners may set it): the home
/// parameter is the sole source of truth under test.
fn user_agents_dir(home: &Path) -> PathBuf {
    #[cfg(not(test))]
    let config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".config"));
    #[cfg(test)]
    let config = home.join(".config");
    config.join("bingo").join("agents")
}

/// Walk up from cwd, looking for `.bingo/agents` at each level.
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
            thinking: None,
            system: body.trim_end().to_string(),
            source,
        };
        for (key, value) in pairs {
            match key.as_str() {
                "name" => def.name = value,
                "description" => def.description = value,
                "model" => def.model = Some(value),
                "provider" => def.provider = Some(value),
                "thinking" => def.thinking = Some(value),
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

/// Load all definitions: project layers (nearest cwd first) → user layer; same-name
/// first-wins (project overrides user). Definitions are usually few; no mtime caching.
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

/// Instance lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    /// Turn in progress (new messages queue and are delivered automatically at turn end).
    Running,
    /// Waiting for a command (SendMessage wakes it immediately; history is kept).
    Idle,
    /// Stopped (no longer receives messages; the name is released after delete).
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

/// Snapshot for list.
#[derive(Debug, Clone)]
pub struct AgentStatus {
    pub name: String,
    pub def: Option<String>,
    pub description: String,
    pub state: AgentState,
    pub pending: usize,
}

/// Inbox item: a direct hub command, or a channel message (injected in batch on wake, in order).
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

/// Delivery result of SendMessage.
pub enum Delivery {
    /// Instance busy: queued; delivered automatically at turn end.
    Queued,
    /// Instance idle: starts a new turn immediately with a history copy (inbox drained in the same pass).
    Start {
        session: Arc<Session>,
        history: Vec<Message>,
        items: Vec<InboxItem>,
    },
}

/// Channel delivery outcome (deposit): same as Delivery, except Stopped is silently dropped.
pub enum DepositOutcome {
    Queued,
    Start {
        session: Arc<Session>,
        history: Vec<Message>,
        items: Vec<InboxItem>,
    },
    /// Instance stopped: dropped (stopped members are no longer woken).
    Dropped,
}

struct Entry {
    def: Option<String>,
    description: String,
    state: AgentState,
    /// Full message history since the last completed turn (continuation context).
    history: Vec<Message>,
    /// Inbox accumulated while busy (commands + channel messages, injected in batch at turn boundaries).
    inbox: Vec<InboxItem>,
    session: Arc<Session>,
    abort: Option<tokio::task::AbortHandle>,
    /// Cumulative run count (watch lines are labeled `#N`).
    runs: u64,
    /// Watch line of the current turn (used to set Cancelled on stop/delete).
    watch_id: Option<crate::watch::WatchId>,
    /// Streaming output of the current turn (shares the same Arc with subagent_hooks;
    /// cleared at turn end — the TUI instance view shows the live tail from this).
    live: Option<Arc<Mutex<String>>>,
}

/// Session-level instance registry (Session holds the Arc; shared by child sessions).
/// A single lock carries the state machine + inbox: the check-and-claim of delivery
/// (deposit/deliver) and turn finalization (finish) happen atomically under one lock,
/// so no wakeup is ever lost.
pub struct AgentRegistry {
    inner: Mutex<HashMap<String, Entry>>,
    /// share 持久化（Option 语义：不挂接时行为不变；挂接后 insert/finish/stop 同步快照）。
    share: Mutex<Option<Arc<crate::share::ShareStore>>>,
}

impl AgentRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            share: Mutex::new(None),
        })
    }

    /// 挂接 share 持久化：之后实例的建/完成/停止事件同步进 share 文档。
    pub fn attach_share(&self, store: Arc<crate::share::ShareStore>) {
        *self.share.lock().unwrap_or_else(|e| e.into_inner()) = Some(store);
    }

    /// 把某实例的最新快照写入 share 文档（无 store 时 no-op）。
    fn sync_share(&self, name: &str) {
        let Some(store) = self.share.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return;
        };
        let inner = self.lock();
        let Some(entry) = inner.get(name) else {
            return;
        };
        store.upsert_agent(
            name,
            entry.def.clone(),
            entry.description.clone(),
            entry.state,
            entry.history.clone(),
        );
        store.persist();
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Claim an instance name: use the base name when free, otherwise append `-2`/`-3`…
    /// (so parallel same-name instances stay distinguishable).
    /// `main`/`user` are reserved for the hub and the user (channel member names) and
    /// are never handed out.
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

    /// Register a new instance (state=Running). The name must first go through claim_name.
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
        self.sync_share(name);
    }

    /// Inject an instance's initial/restored history (D31 team memory restore: no wake-up, only preloads continuation context).
    pub fn set_history(&self, name: &str, history: Vec<Message>) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.history = history;
        }
    }

    /// Streaming output buffer of the current turn (attached at turn start, detached at turn end).
    pub fn set_live(&self, name: &str, live: Option<Arc<Mutex<String>>>) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.live = live;
        }
    }

    /// Instance view data: history + live tail + state (None if the instance doesn't exist).
    pub fn view_of(&self, name: &str) -> Option<(Vec<Message>, Option<String>, AgentState)> {
        let inner = self.lock();
        let entry = inner.get(name)?;
        let live = entry.live.as_ref().map(|l| {
            l.lock().unwrap_or_else(|e| e.into_inner()).clone()
        });
        Some((entry.history.clone(), live, entry.state))
    }

    /// Instance depth (channel cohort check: only direct subagents with depth==1 may join a channel).
    pub fn depth_of(&self, name: &str) -> Option<usize> {
        self.lock().get(name).map(|e| e.session.depth)
    }

    pub fn set_abort(&self, name: &str, abort: tokio::task::AbortHandle) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.abort = Some(abort);
        }
    }

    /// Next run sequence number (starting at 1).
    pub fn next_run(&self, name: &str) -> u64 {
        match self.lock().get_mut(name) {
            Some(entry) => {
                entry.runs += 1;
                entry.runs
            }
            None => 1,
        }
    }

    /// Record the watch line of the current turn.
    pub fn set_run_watch(&self, name: &str, id: crate::watch::WatchId) {
        if let Some(entry) = self.lock().get_mut(name) {
            entry.watch_id = Some(id);
        }
    }

    /// Turn finished: store the latest history. Inbox non-empty → stay Running and
    /// return (history copy, drained inbox); empty → switch to Idle.
    /// Stopped (stopped mid-turn) never revives and never returns a continuation.
    pub fn finish(
        &self,
        name: &str,
        history: Vec<Message>,
    ) -> Option<(Vec<Message>, Vec<InboxItem>)> {
        let result = {
            let mut inner = self.lock();
            let entry = inner.get_mut(name)?;
            entry.history = history;
            if entry.state == AgentState::Stopped {
                None
            } else if entry.inbox.is_empty() {
                entry.state = AgentState::Idle;
                None
            } else {
                let items = std::mem::take(&mut entry.inbox);
                entry.state = AgentState::Running;
                Some((entry.history.clone(), items))
            }
        };
        self.sync_share(name);
        result
    }

    /// Turn failed: keep the pre-failure history, switch to Idle (retryable via SendMessage).
    pub fn mark_idle(&self, name: &str) {
        if let Some(entry) = self.lock().get_mut(name)
            && entry.state != AgentState::Stopped
        {
            entry.state = AgentState::Idle;
        }
    }

    /// Deliver a hub command: queue when Running; wake when Idle (returns the session,
    /// history and drained inbox needed to continue); error when Stopped/unknown.
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

    /// Deliver a channel message: same shape as deliver, but stopped members are silently
    /// dropped (no error — a broadcast doesn't fail because one member stopped).
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

    /// Stop: abort a running turn (abort), no longer accept commands; history is kept
    /// and listable. Returns the watch line of the aborted turn (the caller sets
    /// Cancelled); when idle/already stopped there is no active line, returns None (idempotent).
    pub fn stop(&self, name: &str) -> Result<Option<crate::watch::WatchId>, String> {
        let watch_id = {
            let mut inner = self.lock();
            let Some(entry) = inner.get_mut(name) else {
                return Err(format!("没有名为 {name} 的子代理"));
            };
            if entry.state == AgentState::Stopped {
                None
            } else {
                let was_running = entry.state == AgentState::Running;
                entry.state = AgentState::Stopped;
                entry.inbox.clear();
                if let Some(abort) = entry.abort.take() {
                    abort.abort();
                }
                if was_running { entry.watch_id } else { None }
            }
        };
        self.sync_share(name);
        Ok(watch_id)
    }

    /// Remove: stop first, then drop the entry (name released). Returns the watch line of the aborted turn.
    pub fn remove(&self, name: &str) -> Result<Option<crate::watch::WatchId>, String> {
        let id = self.stop(name)?;
        self.lock().remove(name);
        Ok(id)
    }

    /// Snapshot of all instances (sorted by name for stable list output).
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
        // No frontmatter: name comes from the file name, description falls back to the first body line.
        assert_eq!(defs[1].description, "调研专用。");
        assert_eq!(defs[1].source, AgentDefSource::Project);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// source=User when only the user layer has a definition (D31 badge data).
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
            "---\nname: 深潜\ndescription: >-\n  多行\n  描述\nmodel: sub-model\nprovider: ds\nthinking: xhigh\n---\nsystem 正文\n",
        );
        let defs = load_agent_defs(&home, &root);
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "深潜", "frontmatter name 覆盖文件名");
        assert_eq!(defs[0].description, "多行 描述", "折叠标量");
        assert_eq!(defs[0].model.as_deref(), Some("sub-model"));
        assert_eq!(defs[0].provider.as_deref(), Some("ds"));
        assert_eq!(defs[0].thinking.as_deref(), Some("xhigh"));
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
        // Running: message queued.
        match reg.deliver("scout", "补充 A").unwrap_or_else(|e| panic!("{e}")) {
            Delivery::Queued => {}
            Delivery::Start { .. } => panic!("running 应排队"),
        }
        // Turn finished + inbox non-empty → continues immediately (history saved, inbox drained).
        let next = reg.finish("scout", vec![Message::user_text("hi")]);
        let (history, items) = next.unwrap_or_else(|| panic!("应续跑"));
        assert_eq!(history.len(), 1, "续跑携带最新历史");
        assert!(
            matches!(&items[..], [InboxItem::Direct(m)] if m == "补充 A"),
            "信箱内容"
        );
        assert_eq!(reg.list()[0].state, AgentState::Running);
        // Finish again with an empty inbox → Idle.
        assert!(reg.finish("scout", Vec::new()).is_none());
        assert_eq!(reg.list()[0].state, AgentState::Idle);
        // Idle: deliver wakes it (Start carries history and inbox).
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
        // Idle: deposit wakes it; Stopped/unknown silently dropped.
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
    fn share_hooks_track_insert_finish_stop() {
        let root = std::env::temp_dir().join(format!("bingo-agents-{}-share", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store =
            crate::share::ShareStore::load_or_create(&root.join("shares").join("s.json"))
                .unwrap_or_else(|e| panic!("{e}"));
        let reg = AgentRegistry::new();
        reg.attach_share(store.clone());

        // insert → 建条目（running，空历史）。
        reg.insert("scout", Some("scout".into()), "调研".into(), test_session());
        let doc = store.snapshot();
        assert_eq!(doc.agents.len(), 1);
        assert_eq!(doc.agents[0].state, "running");
        assert_eq!(doc.agents[0].def.as_deref(), Some("scout"));
        assert!(doc.agents[0].history.is_empty());

        // finish → 历史 + 状态（空信箱 → idle）。
        reg.finish("scout", vec![Message::user_text("hi")]);
        let doc = store.snapshot();
        assert_eq!(doc.agents[0].state, "idle");
        assert_eq!(doc.agents[0].history.len(), 1);
        assert_eq!(doc.agents[0].history[0], Message::user_text("hi"));

        // 忙碌信箱非空 → finish 后保持 running（Idle 唤醒排空 inbox 给 Start，
        // Running 时才排队；两条指令制造排队场景）。
        reg.deliver("scout", "再查").unwrap_or_else(|e| panic!("{e}"));
        reg.deliver("scout", "又查").unwrap_or_else(|e| panic!("{e}"));
        reg.finish("scout", Vec::new());
        let doc = store.snapshot();
        assert_eq!(doc.agents[0].state, "running");
        // 信箱排空 → idle。
        reg.finish("scout", Vec::new());
        let doc = store.snapshot();
        assert_eq!(doc.agents[0].state, "idle");

        // stop → stopped。
        reg.stop("scout").unwrap_or_else(|e| panic!("{e}"));
        let doc = store.snapshot();
        assert_eq!(doc.agents[0].state, "stopped");
        let _ = std::fs::remove_dir_all(&root);
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
        // Turn finishing after a stop: history is still archived, no revival.
        assert!(reg.finish("x", vec![Message::user_text("h")]).is_none());
        assert_eq!(reg.list()[0].state, AgentState::Stopped);
        reg.remove("x").unwrap_or_else(|e| panic!("{e}"));
        assert!(reg.list().is_empty());
        assert_eq!(reg.claim_name("x"), "x", "删除释放名字");
        assert!(reg.deliver("x", "hi").is_err(), "未知实例报错");
        // Stopping an idle instance: no active line.
        reg.insert("y", None, "y".into(), test_session());
        reg.set_run_watch("y", crate::watch::WatchId(9));
        assert!(reg.finish("y", Vec::new()).is_none());
        assert!(
            reg.stop("y").unwrap_or_else(|e| panic!("{e}")).is_none(),
            "idle 停止不取消已终态的行"
        );
    }
}

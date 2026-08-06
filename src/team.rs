//! agent team：项目级编队（D31）。
//!
//! 心智模型：team 是图纸（`.bingo/team.json` 持久定义），room 是工地
//! （运行时实例 + 频道）。本模块 = 三块薄层：team.json 解析与校验
//! （validate 与 start 同源：validate 能过 start 必成）、`spawn_team`
//! 编排（复用现有 Agent spawn + ChannelRegistry，幂等键 = 实例名）、
//! team 记忆（键 = 项目路径哈希 + 分支，跨会话恢复）。
//!
//! 成员引用 AgentDef 而非内联人格——人格单一事实来源仍在
//! `.bingo/agents/<名>.md`，team 只是编队层。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use thiserror::Error;

use crate::agents::{AgentDef, AgentDefSource};
use crate::channels::ChannelMode;
use crate::query::Session;

/// team 配置文件（项目层 `.bingo/team.json`，进版本库）。
pub const TEAM_FILE: &str = ".bingo/team.json";
/// 记忆根目录：`~/.config/bingo/teams/`。
const TEAM_MEMORY_ROOT: &str = "teams";

#[derive(Debug, Error)]
pub enum TeamError {
    #[error("team.json 读取失败: {0}")]
    Io(#[from] std::io::Error),
    #[error("team.json 解析失败: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}

impl TeamError {
    fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }
}

/// 房间规格（复用 Channel 既有词汇，不发明新概念）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSpec {
    /// 发言模式：serial（缺省）| free。
    #[serde(default)]
    pub mode: Option<String>,
    /// 每频道消息总上限（缺省 500，见 ChannelLimits）。
    #[serde(rename = "messageLimit", default)]
    pub message_limit: Option<u64>,
}

/// 单个成员：`name`（实例名）+ `agent`（引用的 AgentDef 名）。
#[derive(Debug, Clone, Deserialize)]
pub struct TeamMember {
    pub name: String,
    pub agent: String,
}

/// team 定义（图纸）。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamDef {
    pub name: String,
    #[serde(default)]
    pub channel: Option<ChannelSpec>,
    pub members: Vec<TeamMember>,
}

/// 解析 `.bingo/team.json`：不存在返回 Ok(None)；存在则解析 + 结构校验
/// （类型/枚举非法即错误）。成员引用的 AgentDef 存在性由 `validate` 查
/// （需要加载后的定义清单，不在纯解析里做）。
pub fn load_team_file(project_dir: &Path) -> Result<Option<TeamDef>, TeamError> {
    let path = project_dir.join(TEAM_FILE);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let def: TeamDef = serde_json::from_str(&raw)?;
    validate_structure(&def, &path)?;
    Ok(Some(def))
}

/// 结构校验（不依赖 AgentDef 清单）：名字/频道模式/成员约束。
/// 与 `validate` 共享错误格式（三段式：文件路径 + 字段路径 + 期望）。
fn validate_structure(def: &TeamDef, path: &Path) -> Result<(), TeamError> {
    let file = path.display();
    if def.name.trim().is_empty() {
        return Err(TeamError::invalid(format!(
            "{file}: name: 不能为空（team 需要名字以区分）"
        )));
    }
    if def.members.is_empty() {
        return Err(TeamError::invalid(format!(
            "{file}: members: 不能为空（空 team 没有意义；单成员 team 合法）"
        )));
    }
    if let Some(spec) = &def.channel {
        if let Some(mode) = &spec.mode {
            ChannelMode::parse(mode).map_err(|e| {
                TeamError::invalid(format!("{file}: channel.mode: {e}"))
            })?;
        }
        if let Some(limit) = spec.message_limit
            && limit == 0
        {
            return Err(TeamError::invalid(format!(
                "{file}: channel.messageLimit: 必须为正整数"
            )));
        }
    }
    let mut seen = std::collections::HashSet::new();
    for (i, m) in def.members.iter().enumerate() {
        if m.name.trim().is_empty() {
            return Err(TeamError::invalid(format!(
                "{file}: members[{i}].name: 不能为空"
            )));
        }
        if m.agent.trim().is_empty() {
            return Err(TeamError::invalid(format!(
                "{file}: members[{i}].agent: 不能为空（需引用一个 AgentDef）"
            )));
        }
        if !seen.insert(m.name.as_str()) {
            return Err(TeamError::invalid(format!(
                "{file}: members[{i}].name: 配置内重名 \"{}\"（成员名须唯一）",
                m.name
            )));
        }
    }
    Ok(())
}

/// 引用校验：每个成员的 agent 必须存在于定义清单（项目层 + user 层）。
/// `/team validate` 与 `spawn_team` 共用（同源：validate 能过 start 必成）。
pub fn validate(def: &TeamDef, defs: &[AgentDef]) -> Result<(), TeamError> {
    let by_name: HashMap<&str, &AgentDef> = defs.iter().map(|d| (d.name.as_str(), d)).collect();
    for (i, m) in def.members.iter().enumerate() {
        if !by_name.contains_key(m.agent.as_str()) {
            let known: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
            let hint = if known.is_empty() {
                "没有任何 AgentDef（项目层 `.bingo/agents/*.md` 或 user 层 `~/.config/bingo/agents/*.md`）"
                    .to_string()
            } else {
                format!("可用：{}", known.join(", "))
            };
            return Err(TeamError::invalid(format!(
                "{TEAM_FILE}: members[{i}].agent: 引用不存在的 AgentDef \"{}\"；{hint}",
                m.agent
            )));
        }
    }
    Ok(())
}

/// 展示用：team 定义 + 其成员引用的定义（/team list 定义区）。
#[derive(Debug, Clone)]
pub struct TeamView {
    pub def: TeamDef,
    pub members: Vec<MemberView>,
}

#[derive(Debug, Clone)]
pub struct MemberView {
    pub name: String,
    pub agent: String,
    pub description: String,
    pub source: AgentDefSource,
}

/// 定义区视图：成员引用缺失时 source 记 Unknown、描述留空（不报错——
/// 展示层对坏引用宽容，拉起时才拒绝）。
pub fn view(def: &TeamDef, defs: &[AgentDef]) -> TeamView {
    let by_name: HashMap<&str, &AgentDef> = defs.iter().map(|d| (d.name.as_str(), d)).collect();
    TeamView {
        def: def.clone(),
        members: def
            .members
            .iter()
            .map(|m| {
                let agent = by_name.get(m.agent.as_str());
                MemberView {
                    name: m.name.clone(),
                    agent: m.agent.clone(),
                    description: agent
                        .map(|a| a.description.clone())
                        .unwrap_or_else(|| "（缺失定义）".to_string()),
                    source: agent.map(|a| a.source).unwrap_or(AgentDefSource::Unknown),
                }
            })
            .collect(),
    }
}

/// 频道模式解析（缺省 serial）。
pub fn channel_mode(def: &TeamDef) -> ChannelMode {
    def.channel
        .as_ref()
        .and_then(|s| s.mode.as_deref())
        .and_then(|m| ChannelMode::parse(m).ok())
        .unwrap_or(ChannelMode::Serial)
}

// ---- team 记忆（键 = 项目路径哈希 + 分支） ----

/// 记忆根目录：`~/.config/bingo/teams/`（user 层，默认不进版本库）。
pub fn team_memory_root(home: &Path) -> PathBuf {
    let config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".config"));
    config.join("bingo").join(TEAM_MEMORY_ROOT)
}

/// 项目键：`<目录名>-<完整路径哈希>`（与项目记忆 `memory_file` 同键族；
/// worktree 场景天然隔离——不同 worktree 路径不同 → 键不同）。
pub fn project_key(project_dir: &Path) -> String {
    let name = project_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());
    let name: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    format!("{name}-{}", crate::memory::path_hash(project_dir))
}

/// 当前分支名（非 git 仓库/无分支时回落 "detached"）。
pub fn current_branch(project_dir: &Path) -> String {
    std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["branch", "--show-current"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "detached".to_string())
}

/// 某 team 在某项目+分支下的记忆目录：
/// `~/.config/bingo/teams/<project_key>/<branch>/<team>/`。
pub fn team_memory_dir(home: &Path, project_dir: &Path, branch: &str, team: &str) -> PathBuf {
    team_memory_root(home)
        .join(project_key(project_dir))
        .join(branch)
        .join(team)
}

/// 成员历史文件（完整消息历史落盘，供跨会话恢复）。
pub fn member_history_path(dir: &Path, member: &str) -> PathBuf {
    dir.join(format!("{}.json", sanitize_name(member)))
}

/// 决策记录文件（append-only，`sources` 管道分隔，复用 frontmatter 约定）。
pub fn decisions_path(dir: &Path) -> PathBuf {
    dir.join("decisions.md")
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

// ---- 拉起编排（spawn_team） ----

/// 一次拉起的摘要（/team start 输出与事件日志共用）。
#[derive(Debug, Clone, Default)]
pub struct SpawnSummary {
    /// 新派生的实例名。
    pub spawned: Vec<String>,
    /// 复用既有空闲实例（幂等：未重派）。
    pub reused: Vec<String>,
    /// 失败的成员：(实例名, 原因)。
    pub failed: Vec<(String, String)>,
}

impl SpawnSummary {
    /// 事件措辞（qa 验收：`spawned ×N` vs `reused ×N` 可 grep 可断言）。
    pub fn events(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.spawned.is_empty() {
            out.push(format!("spawned ×{}", self.spawned.len()));
        }
        if !self.reused.is_empty() {
            out.push(format!("reused ×{}", self.reused.len()));
        }
        out
    }
}

/// 拉起 team（D31）：建频道（幂等）+ 派生/复用成员实例（「拉起 ≠ 唤醒」——
/// 成员走 Idle 待命态，零 token、零回合，等 SendMessage/频道消息才开跑）。
/// 记忆恢复同走本路径：有落盘历史则预载（不唤醒），缺文件静默回落空历史。
/// 成员级失败隔离：单个失败不拖垮全队，失败者留在 failed 可单独 re-spawn。
/// 返回 `Err` 仅当配置校验失败（validate 与 start 同源）。
pub fn spawn_team(
    session: &Arc<Session>,
    def: &TeamDef,
    defs: &[AgentDef],
    home: &Path,
    project_dir: &Path,
    branch: &str,
) -> Result<SpawnSummary, TeamError> {
    validate(def, defs)?;
    let mut summary = SpawnSummary::default();

    // 频道幂等：create-if-not-exists。
    let channel_name = &def.name;
    let channel_exists = session.channels.info(channel_name).is_some();
    if !channel_exists
        && let Err(e) = session.channels.create(channel_name, Vec::new(), channel_mode(def))
    {
        summary.failed.push((channel_name.clone(), e));
    }
    if let Some(limit) = def.channel.as_ref().and_then(|s| s.message_limit) {
        let _ = session.channels.set_message_limit(channel_name, limit);
    }

    let by_name: HashMap<&str, &AgentDef> = defs.iter().map(|d| (d.name.as_str(), d)).collect();
    for member in &def.members {
        // 幂等键 = 实例名：已存在（Idle/Running）→ 复用，不重派。
        let exists = session
            .agents
            .list()
            .iter()
            .any(|a| a.name == member.name);
        if exists {
            summary.reused.push(member.name.clone());
            continue;
        }
        let Some(agent_def) = by_name.get(member.agent.as_str()) else {
            summary.failed.push((
                member.name.clone(),
                format!("引用不存在的 AgentDef \"{}\"", member.agent),
            ));
            continue;
        };
        let name = session.agents.claim_name(&member.name);
        let sub = match crate::tool::agent::build_sub_session(
            session,
            None,
            None,
            Some(agent_def),
            &name,
        ) {
            Ok(s) => s,
            Err(e) => {
                summary.failed.push((member.name.clone(), e.to_string()));
                continue;
            }
        };
        let description = agent_def.description.clone();
        session.agents.insert(&name, Some(member.agent.clone()), description, sub);
        // 记忆恢复：有落盘历史则预载（不唤醒；SendMessage 续话时自动携带）。
        let history = load_member_history(home, project_dir, branch, &def.name, &name);
        if !history.is_empty() {
            session.agents.set_history(&name, history);
        }
        // 拉起 ≠ 唤醒：insert 后置 Idle（零 token 待命；回合从 SendMessage 才开始）。
        session.agents.mark_idle(&name);
        // 入席频道（迟入无 backlog，从当前头开始听）。
        let _ = session.channels.invite(channel_name, &name);
        summary.spawned.push(name);
    }
    Ok(summary)
}

// ---- 记忆读写（跨会话恢复） ----

/// 保存成员完整消息历史（落盘 JSON；失败静默——记忆是增强不是契约）。
pub fn save_member_history(
    home: &Path,
    project_dir: &Path,
    branch: &str,
    team: &str,
    member: &str,
    history: &[crate::api::types::Message],
) {
    let dir = team_memory_dir(home, project_dir, branch, team);
    let Ok(_) = std::fs::create_dir_all(&dir) else {
        return;
    };
    let path = member_history_path(&dir, member);
    if let Ok(json) = serde_json::to_string_pretty(history) {
        let _ = std::fs::write(path, json);
    }
}

/// 读取成员历史（不存在/损坏 → 空，静默回落）。
pub fn load_member_history(
    home: &Path,
    project_dir: &Path,
    branch: &str,
    team: &str,
    member: &str,
) -> Vec<crate::api::types::Message> {
    let path = member_history_path(&team_memory_dir(home, project_dir, branch, team), member);
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

/// 追加一条决策记录（append-only，零模型成本；frontmatter 管道分隔约定
/// `sources: a|b|c`，`type` 下沉条目级）。失败静默。
pub fn append_decision(
    home: &Path,
    project_dir: &Path,
    branch: &str,
    team: &str,
    kind: &str,
    text: &str,
    sources: &[&str],
) {
    let dir = team_memory_dir(home, project_dir, branch, team);
    let Ok(_) = std::fs::create_dir_all(&dir) else {
        return;
    };
    let path = decisions_path(&dir);
    let mut entry = format!("- type: {kind}\n  text: {text}\n");
    if !sources.is_empty() {
        entry.push_str(&format!("  sources: {}\n", sources.join("|")));
    }
    use std::io::Write;
    let mut file = match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(f) => f,
        Err(_) => return,
    };
    let _ = writeln!(file, "{entry}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::agents::AgentRegistry;
    use crate::channels::{ChannelLimits, ChannelRegistry};

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bingo-team-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_team(dir: &std::path::Path, content: &str) -> std::path::PathBuf {
        let path = dir.join(TEAM_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn parses_valid_team_file() {
        let dir = tmp("parse");
        write_team(
            &dir,
            r#"{"name":"dev-room","channel":{"mode":"serial","messageLimit":100},"members":[{"name":"dev-ex","agent":"dev-ex"},{"name":"ui","agent":"ui/ux"}]}"#,
        );
        let def = load_team_file(&dir).unwrap().unwrap();
        assert_eq!(def.name, "dev-room");
        assert_eq!(def.members.len(), 2);
        assert_eq!(channel_mode(&def), ChannelMode::Serial);
        assert_eq!(def.channel.as_ref().unwrap().message_limit, Some(100));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_returns_none() {
        let dir = tmp("missing");
        assert!(load_team_file(&dir).unwrap().is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_bad_structure_with_field_path() {
        let dir = tmp("bad");
        // 空成员。
        let path = write_team(&dir, r#"{"name":"t","members":[]}"#);
        let err = load_team_file(&dir).unwrap_err().to_string();
        assert!(err.contains("members") && err.contains("不能为空"), "{err}");
        // 配置内重名。
        write_team(
            &dir,
            r#"{"name":"t","members":[{"name":"a","agent":"x"},{"name":"a","agent":"y"}]}"#,
        );
        let err = load_team_file(&dir).unwrap_err().to_string();
        assert!(err.contains("重名") && err.contains("members[1]"), "{err}");
        // 非法频道模式。
        write_team(
            &dir,
            r#"{"name":"t","channel":{"mode":"bogus"},"members":[{"name":"a","agent":"x"}]}"#,
        );
        let err = load_team_file(&dir).unwrap_err().to_string();
        assert!(err.contains("channel.mode"), "{err}");
        let _ = path;
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn validate_checks_agent_refs() {
        let dir = tmp("ref");
        let def = TeamDef {
            name: "t".into(),
            channel: None,
            members: vec![TeamMember {
                name: "a".into(),
                agent: "ghost".into(),
            }],
        };
        let err = validate(&def, &[]).unwrap_err().to_string();
        assert!(
            err.contains("ghost") && err.contains("没有任何 AgentDef"),
            "{err}"
        );
        let known = AgentDef {
            name: "real".into(),
            description: "d".into(),
            model: None,
            provider: None,
            system: "s".into(),
            source: AgentDefSource::Project,
        };
        let ok = TeamDef {
            name: "t".into(),
            channel: None,
            members: vec![TeamMember {
                name: "a".into(),
                agent: "real".into(),
            }],
        };
        assert!(validate(&ok, &[known]).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn memory_dir_scopes_by_project_and_branch() {
        let home = std::path::Path::new("/tmp/home");
        let a = team_memory_dir(home, std::path::Path::new("/work/alpha"), "main", "dev");
        let b = team_memory_dir(home, std::path::Path::new("/work/beta"), "main", "dev");
        assert_ne!(a, b, "不同项目隔离");
        // 同项目不同分支隔离（worktree 场景）。
        let c = team_memory_dir(home, std::path::Path::new("/work/alpha"), "agent-team", "dev");
        assert_ne!(a, c, "不同分支隔离");
        assert!(a.starts_with(team_memory_root(home)));
        assert!(a.to_string_lossy().contains("dev"), "{a:?}");
    }

    #[test]
    fn project_key_is_stable_and_path_scoped() {
        let p = std::path::Path::new("/tmp/h/proj");
        assert_eq!(project_key(p), project_key(p), "稳定");
        assert!(
            project_key(std::path::Path::new("/a/web")) != project_key(std::path::Path::new("/b/web")),
            "同名目录不同项目不碰撞"
        );
    }

    fn def(name: &str) -> AgentDef {
        AgentDef {
            name: name.into(),
            description: format!("{name} 描述"),
            model: None,
            provider: None,
            system: format!("你是 {name}。"),
            source: AgentDefSource::Project,
        }
    }

    fn team_def(name: &str, members: &[(&str, &str)]) -> TeamDef {
        TeamDef {
            name: name.into(),
            channel: None,
            members: members
                .iter()
                .map(|(n, a)| TeamMember {
                    name: n.to_string(),
                    agent: a.to_string(),
                })
                .collect(),
        }
    }

    fn session() -> Arc<Session> {
        Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "http://x".into()),
            runtime: crate::query::Runtime::new("m".into(), None, Default::default()),
            permission_mode: crate::permission::PermissionMode::Default,
            settings: crate::settings::Settings::default(),
            system: Vec::new(),
            depth: 0,
            home: std::env::temp_dir(),
            quiet: true,
            compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "t")),
            last_task_reminder_turn: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: AgentRegistry::new(),
            channels: ChannelRegistry::new(ChannelLimits::default()),
            instance: None,
        })
    }

    /// 拉起 ≠ 唤醒：新派生成员 Idle（零回合）、房间建成；重复 start 幂等复用。
    #[test]
    fn spawn_team_is_idempotent_and_members_idle() {
        let s = session();
        let mem_home = tmp("spawn-mem");
        let defs = vec![def("dev-ex"), def("ui/ux"), def("dev")];
        let team = team_def("dev-room", &[("dev-ex", "dev-ex"), ("ui", "ui/ux"), ("dev", "dev")]);

        let first = spawn_team(&s, &team, &defs, &mem_home, &mem_home, "main")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(first.spawned.len(), 3, "{first:?}");
        assert!(first.reused.is_empty());
        assert!(first.failed.is_empty());
        // 成员 Idle 待命（零 token 未开回合）；频道建成含 hub/user + 三成员。
        let states = s.agents.list();
        assert_eq!(states.len(), 3);
        assert!(states.iter().all(|a| a.state == crate::agents::AgentState::Idle));
        let ch = s.channels.info("dev-room").unwrap_or_else(|| panic!("频道应存在"));
        assert_eq!(ch.members, vec!["main", "user", "dev-ex", "ui", "dev"]);

        // 重复 start：全部复用，不重派。
        let second = spawn_team(&s, &team, &defs, &mem_home, &mem_home, "main")
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(second.spawned.is_empty());
        assert_eq!(second.reused.len(), 3, "{second:?}");
        assert_eq!(s.agents.list().len(), 3, "不产生重复实例");
        std::fs::remove_dir_all(&mem_home).unwrap();
    }

    /// 记忆恢复：落盘历史在 spawn 时预载进实例（不唤醒，等 SendMessage 续话携带）。
    #[test]
    fn spawn_team_restores_member_history() {
        let s = session();
        let mem_home = tmp("spawn-restore");
        let defs = vec![def("qa")];
        let team = team_def("t", &[("qa", "qa")]);
        let msgs = vec![crate::api::types::Message::user_text("上一轮结论")];
        save_member_history(&mem_home, &mem_home, "main", "t", "qa", &msgs);

        spawn_team(&s, &team, &defs, &mem_home, &mem_home, "main")
            .unwrap_or_else(|e| panic!("{e}"));
        let (history, _, state) = s.agents.view_of("qa").unwrap_or_else(|| panic!("实例应存在"));
        assert_eq!(history.len(), 1, "历史已预载");
        assert_eq!(state, crate::agents::AgentState::Idle, "恢复不唤醒");
        std::fs::remove_dir_all(&mem_home).unwrap();
    }

    /// 配置校验失败（引用全缺）→ Err，不拉起任何东西（validate 与 start 同源）。
    #[test]
    fn spawn_team_returns_err_on_invalid_config() {
        let s = session();
        let mem_home = tmp("spawn-err");
        let team = team_def("t", &[("x", "nope")]);
        let err = spawn_team(&s, &team, &[], &mem_home, &mem_home, "main")
            .unwrap_err()
            .to_string();
        assert!(err.contains("nope"), "{err}");
        assert!(s.agents.list().is_empty(), "校验失败不产生副作用");
        std::fs::remove_dir_all(&mem_home).unwrap();
    }

    #[test]
    fn memory_roundtrip_and_decision_append() {
        let home = tmp("mem");
        let project = home.join("proj");
        let branch = "agent-team";
        let team = "dev-room";
        let msgs = vec![
            crate::api::types::Message::user_text("第一轮"),
            crate::api::types::Message::user_text("第二轮"),
        ];
        save_member_history(&home, &project, branch, team, "dev", &msgs);
        let loaded = load_member_history(&home, &project, branch, team, "dev");
        assert_eq!(loaded.len(), 2, "roundtrip 等值");
        assert_eq!(loaded[0].content, msgs[0].content);
        // 缺失/损坏回落空。
        assert!(load_member_history(&home, &project, branch, team, "ghost").is_empty());
        // 决策记录 append-only。
        append_decision(&home, &project, branch, team, "decision", "用 JSON 不用 YAML", &["dev", "qa"]);
        append_decision(&home, &project, branch, team, "decision", "第二案", &["ui/ux"]);
        let raw = std::fs::read_to_string(decisions_path(&team_memory_dir(&home, &project, branch, team)))
            .unwrap();
        assert_eq!(raw.matches("type: decision").count(), 2, "追加两条");
        assert!(raw.contains("sources: dev|qa"), "管道分隔 sources");
        std::fs::remove_dir_all(&home).unwrap();
    }
}

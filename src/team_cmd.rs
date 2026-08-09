//! `/team` command family implementation (D31): list/start/status/assign/stop/validate/new + memory.
//! Purely functional: takes Session + cwd + args, returns output lines (chat.rs pushes after dispatch).
//! The three-part error format (file path + field path + expectation) is shared with spawn/validate.

use std::path::Path;
use std::sync::Arc;

use crate::agents::AgentState;
use crate::query::Session;

/// Main entry: `/team <subcommand>`, returns the lines to display. Unknown subcommands get usage.
pub fn run(session: &Arc<Session>, cwd: &Path, arg: &str) -> Vec<String> {
    let mut parts = arg.split_whitespace();
    let sub = parts.next().unwrap_or("");
    match sub {
        "" => usage(),
        "list" | "ls" => list(session, cwd),
        "start" | "up" => start(session, cwd),
        "status" | "st" => status(session, cwd),
        "assign" | "say" => assign(session, cwd, parts.collect::<Vec<_>>().join(" ")),
        "stop" | "down" => stop(session, cwd),
        "validate" | "check" => validate(session, cwd),
        "new" => new_team(session, cwd, parts.next().unwrap_or("")),
        "memory" => memory(session, cwd, parts.next().unwrap_or("")),
        other => {
            let mut out = vec![format!("未知子命令: /team {other}"), String::new()];
            out.extend(usage());
            out
        }
    }
}

fn usage() -> Vec<String> {
    vec![
        "用法: /team <list|start|status|assign|stop|validate|new|memory>".to_string(),
        "  list       定义区（图纸）+ 运行区（工地）同屏".to_string(),
        "  start      拉起 team（成员待命 · 幂等复用）".to_string(),
        "  status     成员状态（●待命 ◐忙碌 ✗异常 ○离线）".to_string(),
        "  assign     派任务给成员（/team assign <成员> <任务>）".to_string(),
        "  stop       停止 team（成员不再接收指令）".to_string(),
        "  validate   校验 team.json（与 start 同源）".to_string(),
        "  new        脚手架：生成 .bingo/team.json（产物必过 validate）".to_string(),
        "  memory     记忆管理（list 查看 / gc 清理）".to_string(),
    ]
}

/// Load the team definition + the definition list. Parse errors (invalid JSON, etc.)
/// → Err (not silently swallowed).
fn load(
    session: &Arc<Session>,
    cwd: &Path,
) -> Result<Option<(crate::team::TeamDef, Vec<crate::agents::AgentDef>)>, String> {
    let defs = crate::agents::load_agent_defs(&session.home, cwd);
    crate::team::load_team_file(cwd)
        .map(|opt| opt.map(|def| (def, defs)))
        .map_err(|e| e.to_string())
}

/// Convenience: both Ok(None) and Err become a "no team file" output (shared wording across read commands).
fn load_or_no_team(
    session: &Arc<Session>,
    cwd: &Path,
    no_team_msg: &str,
) -> Result<(crate::team::TeamDef, Vec<crate::agents::AgentDef>), Vec<String>> {
    match load(session, cwd) {
        Ok(Some(x)) => Ok(x),
        Ok(None) => Err(vec![no_team_msg.to_string()]),
        Err(e) => Err(vec![format!("✗ {e}")]),
    }
}

fn branch(_session: &Arc<Session>, cwd: &Path) -> String {
    crate::team::current_branch(cwd)
}

/// Definitions section: team name + channel mode + members (role + source badge).
fn def_zone(def: &crate::team::TeamDef, defs: &[crate::agents::AgentDef]) -> Vec<String> {
    let view = crate::team::view(def, defs);
    let mode = crate::team::channel_mode(def).label();
    let mut out = vec![format!(
        "▸ {} · {} 成员 · {mode} 频道",
        view.def.name,
        view.members.len()
    )];
    for m in &view.members {
        let badge = match m.source {
            crate::agents::AgentDefSource::Project => "[项目]",
            crate::agents::AgentDefSource::User => "[用户]",
            crate::agents::AgentDefSource::Unknown => "",
        };
        out.push(format!(
            "  {} → {} {}（{}）",
            m.name, m.agent, badge, m.description
        ));
    }
    out
}

fn list(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let (def, defs) = match load_or_no_team(
        session,
        cwd,
        "没有 .bingo/team.json（team 未固定到本项目；/team new 创建）。",
    ) {
        Ok(x) => x,
        Err(out) => return out,
    };
    let mut out = def_zone(&def, &defs);
    out.push(String::new());
    // Runtime section: member instance states (not spawned = offline ○).
    let instances = session.agents.list();
    let running: Vec<&crate::agents::AgentStatus> = instances
        .iter()
        .filter(|a| def.members.iter().any(|m| m.name == a.name))
        .collect();
    if running.is_empty() {
        out.push("  运行区：未拉起（/team start 拉起）".to_string());
    } else {
        out.push(format!("  运行区（{} 实例）", running.len()));
        for a in &running {
            out.push(format!("  {} · {}", a.name, state_mark(a.state)));
        }
    }
    out
}

fn state_mark(state: AgentState) -> &'static str {
    match state {
        AgentState::Idle => "● 待命",
        AgentState::Running => "◐ 忙碌",
        AgentState::Stopped => "✗ 异常",
    }
}

fn start(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let (def, defs) = match load_or_no_team(
        session,
        cwd,
        "没有 .bingo/team.json（/team new 创建后 /team start）。",
    ) {
        Ok(x) => x,
        Err(out) => return out,
    };
    let branch = branch(session, cwd);
    match crate::team::spawn_team(session, &def, &defs, &session.home, cwd, &branch) {
        Ok(summary) => {
            let total = summary.spawned.len() + summary.reused.len() + summary.failed.len();
            let ready = total - summary.failed.len();
            let mut out = Vec::new();
            if !summary.spawned.is_empty() {
                let names = summary.spawned.join(" · ");
                out.push(format!(
                    "[team] {} 拉起 · {ready}/{total} 待命（{names}）",
                    def.name
                ));
            } else if !summary.reused.is_empty() {
                out.push(format!(
                    "[team] {} 已在运行 · 复用现有实例（{ready}/{total} 待命）",
                    def.name
                ));
            }
            if !summary.events().is_empty() {
                out.push(format!("[team] {}", summary.events().join(" · ")));
            }
            for (member, reason) in &summary.failed {
                out.push(format!(
                    "[team] ✗ {member} 拉起失败：{reason}（修复后 /team start 单独重试）"
                ));
            }
            if out.is_empty() {
                out.push(format!("[team] {} 无变化", def.name));
            }
            out.push("（/team status 查看 · /team assign <成员> <任务> 派活）".to_string());
            out
        }
        Err(e) => vec![format!(
            "[team] {} 校验失败：{e}（修复后重试；/team validate 预检）",
            def.name
        )],
    }
}

fn status(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let (def, _defs) = match load_or_no_team(
        session,
        cwd,
        "没有 .bingo/team.json（/team start 需先固定 team）。",
    ) {
        Ok(x) => x,
        Err(out) => return out,
    };
    let instances = session.agents.list();
    let mut out = vec![format!("▸ {} 状态", def.name)];
    for m in &def.members {
        let mark = match instances.iter().find(|a| a.name == m.name) {
            Some(a) => state_mark(a.state),
            None => "○ 离线",
        };
        out.push(format!("  {mark}  {}", m.name));
    }
    out
}

fn assign(session: &Arc<Session>, cwd: &Path, rest: String) -> Vec<String> {
    let (def, _defs) = match load_or_no_team(session, cwd, "没有 .bingo/team.json（team 未固定）。")
    {
        Ok(x) => x,
        Err(out) => return out,
    };
    let mut parts = rest.splitn(2, char::is_whitespace);
    let member = parts.next().unwrap_or("").trim();
    let message = parts.next().unwrap_or("").trim();
    if member.is_empty() || message.is_empty() {
        return vec!["用法: /team assign <成员> <任务>".to_string()];
    }
    if !def.members.iter().any(|m| m.name == member) {
        let known: Vec<&str> = def.members.iter().map(|m| m.name.as_str()).collect();
        return vec![format!(
            "{member} 不是 {} 的成员；成员：{}",
            def.name,
            known.join(", ")
        )];
    }
    match session.agents.deliver(member, message, Vec::new(), None) {
        Ok(_) => {
            // A slash command has no turn boundary behind it: deliver now, so the user sees the
            // assignment start instead of waiting for the hub's next turn.
            crate::tool::agent::flush_agent_inbox(session, &session.watch);
            // Dispatch audit: append-only decision record (zero model cost).
            crate::team::append_decision(
                &session.home,
                cwd,
                &branch(session, cwd),
                &def.name,
                "task",
                message,
                &[member],
            );
            vec![format!(
                "✓ 已派给 {member} · 完成后通知（/team status 查看状态）"
            )]
        }
        Err(e) => vec![format!("✗ 派发失败：{e}")],
    }
}

fn stop(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let (def, _defs) = match load_or_no_team(session, cwd, "没有 .bingo/team.json。") {
        Ok(x) => x,
        Err(out) => return out,
    };
    let mut stopped = Vec::new();
    for m in &def.members {
        if session.agents.stop(&m.name).is_ok() {
            stopped.push(m.name.clone());
        }
    }
    if stopped.is_empty() {
        vec![format!("[team] {} 没有运行中的成员", def.name)]
    } else {
        vec![format!(
            "[team] {} 已停止 · {} 成员（历史保留，/team start 可再拉起）",
            def.name,
            stopped.len()
        )]
    }
}

fn validate(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let (def, defs) = match load_or_no_team(
        session,
        cwd,
        "没有 .bingo/team.json（/team new 创建后 validate）。",
    ) {
        Ok(x) => x,
        Err(out) => return out,
    };
    match crate::team::validate(&def, &defs) {
        Ok(()) => {
            let mode = crate::team::channel_mode(&def).label();
            let limit = def
                .channel
                .as_ref()
                .and_then(|s| s.message_limit)
                .map(|l| format!(" · 预算 {l}"))
                .unwrap_or_default();
            vec![format!(
                "✓ team.json 通过校验（{} 成员 · {mode} 频道{limit}）",
                def.members.len()
            )]
        }
        Err(e) => vec![format!("✗ 校验失败：{e}")],
    }
}

/// Scaffolding: `/team new [name]` — generates .bingo/team.json with members = all
/// current AgentDefs (all references exist → the output naturally passes validate).
/// Refuses to overwrite an existing file.
fn new_team(session: &Arc<Session>, cwd: &Path, name: &str) -> Vec<String> {
    let path = cwd.join(crate::team::TEAM_FILE);
    if path.exists() {
        return vec![format!(
            "✗ {} 已存在（不覆盖；手改或删除后重来）",
            path.display()
        )];
    }
    let name = if name.trim().is_empty() {
        cwd.file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "team".to_string())
    } else {
        name.trim().to_string()
    };
    let defs = crate::agents::load_agent_defs(&session.home, cwd);
    if defs.is_empty() {
        return vec![
            "✗ 没有任何 AgentDef 可入队（先在 .bingo/agents/ 或 ~/.config/bingo/agents/ 建角色）。"
                .to_string(),
        ];
    }
    let def = crate::team::TeamDef {
        name,
        channel: Some(crate::team::ChannelSpec {
            mode: Some("serial".to_string()),
            message_limit: None,
        }),
        // Portraits handed out in roster order rather than by hashing the name:
        // a scaffolded crew should come out with distinct faces, and a hash of
        // four role names collides more often than not.
        members: defs
            .iter()
            .zip(crate::tui::avatar::ids().into_iter().cycle())
            .map(|(d, avatar)| crate::team::TeamMember {
                name: d.name.clone(),
                agent: d.name.clone(),
                avatar: Some(avatar.to_string()),
            })
            .collect(),
    };
    match crate::team::write_team_file(cwd, &def) {
        Ok(()) => vec![
            format!(
                "✓ 已生成 {}（{} 成员 · serial 频道）",
                path.display(),
                def.members.len()
            ),
            "  产物已通过校验（/team start 拉起 · 手动精简 members 后 /team validate 复查）"
                .to_string(),
        ],
        Err(e) => vec![format!("✗ 写入失败：{e}")],
    }
}

/// Memory subcommand: list shows this team's memory under the project + branch; gc cleans by TTL.
fn memory(session: &Arc<Session>, cwd: &Path, sub: &str) -> Vec<String> {
    let (def, _defs) = match load_or_no_team(
        session,
        cwd,
        "没有 .bingo/team.json（记忆按 team 命名空间存储）。",
    ) {
        Ok(x) => x,
        Err(out) => return out,
    };
    let branch = branch(session, cwd);
    let dir = crate::team::team_memory_dir(&session.home, cwd, &branch, &def.name);
    match sub {
        "" | "list" | "ls" => {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                return vec![format!(
                    "暂无 {} 的记忆（{} 分支下没有落盘内容）",
                    def.name, branch
                )];
            };
            let mut out = vec![format!(
                "▸ {} 记忆 · {} 分支 · {}",
                def.name,
                branch,
                dir.display()
            )];
            for e in entries.flatten() {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                out.push(format!(
                    "  {} · {} B",
                    e.file_name().to_string_lossy(),
                    size
                ));
            }
            out
        }
        "gc" => {
            const TTL_SECS: u64 = 30 * 24 * 3600;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                return vec!["没有可清理的记忆。".to_string()];
            };
            let mut removed = 0;
            for e in entries.flatten() {
                let stale = e
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| now.saturating_sub(d.as_secs()) > TTL_SECS)
                    .unwrap_or(false);
                if stale {
                    let _ = std::fs::remove_file(e.path());
                    removed += 1;
                }
            }
            if removed == 0 {
                vec![format!("{} 的记忆无过时文件（TTL 30 天）", def.name)]
            } else {
                vec![format!("✓ 已清理 {removed} 个过时记忆文件（TTL 30 天）")]
            }
        }
        other => vec![format!(
            "未知记忆子命令: /team memory {other}（可用 list / gc）"
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(name: &str) -> (Arc<Session>, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("bingo-teamcmd-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("proj");
        std::fs::create_dir_all(project.join(".bingo/agents")).unwrap();
        let s = Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "http://x".into()),
            runtime: crate::query::Runtime::new("m".into(), None, Default::default()),
            permission_mode: crate::permission::PermissionMode::Default,
            settings: crate::settings::Settings::default(),
            system: Vec::new(),
            depth: 0,
            home: root.join("home"),
            user_config_dir: root.join("home").join(".config"),
            quiet: true,
            compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&root, "t")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: crate::agents::AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(Default::default()),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
        });
        (s, project)
    }

    /// Scaffold output → validate passes → start doesn't fail on config (a three-step
    /// acceptance assertion chain run in one go).
    #[test]
    fn scaffold_validate_start_chain() {
        let (s, project) = session("scaffold");
        std::fs::write(project.join(".bingo/agents/qa.md"), "你是 QA。\n").unwrap();
        std::fs::write(project.join(".bingo/agents/dev.md"), "你是 Dev。\n").unwrap();

        // 1. /team new: generates the artifact.
        let out = new_team(&s, &project, "");
        assert!(out[0].contains("已生成"), "{out:?}");
        assert!(project.join(crate::team::TEAM_FILE).exists());

        // 2. validate passes.
        let out = validate(&s, &project);
        assert!(out[0].contains("通过校验"), "{out:?}");

        // 3. start doesn't fail on config; members standby + channel built.
        let out = start(&s, &project);
        assert!(!out.iter().any(|l| l.contains("失败")), "{out:?}");
        let instances = s.agents.list();
        assert_eq!(instances.len(), 2, "qa + dev 两个成员");
        assert!(instances.iter().all(|a| a.state == AgentState::Idle));
        assert!(
            s.channels.info("proj").is_some(),
            "频道 = team 名（缺省取目录名）"
        );
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    /// status shows offline marks; assign errors on non-members.
    #[test]
    fn status_and_assign() {
        let (s, project) = session("status");
        std::fs::write(project.join(".bingo/agents/qa.md"), "你是 QA。\n").unwrap();
        let _ = new_team(&s, &project, "qt");

        let out = status(&s, &project);
        assert!(
            out.iter().any(|l| l.contains("○ 离线")),
            "未拉起成员为离线: {out:?}"
        );

        let out = assign(&s, &project, "ghost 干点活".to_string());
        assert!(out[0].contains("不是 qt 的成员"), "{out:?}");
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    /// Parse errors are not silently swallowed: broken JSON surfaces an error line.
    #[test]
    fn broken_team_file_reports_error() {
        let (s, project) = session("broken");
        std::fs::create_dir_all(project.join(".bingo")).unwrap();
        std::fs::write(project.join(crate::team::TEAM_FILE), "{not json").unwrap();
        let out = list(&s, &project);
        assert!(out[0].contains("✗") && out[0].contains("解析"), "{out:?}");
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }
}

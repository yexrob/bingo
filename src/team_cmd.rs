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
            let mut out = vec![format!("unknown subcommand: /team {other}"), String::new()];
            out.extend(usage());
            out
        }
    }
}

fn usage() -> Vec<String> {
    vec![
        "usage: /team <list|start|status|assign|stop|validate|new|memory>".to_string(),
        "  list       definition zone (blueprint) + runtime zone (worksite) side by side"
            .to_string(),
        "  start      bring up the team (members on standby · idempotent reuse)".to_string(),
        "  status     member states (●standby ◐busy ✗failed ○offline)".to_string(),
        "  assign     assign a task to a member (/team assign <member> <task>)".to_string(),
        "  stop       stop the team (members stop receiving instructions)".to_string(),
        "  validate   validate team.json (same source as start)".to_string(),
        "  new        scaffold: generate .bingo/team.json (the output must pass validate)"
            .to_string(),
        "  memory     memory management (list to view / gc to clean)".to_string(),
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
        "▸ {} · {} members · {mode} channel",
        view.def.name,
        view.members.len()
    )];
    for m in &view.members {
        let badge = match m.source {
            crate::agents::AgentDefSource::Project => "[project]",
            crate::agents::AgentDefSource::User => "[user]",
            crate::agents::AgentDefSource::Unknown => "",
        };
        let engine = def
            .members
            .iter()
            .find(|p| p.name == m.name)
            .map(crate::team::engine_label)
            .unwrap_or_default();
        out.push(format!(
            "  {} → {} {}{engine} ({})",
            m.name, m.agent, badge, m.description
        ));
    }
    out
}

fn list(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let (def, defs) = match load_or_no_team(
        session,
        cwd,
        "no .bingo/team.json (the team is not pinned to this project; /team new creates it).",
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
        out.push("  runtime zone: not up (/team start to bring it up)".to_string());
    } else {
        out.push(format!("  runtime zone ({} instances)", running.len()));
        for a in &running {
            out.push(format!(
                "  {} · {} · {} @ {}",
                a.name,
                state_mark(a.state),
                a.model,
                a.provider
            ));
        }
    }
    out
}

fn state_mark(state: AgentState) -> &'static str {
    match state {
        AgentState::Idle => "● standby",
        AgentState::Running => "◐ busy",
        AgentState::Stopped => "✗ failed",
    }
}

fn start(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let (def, defs) = match load_or_no_team(
        session,
        cwd,
        "no .bingo/team.json (/team new first, then /team start).",
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
                    "[team] {} up · {ready}/{total} on standby ({names})",
                    def.name
                ));
            } else if !summary.reused.is_empty() {
                out.push(format!(
                    "[team] {} already running · reusing existing instances ({ready}/{total} on standby)",
                    def.name
                ));
            }
            if !summary.events().is_empty() {
                out.push(format!("[team] {}", summary.events().join(" · ")));
            }
            for (member, reason) in &summary.failed {
                out.push(format!(
                    "[team] ✗ failed to spawn {member}: {reason} (fix and /team start to retry it alone)"
                ));
            }
            if out.is_empty() {
                out.push(format!("[team] {} no change", def.name));
            }
            out.push(
                "(/team status to check · /team assign <member> <task> to delegate)".to_string(),
            );
            out
        }
        Err(e) => vec![format!(
            "[team] {} validation failed: {e} (fix and retry; /team validate to pre-check)",
            def.name
        )],
    }
}

fn status(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let (def, _defs) = match load_or_no_team(
        session,
        cwd,
        "no .bingo/team.json (the team must be pinned before /team start).",
    ) {
        Ok(x) => x,
        Err(out) => return out,
    };
    let instances = session.agents.list();
    let mut out = vec![format!("▸ {} status", def.name)];
    for m in &def.members {
        let mark = match instances.iter().find(|a| a.name == m.name) {
            Some(a) => state_mark(a.state),
            None => "○ offline",
        };
        out.push(format!("  {mark}  {}", m.name));
    }
    out
}

fn assign(session: &Arc<Session>, cwd: &Path, rest: String) -> Vec<String> {
    let (def, _defs) = match load_or_no_team(
        session,
        cwd,
        "no .bingo/team.json (the team is not pinned).",
    ) {
        Ok(x) => x,
        Err(out) => return out,
    };
    let mut parts = rest.splitn(2, char::is_whitespace);
    let member = parts.next().unwrap_or("").trim();
    let message = parts.next().unwrap_or("").trim();
    if member.is_empty() || message.is_empty() {
        return vec!["usage: /team assign <member> <task>".to_string()];
    }
    if !def.members.iter().any(|m| m.name == member) {
        let known: Vec<&str> = def.members.iter().map(|m| m.name.as_str()).collect();
        return vec![format!(
            "{member} is not a member of {}; members: {}",
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
                "✓ assigned to {member} · notified on completion (/team status to check)"
            )]
        }
        Err(e) => vec![format!("✗ assignment failed: {e}")],
    }
}

fn stop(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let (def, _defs) = match load_or_no_team(session, cwd, "no .bingo/team.json.") {
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
        vec![format!("[team] {} has no running members", def.name)]
    } else {
        vec![format!(
            "[team] {} stopped · {} members (history kept; /team start brings it back)",
            def.name,
            stopped.len()
        )]
    }
}

fn validate(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let (def, defs) = match load_or_no_team(
        session,
        cwd,
        "no .bingo/team.json (/team new first, then validate).",
    ) {
        Ok(x) => x,
        Err(out) => return out,
    };
    match crate::team::validate(&def, &defs, session) {
        Ok(()) => {
            let mode = crate::team::channel_mode(&def).label();
            let limit = def
                .channel
                .as_ref()
                .and_then(|s| s.message_limit)
                .map(|l| format!(" · budget {l}"))
                .unwrap_or_default();
            vec![format!(
                "✓ team.json passes validation ({} members · {mode} channel{limit})",
                def.members.len()
            )]
        }
        Err(e) => vec![format!("✗ validation failed: {e}")],
    }
}

/// The portraits a scaffolded crew may wear: every one except the two the hub
/// and the human already occupy in the transcript.
fn crew_portraits() -> Vec<&'static str> {
    let taken = [
        crate::tui::avatar::index_of(crate::channels::HUB_NAME),
        crate::tui::avatar::index_of(crate::channels::USER_NAME),
    ];
    crate::tui::avatar::ids()
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !taken.contains(i))
        .map(|(_, id)| id)
        .collect()
}

/// Scaffolding: `/team new [name]` — generates .bingo/team.json with members = all
/// current AgentDefs (all references exist → the output naturally passes validate).
/// Refuses to overwrite an existing file.
fn new_team(session: &Arc<Session>, cwd: &Path, name: &str) -> Vec<String> {
    let path = cwd.join(crate::team::TEAM_FILE);
    if path.exists() {
        return vec![format!(
            "✗ {} already exists (not overwritten; edit or delete it and start over)",
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
            "✗ no AgentDef available to enlist (create roles in .bingo/agents/ or ~/.config/bingo/agents/ first)."
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
        // four role names collides more often than not. The two the hub and the
        // human wear are dealt out of the deck (D50) — they are in every room the
        // crew appears in, so a member sharing one collides on sight. Derived
        // from the same hash they use rather than named here, so the reservation
        // follows if either the hash or the portrait list changes.
        members: defs
            .iter()
            .zip(crew_portraits().into_iter().cycle())
            .map(|(d, avatar)| crate::team::TeamMember {
                name: d.name.clone(),
                agent: d.name.clone(),
                avatar: Some(avatar.to_string()),
                // Scaffolding pins no engine: a member runs what its definition
                // (or the session) runs until someone decides otherwise.
                ..Default::default()
            })
            .collect(),
    };
    match crate::team::write_team_file(cwd, &def) {
        Ok(()) => vec![
            format!(
                "✓ generated {} ({} members · serial channel)",
                path.display(),
                def.members.len()
            ),
            "  output passes validation (/team start to bring up · /team validate to re-check after trimming members)"
                .to_string(),
        ],
        Err(e) => vec![format!("✗ write failed: {e}")],
    }
}

/// Memory subcommand: list shows this team's memory under the project + branch; gc cleans by TTL.
fn memory(session: &Arc<Session>, cwd: &Path, sub: &str) -> Vec<String> {
    let (def, _defs) = match load_or_no_team(
        session,
        cwd,
        "no .bingo/team.json (memory is namespaced per team).",
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
                    "no memory for {} yet (nothing persisted under the {} branch)",
                    def.name, branch
                )];
            };
            let mut out = vec![format!(
                "▸ {} memory · {} branch · {}",
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
                return vec!["no memory to clean.".to_string()];
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
                vec![format!(
                    "no stale memory files for {} (TTL 30 days)",
                    def.name
                )]
            } else {
                vec![format!(
                    "✓ cleaned {removed} stale memory files (TTL 30 days)"
                )]
            }
        }
        other => vec![format!(
            "unknown memory subcommand: /team memory {other} (available: list / gc)"
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
        std::fs::write(project.join(".bingo/agents/qa.md"), "You are QA.\n").unwrap();
        std::fs::write(project.join(".bingo/agents/dev.md"), "You are Dev.\n").unwrap();

        // 1. /team new: generates the artifact.
        let out = new_team(&s, &project, "");
        assert!(out[0].contains("generated"), "{out:?}");
        assert!(project.join(crate::team::TEAM_FILE).exists());

        // 2. validate passes.
        let out = validate(&s, &project);
        assert!(out[0].contains("passes validation"), "{out:?}");

        // 3. start doesn't fail on config; members standby + channel built.
        let out = start(&s, &project);
        assert!(!out.iter().any(|l| l.contains("failed")), "{out:?}");
        let instances = s.agents.list();
        assert_eq!(instances.len(), 2, "qa + dev, two members");
        assert!(instances.iter().all(|a| a.state == AgentState::Idle));
        assert!(
            s.channels.info("proj").is_some(),
            "channel = team name (defaults to the dir name)"
        );
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    /// A scaffolded crew never wears the hub's or the human's face: those two are
    /// in every room the crew shows up in, so a member sharing one collides on
    /// sight — the very confusion pinning a portrait exists to prevent.
    #[test]
    fn scaffolded_crew_avoids_the_hub_and_human_faces() {
        let (s, project) = session("faces");
        // More members than there are free portraits, so the deal wraps and any
        // reserved face would certainly surface.
        for i in 0..crate::tui::avatar::COUNT + 1 {
            std::fs::write(
                project.join(format!(".bingo/agents/a{i}.md")),
                "You are A.\n",
            )
            .unwrap();
        }
        let _ = new_team(&s, &project, "wide");
        let def = crate::team::load_team_file(&project).unwrap().unwrap();
        let taken = [
            crate::tui::avatar::index_of(crate::channels::HUB_NAME),
            crate::tui::avatar::index_of(crate::channels::USER_NAME),
        ];
        for m in &def.members {
            let id = m.avatar.as_deref().unwrap_or_default();
            let index = crate::tui::avatar::index_of_id(id)
                .unwrap_or_else(|| panic!("{} wears an unknown portrait {id:?}", m.name));
            assert!(
                !taken.contains(&index),
                "{} took a reserved face ({id})",
                m.name
            );
        }
        // The remaining portraits are all still dealt — reserving is not thinning.
        let dealt: std::collections::HashSet<&str> = def
            .members
            .iter()
            .filter_map(|m| m.avatar.as_deref())
            .collect();
        assert_eq!(
            dealt.len(),
            crate::tui::avatar::COUNT - taken.len(),
            "every unreserved portrait is used: {dealt:?}"
        );
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    /// status shows offline marks; assign errors on non-members.
    #[test]
    fn status_and_assign() {
        let (s, project) = session("status");
        std::fs::write(project.join(".bingo/agents/qa.md"), "You are QA.\n").unwrap();
        let _ = new_team(&s, &project, "qt");

        let out = status(&s, &project);
        assert!(
            out.iter().any(|l| l.contains("○ offline")),
            "unspawned members show as offline: {out:?}"
        );

        let out = assign(&s, &project, "ghost do some work".to_string());
        assert!(out[0].contains("is not a member of qt"), "{out:?}");
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    /// Parse errors are not silently swallowed: broken JSON surfaces an error line.
    #[test]
    fn broken_team_file_reports_error() {
        let (s, project) = session("broken");
        std::fs::create_dir_all(project.join(".bingo")).unwrap();
        std::fs::write(project.join(crate::team::TEAM_FILE), "{not json").unwrap();
        let out = list(&s, &project);
        assert!(
            out[0].contains("✗") && out[0].contains("parse failed"),
            "{out:?}"
        );
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }
}

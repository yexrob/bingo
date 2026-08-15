//! `/team` command family implementation (D31): list/start/status/assign/stop/validate/new + memory.
//! Purely functional: takes Session + cwd + args, returns output lines (chat.rs pushes after dispatch).
//! The three-part error format (file path + field path + expectation) is shared with spawn/validate.
//!
//! Every read and every action spans the whole tree (D54): a chart declared in one file is
//! one formation, so `status` shows all of it and `start` brings all of it up.

use std::path::Path;
use std::sync::Arc;

use crate::agents::AgentState;
use crate::query::Session;
use crate::team::{TeamNode, TeamTree};

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
        "norms" => norms(cwd),
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
        "usage: /team <list|start|status|assign|stop|validate|new|norms|memory>".to_string(),
        "  list       definition zone (blueprint) + runtime zone (worksite) side by side"
            .to_string(),
        "  start      bring up the team (members on standby · idempotent reuse)".to_string(),
        "  status     member states (●standby ◐busy ✗failed ○offline)".to_string(),
        "  assign     assign a task to a member (/team assign <member> <task>)".to_string(),
        "  stop       stop the team (members stop receiving instructions)".to_string(),
        "  validate   validate team.json (same source as start)".to_string(),
        "  new        scaffold: generate .bingo/team.json (the output must pass validate)"
            .to_string(),
        "  norms      the crew's working agreement (.bingo/team-norms.md)".to_string(),
        "  memory     memory management (list to view / gc to clean)".to_string(),
    ]
}

/// Load the whole org chart. Parse errors (invalid JSON, a broken reference, a name
/// used twice) → Err (not silently swallowed).
fn load(cwd: &Path) -> Result<Option<TeamTree>, String> {
    crate::team::load_team_tree(cwd).map_err(|e| e.to_string())
}

/// Convenience: both Ok(None) and Err become a "no team file" output (shared wording across read commands).
fn load_or_no_team(cwd: &Path, no_team_msg: &str) -> Result<TeamTree, Vec<String>> {
    match load(cwd) {
        Ok(Some(tree)) => Ok(tree),
        Ok(None) => Err(vec![no_team_msg.to_string()]),
        Err(e) => Err(vec![format!("✗ {e}")]),
    }
}

/// Indent by depth, so a chart reads as one.
fn indent(node: &TeamNode) -> String {
    "  ".repeat(node.depth)
}

/// A child team's directory as the root sees it (the root's own line names no path —
/// it is the directory the session is already in).
fn where_at(tree: &TeamTree, node: &TeamNode) -> String {
    if node.depth == 0 {
        return String::new();
    }
    let root = &tree.root().dir;
    let path = node
        .dir
        .strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| node.dir.display().to_string());
    format!(" · {path}")
}

/// Definitions section: every team in the chart, with its rooms and members
/// (role + source badge).
fn def_zone(session: &Arc<Session>, tree: &TeamTree) -> Vec<String> {
    let mut out = Vec::new();
    for node in tree.nodes() {
        let defs = crate::agents::load_agent_defs(&session.home, &node.dir);
        let view = crate::team::view(&node.def, &defs);
        let pad = indent(node);
        out.push(format!(
            "{pad}▸ {} · {} members{}",
            view.def.name,
            view.members.len(),
            where_at(tree, node)
        ));
        for room in crate::team::rooms(&node.def) {
            out.push(format!(
                "{pad}  #{} · {} · {}",
                room.name,
                room.mode.label(),
                room.members.join(", ")
            ));
        }
        for m in &view.members {
            let badge = match m.source {
                crate::agents::AgentDefSource::Project => "[project]",
                crate::agents::AgentDefSource::User => "[user]",
                crate::agents::AgentDefSource::Unknown => "",
            };
            let engine = node
                .def
                .members
                .iter()
                .find(|p| p.name == m.name)
                .map(crate::team::engine_label)
                .unwrap_or_default();
            out.push(format!(
                "{pad}  {} → {} {}{engine} ({})",
                m.name, m.agent, badge, m.description
            ));
        }
    }
    out
}

fn list(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let tree = match load_or_no_team(
        cwd,
        "no .bingo/team.json (the team is not pinned to this project; /team new creates it).",
    ) {
        Ok(x) => x,
        Err(out) => return out,
    };
    let mut out = def_zone(session, &tree);
    out.push(String::new());
    // Runtime section: member instance states (not spawned = offline ○).
    let instances = session.agents.list();
    let running: Vec<&crate::agents::AgentStatus> = instances
        .iter()
        .filter(|a| a.kind == crate::agents::AgentKind::Crew)
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
    // Hires are listed apart from the crew, never among it: they are not in the blueprint,
    // they leave when their task does, and a listing that mixed the two would be the exact
    // confusion the distinction exists to prevent (D53).
    let hires: Vec<&crate::agents::AgentStatus> = instances
        .iter()
        .filter(|a| a.kind == crate::agents::AgentKind::Hire)
        .collect();
    if !hires.is_empty() {
        out.push(String::new());
        out.push(format!(
            "  temporary hires ({}) · not in {} · released when their task is done",
            hires.len(),
            crate::team::TEAM_FILE
        ));
        for a in &hires {
            out.push(format!(
                "  {} · {} · {}",
                a.name,
                state_mark(a.state),
                a.description
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
    let tree = match load_or_no_team(
        cwd,
        "no .bingo/team.json (/team new first, then /team start).",
    ) {
        Ok(x) => x,
        Err(out) => return out,
    };
    let name = &tree.root().def.name;
    match crate::team::spawn_tree(session, &tree, &session.home) {
        Ok(summary) => {
            let ready = summary.ready();
            let total = ready + summary.failed.len();
            let scope = if tree.nodes().len() > 1 {
                format!(" across {} teams", tree.nodes().len())
            } else {
                String::new()
            };
            let mut out = Vec::new();
            if !summary.spawned.is_empty() {
                let names = summary.spawned.join(" · ");
                out.push(format!(
                    "[team] {name} up · {ready}/{total} on standby{scope} ({names})"
                ));
            } else if !summary.refreshed.is_empty() {
                // The headline says what actually happened: a start whose only effect was
                // re-reading definitions is not "already running, nothing to do".
                let names = summary.refreshed.join(" · ");
                out.push(format!(
                    "[team] {name} up to date · definitions re-read for {names} (history kept)"
                ));
            } else if !summary.reused.is_empty() {
                out.push(format!(
                    "[team] {name} already running · reusing existing instances ({ready}/{total} on standby{scope})"
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
                out.push(format!("[team] {name} no change"));
            }
            out.push(
                "(/team status to check · /team assign <member> <task> to delegate)".to_string(),
            );
            out
        }
        Err(e) => vec![format!(
            "[team] {name} validation failed: {e} (fix and retry; /team validate to pre-check)"
        )],
    }
}

fn status(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let tree = match load_or_no_team(
        cwd,
        "no .bingo/team.json (the team must be pinned before /team start).",
    ) {
        Ok(x) => x,
        Err(out) => return out,
    };
    let instances = session.agents.list();
    let mut out = Vec::new();
    for node in tree.nodes() {
        let pad = indent(node);
        out.push(format!(
            "{pad}▸ {} status{}",
            node.def.name,
            where_at(&tree, node)
        ));
        for m in &node.def.members {
            let mark = match instances.iter().find(|a| a.name == m.name) {
                Some(a) => state_mark(a.state),
                None => "○ offline",
            };
            out.push(format!("{pad}  {mark}  {}", m.name));
        }
    }
    out
}

fn assign(session: &Arc<Session>, cwd: &Path, rest: String) -> Vec<String> {
    let tree = match load_or_no_team(cwd, "no .bingo/team.json (the team is not pinned).") {
        Ok(x) => x,
        Err(out) => return out,
    };
    let mut parts = rest.splitn(2, char::is_whitespace);
    let member = parts.next().unwrap_or("").trim();
    let message = parts.next().unwrap_or("").trim();
    if member.is_empty() || message.is_empty() {
        return vec!["usage: /team assign <member> <task>".to_string()];
    }
    // Names are unique across the tree, so a bare name resolves from anywhere in it.
    let Some((node, _)) = tree.find_member(member) else {
        let known: Vec<&str> = tree.members().map(|(_, m)| m.name.as_str()).collect();
        return vec![format!(
            "{member} is not a member of {}; members: {}",
            tree.root().def.name,
            known.join(", ")
        )];
    };
    let (team, dir) = (node.def.name.clone(), node.dir.clone());
    // The human typed the assignment, so it arrives under their name, not main's.
    match session.agents.deliver(
        member,
        crate::channels::USER_NAME,
        message,
        Vec::new(),
        None,
    ) {
        Ok(_) => {
            // Use the same dispatcher as SendMessage so a slash-command assignment starts now.
            crate::tool::agent::flush_agent_inbox(session, &session.watch);
            // Dispatch audit: append-only decision record (zero model cost), filed with the
            // team the member actually belongs to.
            crate::team::append_decision(
                &session.home,
                &dir,
                &crate::team::current_branch(&dir),
                &team,
                "task",
                message,
                &[member],
            );
            vec![format!(
                "✓ assigned to {member} ({team}) · notified on completion (/team status to check)"
            )]
        }
        Err(e) => vec![format!("✗ assignment failed: {e}")],
    }
}

fn stop(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let tree = match load_or_no_team(cwd, "no .bingo/team.json.") {
        Ok(x) => x,
        Err(out) => return out,
    };
    let mut stopped = Vec::new();
    for (_, m) in tree.members() {
        if session.agents.stop(&m.name).is_ok() {
            stopped.push(m.name.clone());
        }
    }
    let name = &tree.root().def.name;
    if stopped.is_empty() {
        vec![format!("[team] {name} has no running members")]
    } else {
        vec![format!(
            "[team] {name} stopped · {} members (history kept; /team start brings it back)",
            stopped.len()
        )]
    }
}

fn validate(session: &Arc<Session>, cwd: &Path) -> Vec<String> {
    let tree = match load_or_no_team(cwd, "no .bingo/team.json (/team new first, then validate).") {
        Ok(x) => x,
        Err(out) => return out,
    };
    match crate::team::validate_tree(&tree, session, &session.home) {
        Ok(()) => {
            let members = tree.members().count();
            let rooms: Vec<String> = tree
                .rooms()
                .map(|(_, r)| format!("#{} ({})", r.name, r.mode.label()))
                .collect();
            vec![format!(
                "✓ team.json passes validation ({} team(s) · {members} members · {})",
                tree.nodes().len(),
                rooms.join(" · ")
            )]
        }
        Err(e) => vec![format!("✗ validation failed: {e}")],
    }
}

/// The portraits a scaffolded crew may wear: every one except the two the main
/// agent and the human already occupy in the transcript.
///
/// Filtered by the portrait each id *resolves to* rather than by its position in
/// the list — since D99 reserved main's face, `ids()` no longer starts at
/// portrait 0 and the two numbers are not the same.
fn crew_portraits() -> Vec<&'static str> {
    let taken = [
        crate::tui::avatar::index_of(crate::channels::MAIN_NAME),
        crate::tui::avatar::index_of(crate::channels::USER_NAME),
    ];
    crate::tui::avatar::ids()
        .into_iter()
        .filter(|id| {
            crate::tui::avatar::index_of_id(id).is_some_and(|index| !taken.contains(&index))
        })
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
        // Scaffolding writes one room and no children: a starter chart with an
        // invented org in it is one the user has to undo before they can use it.
        channels: Vec::new(),
        teams: Vec::new(),
        // Portraits handed out in roster order rather than by hashing the name:
        // a scaffolded crew should come out with distinct faces, and a hash of
        // four role names collides more often than not. The two main and the
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
        Ok(()) => {
            let mut out = vec![
                format!(
                    "✓ generated {} ({} members · serial channel)",
                    path.display(),
                    def.members.len()
                ),
                "  output passes validation (/team start to bring up · /team validate to re-check after trimming members)"
                    .to_string(),
            ];
            // A blueprint without an agreement is a crew with no shared rules, and an
            // agreement nobody scaffolds is a file nobody writes. An existing one is left
            // alone — the starter text is a starting point, not a reset.
            match crate::team::write_norms_template(cwd) {
                Ok(true) => out.push(format!(
                    "✓ generated {} (starter working agreement — edit it; every member carries it)",
                    cwd.join(crate::team::NORMS_FILE).display()
                )),
                Ok(false) => out.push(format!(
                    "  {} already exists · kept as it is (/team norms to read it)",
                    crate::team::NORMS_FILE
                )),
                Err(e) => out.push(format!("⚠ {} not written: {e}", crate::team::NORMS_FILE)),
            }
            out
        }
        Err(e) => vec![format!("✗ write failed: {e}")],
    }
}

/// `/team norms`: the working agreement as it is on disk, so the thing every member is
/// carrying can be read without leaving the session. No editing here — it is a project file
/// under version control, and an editor is where a file like that is changed.
///
/// Each team in the chart has its own agreement, carried by its own members, so every one
/// that exists is printed under the team it binds.
fn norms(cwd: &Path) -> Vec<String> {
    let Ok(Some(tree)) = crate::team::load_team_tree(cwd) else {
        return norms_of(cwd, None);
    };
    let mut out = Vec::new();
    for node in tree.nodes() {
        if crate::team::load_norms(&node.dir).is_none() {
            continue;
        }
        if !out.is_empty() {
            out.push(String::new());
        }
        out.extend(norms_of(&node.dir, Some(&node.def.name)));
    }
    if out.is_empty() {
        return norms_of(cwd, None);
    }
    out
}

/// One team's agreement, or the advice for a crew that has not written one.
fn norms_of(dir: &Path, team: Option<&str>) -> Vec<String> {
    let path = dir.join(crate::team::NORMS_FILE);
    let who = team.map(|t| format!(" ({t})")).unwrap_or_default();
    match crate::team::load_norms(dir) {
        Some(text) => {
            let mut out = vec![
                format!("▸ team norms{who} · {}", path.display()),
                "  carried by every member and every hire; a direct instruction outranks it"
                    .to_string(),
                String::new(),
            ];
            out.extend(text.lines().map(|l| format!("  {l}")));
            out
        }
        None => vec![
            format!(
                "no {} (this crew has no written agreement)",
                crate::team::NORMS_FILE
            ),
            "  /team new scaffolds one beside a fresh blueprint; or write the file yourself — \
             it is prose, read by members and reviewed by people."
                .to_string(),
        ],
    }
}

/// Memory subcommand: list shows each team's memory under its own project + branch;
/// gc cleans by TTL. Per team rather than per session, because that is how it is
/// stored — a department in another repo keeps its memory with that repo.
fn memory(session: &Arc<Session>, cwd: &Path, sub: &str) -> Vec<String> {
    let tree = match load_or_no_team(cwd, "no .bingo/team.json (memory is namespaced per team).") {
        Ok(x) => x,
        Err(out) => return out,
    };
    let dirs: Vec<(String, String, std::path::PathBuf)> = tree
        .nodes()
        .iter()
        .map(|n| {
            let branch = crate::team::current_branch(&n.dir);
            let dir = crate::team::team_memory_dir(&session.home, &n.dir, &branch, &n.def.name);
            (n.def.name.clone(), branch, dir)
        })
        .collect();
    match sub {
        "" | "list" | "ls" => {
            let mut out = Vec::new();
            for (team, branch, dir) in &dirs {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    out.push(format!(
                        "no memory for {team} yet (nothing persisted under the {branch} branch)"
                    ));
                    continue;
                };
                out.push(format!(
                    "▸ {team} memory · {branch} branch · {}",
                    dir.display()
                ));
                let mut files: Vec<(String, u64)> = entries
                    .flatten()
                    .map(|e| {
                        (
                            e.file_name().to_string_lossy().to_string(),
                            e.metadata().map(|m| m.len()).unwrap_or(0),
                        )
                    })
                    .collect();
                files.sort();
                for (name, size) in files {
                    out.push(format!("  {name} · {size} B"));
                }
            }
            // Nobody should have to read the code to learn that this is not loaded.
            out.push(
                "  (.md is the readable transcript, .json the exact record; members are told \
                 where these are, not given them — open one to read it yourself)"
                    .to_string(),
            );
            out
        }
        "gc" => {
            const TTL_SECS: u64 = 30 * 24 * 3600;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let mut removed = 0;
            for (_, _, dir) in &dirs {
                let Ok(entries) = std::fs::read_dir(dir) else {
                    continue;
                };
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
            }
            if removed == 0 {
                vec![format!(
                    "no stale memory files for {} (TTL 30 days)",
                    tree.root().def.name
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
            cwd: Arc::new(std::sync::Mutex::new(project.clone())),
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

    /// A scaffolded crew never wears main's or the human's face: those two are
    /// in every room the crew shows up in, so a member sharing one collides on
    /// sight — the very confusion pinning a portrait exists to prevent.
    #[test]
    fn scaffolded_crew_avoids_main_and_human_faces() {
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
            crate::tui::avatar::index_of(crate::channels::MAIN_NAME),
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

    /// A blueprint without an agreement is a crew with no shared rules, and an agreement
    /// nobody scaffolds is a file nobody writes — so `/team new` produces both, and `/team
    /// norms` reads back what every member is carrying.
    #[test]
    fn scaffolding_writes_the_agreement_and_norms_reads_it_back() {
        let (s, project) = session("norms");
        std::fs::write(project.join(".bingo/agents/qa.md"), "You are QA.\n").unwrap();

        let out = norms(&project);
        assert!(out[0].contains("no .bingo/team-norms.md"), "{out:?}");

        let out = new_team(&s, &project, "crew");
        assert!(
            out.iter().any(|l| l.contains("team-norms.md")),
            "the scaffold says it wrote the agreement: {out:?}"
        );
        assert!(project.join(crate::team::NORMS_FILE).exists());

        let out = norms(&project);
        assert!(out[0].contains("team norms"), "{out:?}");
        assert!(
            out.iter()
                .any(|l| l.contains("a direct instruction outranks it")),
            "the precedence rule is stated where it is read: {out:?}"
        );
        assert!(
            out.iter()
                .any(|l| l.contains("Report outcomes as they are")),
            "the file's own text is shown: {out:?}"
        );
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    /// Hires are listed apart from the crew, never among it (D53): the roster is the
    /// blueprint's, and someone hired for one task is not on it.
    #[test]
    fn list_separates_temporary_hires_from_the_crew() {
        let (s, project) = session("hires");
        std::fs::write(project.join(".bingo/agents/qa.md"), "You are QA.\n").unwrap();
        let _ = new_team(&s, &project, "crew");
        let _ = start(&s, &project);
        s.agents.insert(
            "temp",
            crate::agents::AgentKind::Hire,
            None,
            "one-off audit".into(),
            s.clone(),
        );

        let out = list(&s, &project);
        let text = out.join("\n");
        assert!(text.contains("runtime zone (1 instances)"), "{text}");
        assert!(
            text.contains("temporary hires (1)") && text.contains("one-off audit"),
            "a hire is shown, in its own section: {text}"
        );
        assert!(
            text.contains("released when their task is done"),
            "and the section says what makes it different: {text}"
        );
        // The blueprint section is the crew's alone.
        let roster_end = text.find("runtime zone").unwrap_or(text.len());
        assert!(!text[..roster_end].contains("temp"), "{text}");
        let _ = std::fs::remove_dir_all(project.parent().unwrap());
    }

    /// The issue's acceptance criterion, from the user's side: in one session at the
    /// project root, `/team status` shows both departments and the team under one of
    /// them, `/team start` brings the whole chart up at once, and running it again
    /// changes nothing.
    #[test]
    fn status_and_start_span_the_whole_chart() {
        let (s, project) = session("chart");
        let agent = |dir: &std::path::Path, name: &str| {
            std::fs::create_dir_all(dir.join(".bingo/agents")).unwrap_or_else(|e| panic!("{e}"));
            std::fs::write(
                dir.join(format!(".bingo/agents/{name}.md")),
                format!("---\ndescription: {name} description\n---\n\nYou are {name}.\n"),
            )
            .unwrap_or_else(|e| panic!("{e}"));
        };
        let blueprint = |dir: &std::path::Path, json: &str| {
            std::fs::create_dir_all(dir.join(".bingo")).unwrap_or_else(|e| panic!("{e}"));
            std::fs::write(dir.join(crate::team::TEAM_FILE), json)
                .unwrap_or_else(|e| panic!("{e}"));
        };

        agent(&project, "lead");
        blueprint(
            &project,
            r#"{"name":"hq","members":[{"name":"Wen","agent":"lead"}],
                "teams":[{"path":"repos/engineering"},{"name":"strategy","path":"strategy"}]}"#,
        );
        let engineering = project.join("repos/engineering");
        agent(&engineering, "dev");
        blueprint(
            &engineering,
            r#"{"name":"engineering","members":[{"name":"Linh","agent":"dev"},{"name":"Mira","agent":"dev"}],
                "channels":[{"name":"eng","members":["Linh","Mira"]},{"name":"oncall","mode":"free","members":["Mira","Kai"]}],
                "teams":[{"path":"platform"}]}"#,
        );
        let platform = engineering.join("platform");
        agent(&platform, "sre");
        blueprint(
            &platform,
            r#"{"name":"platform","members":[{"name":"Kai","agent":"sre"}]}"#,
        );
        let strategy = project.join("strategy");
        agent(&strategy, "analyst");
        blueprint(
            &strategy,
            r#"{"name":"strategy","members":[{"name":"Sora","agent":"analyst"}]}"#,
        );

        // validate first: one chart, one verdict.
        let out = validate(&s, &project).join("\n");
        assert!(
            out.contains("4 team(s)") && out.contains("5 members"),
            "{out}"
        );

        // status: both departments, and the team under one of them.
        let out = status(&s, &project).join("\n");
        for team in ["hq", "engineering", "platform", "strategy"] {
            assert!(out.contains(team), "{team} missing from status: {out}");
        }
        // Derived from the directory the fixture actually built, not written as a
        // literal: the line renders a real path, and its separator is the platform's.
        let expected = platform.strip_prefix(&project).unwrap_or(&platform);
        assert!(
            out.contains(&expected.display().to_string()),
            "a child team is shown where it lives: {out}"
        );
        assert_eq!(
            out.matches("○ offline").count(),
            5,
            "every member in the chart reads offline before start: {out}"
        );

        // start: the whole chart, at once.
        let out = start(&s, &project).join("\n");
        assert!(out.contains("across 4 teams"), "{out}");
        assert!(!out.contains("failed"), "{out}");
        assert_eq!(s.agents.list().len(), 5, "every member in the chart is up");
        for room in ["hq", "eng", "oncall", "platform", "strategy"] {
            assert!(s.channels.info(room).is_some(), "#{room} should exist");
        }
        assert!(
            s.channels.info("engineering").is_none(),
            "a team declaring its own rooms gets no room named after it"
        );
        assert_eq!(
            s.channels
                .info("oncall")
                .map(|c| c.members)
                .unwrap_or_default(),
            vec!["main", "user", "Mira", "Kai"],
            "a room may hold a member from the team below it"
        );

        // Idempotent: a second start reuses everything.
        let out = start(&s, &project).join("\n");
        assert!(out.contains("already running"), "{out}");
        assert_eq!(s.agents.list().len(), 5, "no duplicate instances");

        // Dispatch resolves a bare name against the whole chart, not just the root team:
        // the roster it offers when a name misses is every member under this session.
        let out = assign(&s, &project, "Nobody do it".to_string());
        assert!(out[0].contains("is not a member of hq"), "{out:?}");
        assert!(
            out[0].contains("Kai") && out[0].contains("Sora") && out[0].contains("Wen"),
            "the names it will accept span the chart: {out:?}"
        );

        // stop takes the whole chart down.
        let out = stop(&s, &project).join("\n");
        assert!(out.contains("5 members"), "{out}");
        let _ = std::fs::remove_dir_all(project.parent().unwrap_or(&project));
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

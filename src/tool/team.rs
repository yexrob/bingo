//! Team tool (D46): the agent's hand on the project crew, gated on the user's consent.
//!
//! The `/team` command family already gives the *user* the whole surface; this gives the
//! model the same one minus the parts it doesn't need (`assign` is SendMessage, which it
//! already has). What it deliberately does not give the model is autonomy: reads are free,
//! and every change — bringing the crew up, taking it down, rewriting the blueprint —
//! goes through [`Tool::confirm_reason`], the bypass-immune prompt. Permission *mode* is
//! the wrong lever here: a crew is a standing commitment of the user's tokens and of a
//! committed project file, so "the user is in bypass mode" is not evidence that they meant
//! to hire three agents.
//!
//! `save` is whole-document, matching the file it writes: the blueprint has one shape on
//! disk and one in the tool call, and a partial-merge dialect would be a second format to
//! keep honest. The confirmation line carries the delta so the user reviews the change,
//! not the document. The one thing it does not rewrite is the org chart (`teams`): child
//! teams live in other directories, are set up by hand, and a roster edit is no reason to
//! re-decide them — they are carried across every save unchanged (D54).
//!
//! Reads and actions span the whole tree: `status` shows every team under this one,
//! `start` brings all of them up, `stop` takes all of them down.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::agents::{AgentDef, AgentDefSource};
use crate::query::Session;
use crate::team::{ChannelDef, ChannelSpec, TeamDef, TeamMember, TeamTree};
use crate::tool::{Tool, ToolContext, ToolError, ToolResult, parse_input};
use crate::watch::WatchState;

/// Names listed in full in one confirmation line before it collapses into a count.
const MAX_NAMES_IN_REASON: usize = 4;

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum TeamAction {
    /// Blueprint + room in one read: the roster, each member's runtime state, and the
    /// agent definitions available to draft with.
    Status,
    /// Check `.bingo/team.json` against the agent definitions on disk.
    Validate,
    /// Bring the crew up: create the channel and spawn every member (idempotent).
    Start,
    /// Take the crew down; histories are kept and `start` brings them back.
    Stop,
    /// Write the blueprint to `.bingo/team.json` (replaces the whole document).
    Save,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct MemberInput {
    #[schemars(
        description = "Instance name in the room — how you address this member with SendMessage, and the name shown on its messages, so give it a person's name rather than a role code or a number (\"Linh\", \"Mira\", not \"dev\" or \"member-2\"); unique within the team"
    )]
    name: String,
    #[schemars(
        description = "Agent definition the member plays: a name from status's \"available agent definitions\" (.bingo/agents/<name>.md or ~/.config/bingo/agents/<name>.md)"
    )]
    agent: String,
    #[serde(default)]
    #[schemars(
        description = "Portrait this member wears, pinned so it survives across sessions: one of the ids status lists. Give every member a different one; omitted means a portrait picked from the name, which can collide"
    )]
    avatar: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Model this member runs on. Omit to use the agent definition's model, falling back to the session's — set it only when this member's job wants a different engine from the rest of the crew (a cheap fast one for a reviewer, a stronger one for a designer)"
    )]
    model: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Provider endpoint this member runs against: a name from the configured providers. Omit to share the session's endpoint and follow its switches. A named provider other than the session's own requires model as well, since a model name means nothing at another endpoint"
    )]
    provider: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Thinking level for this member: off/low/medium/high/xhigh/max. Omit to use the agent definition's, falling back to the session's current level"
    )]
    thinking: Option<String>,
}

#[derive(Debug, Default, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ChannelInput {
    #[schemars(
        description = "Room name — how it is addressed as #name; unique across the whole team tree"
    )]
    name: String,
    #[serde(default)]
    #[schemars(
        description = "Speaking mode: serial (a member must have read the latest message before speaking; default) or free (interleaving allowed)"
    )]
    mode: Option<String>,
    #[serde(default)]
    #[schemars(description = "The room freezes past this many messages (default 500)")]
    message_limit: Option<u64>,
    #[serde(default)]
    #[schemars(
        description = "Who is in this room: member names from this team or from a team below it in the tree. Omit to put the whole team in"
    )]
    members: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TeamInput {
    #[schemars(
        description = "read the crew / check the config / bring it up / take it down / write the blueprint"
    )]
    action: TeamAction,
    #[serde(default)]
    #[schemars(
        description = "Team name (save only): also the name of its one room, when the team declares no channels of its own"
    )]
    name: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Complete member roster (save only): whoever is missing here is removed from the blueprint, so send the full crew you want, not the delta"
    )]
    members: Option<Vec<MemberInput>>,
    #[serde(default)]
    #[schemars(
        description = "The team's rooms (save only): a team may hold several, each with its own roster, so the same person is in some and not others. Sending this replaces every room the team has. Omit to keep the rooms it already has — and when it has none, the team gets one room named after it holding everybody"
    )]
    channels: Option<Vec<ChannelInput>>,
    #[serde(default)]
    #[schemars(
        description = "Channel mode (save only) for the one room a team with no declared channels gets: serial (default) or free"
    )]
    mode: Option<String>,
    #[serde(default)]
    #[schemars(
        description = "Channel message budget (save only) for that same one room: it freezes past this many messages (default 500)"
    )]
    message_limit: Option<u64>,
}

/// Project team management (main session only).
pub struct TeamTool {
    session: Arc<Session>,
}

impl TeamTool {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }
}

/// `a, b, c, and 5 total` — a confirmation line names the crew, and stops naming it once the
/// line would be longer than the decision it describes.
fn join_names(names: &[String], sep: &str) -> String {
    if names.len() <= MAX_NAMES_IN_REASON {
        return names.join(sep);
    }
    let head = names[..MAX_NAMES_IN_REASON - 1].join(sep);
    format!("{head}{sep}and {} total", names.len())
}

/// Every member in the tree, in chart order — what `start` and `stop` actually act on.
fn member_names(tree: &TeamTree) -> Vec<String> {
    tree.members().map(|(_, m)| m.name.clone()).collect()
}

/// `· across 3 teams` when the blueprint roots a chart, nothing when it is one team.
fn scope_of(tree: &TeamTree) -> String {
    match tree.nodes().len() {
        0 | 1 => String::new(),
        n => format!(" across {n} teams"),
    }
}

/// An optional field as the blueprint should hold it: whitespace-only is the same
/// as absent, so a field the model filled with a blank never reaches the file and
/// starts reading as an override that overrides nothing.
fn trimmed(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Blueprint on disk, or None when there is no team file / it is unreadable. The
/// confirmation path never reports parse errors: it decides *whether to ask*, and a broken
/// file is a reason to ask with less detail, not a reason to skip the question.
fn blueprint(project: &Path) -> Option<TeamDef> {
    crate::team::load_team_file(project).ok().flatten()
}

/// The org chart on disk, on the same terms: a chart that will not load is still a
/// reason to ask, just with less to say about it.
fn chart(project: &Path) -> Option<TeamTree> {
    crate::team::load_team_tree(project).ok().flatten()
}

fn start_reason(project: &Path) -> String {
    match chart(project) {
        Some(tree) => {
            let names = member_names(&tree);
            let rooms: Vec<String> = tree.rooms().map(|(_, r)| format!("#{}", r.name)).collect();
            format!(
                "Start {} · {} member(s) ({}){} into {}",
                tree.root().def.name,
                names.len(),
                join_names(&names, ", "),
                scope_of(&tree),
                join_names(&rooms, ", ")
            )
        }
        None => "Start this project's team (.bingo/team.json)".to_string(),
    }
}

fn stop_reason(project: &Path) -> String {
    match chart(project) {
        Some(tree) => format!(
            "Stop {} · {} member(s){} (history kept, can start again)",
            tree.root().def.name,
            member_names(&tree).len(),
            scope_of(&tree)
        ),
        None => "Stop this project's team (.bingo/team.json)".to_string(),
    }
}

/// What `save` changes, in one line: the crew's size plus the members entering and leaving.
/// The document itself is not shown — the decision is the delta.
fn save_reason(project: &Path, params: &TeamInput) -> String {
    let name = params.name.as_deref().unwrap_or("").trim();
    let wanted: Vec<String> = params
        .members
        .iter()
        .flatten()
        .map(|m| m.name.clone())
        .collect();
    let Some(old) = blueprint(project) else {
        return format!(
            "Create .bingo/team.json · {name} · {} member(s) ({})",
            wanted.len(),
            join_names(&wanted, ", ")
        );
    };
    let before: Vec<String> = old.members.iter().map(|m| m.name.clone()).collect();
    let delta: Vec<String> = before
        .iter()
        .filter(|n| !wanted.contains(n))
        .map(|n| format!("-{n}"))
        .chain(
            wanted
                .iter()
                .filter(|n| !before.contains(n))
                .map(|n| format!("+{n}")),
        )
        .collect();
    let change = if delta.is_empty() {
        "members unchanged".to_string()
    } else {
        join_names(&delta, " ")
    };
    let renamed = if old.name == name {
        String::new()
    } else {
        format!(" · team name {} → {name}", old.name)
    };
    // A whole-document write also carries what it leaves out: an omitted mode reverts the
    // one-room shorthand to serial, which is a change the user is agreeing to and must
    // therefore read. It is only reported for a team that has that room to revert — one
    // declaring its own channels does not have a `mode` to lose.
    let remode = if old.channels.is_empty() && params.channels.is_none() {
        let old_mode = crate::team::channel_mode(&old).label();
        let new_mode = params.mode.as_deref().unwrap_or("serial");
        if old_mode == new_mode {
            String::new()
        } else {
            format!(" · channel {old_mode} → {new_mode}")
        }
    } else {
        String::new()
    };
    let rerooms = match &params.channels {
        None => String::new(),
        Some(rooms) => {
            let before: Vec<String> = crate::team::rooms(&old)
                .into_iter()
                .map(|r| r.name)
                .collect();
            let after: Vec<String> = rooms.iter().map(|c| c.name.trim().to_string()).collect();
            if before == after {
                format!(" · {} room(s) unchanged", after.len())
            } else {
                format!(
                    " · rooms {} → {}",
                    join_names(&before, ", "),
                    join_names(&after, ", ")
                )
            }
        }
    };
    // The org chart is carried, never rewritten here — say so, so its absence from the
    // roster the user is reading is not mistaken for its removal.
    let kept = if old.teams.is_empty() {
        String::new()
    } else {
        format!(" · {} child team(s) kept", old.teams.len())
    };
    format!(
        "Rewrite .bingo/team.json · {name} · {} member(s) ({change}){renamed}{remode}{rerooms}{kept}",
        wanted.len()
    )
}

fn source_badge(source: AgentDefSource) -> &'static str {
    match source {
        AgentDefSource::Project => " [project]",
        AgentDefSource::User => " [user]",
        AgentDefSource::Unknown => "",
    }
}

/// The portraits a blueprint may pin, so the model drafts with real ids rather
/// than inventing one.
fn available_portraits() -> String {
    format!(
        "available portraits: {} (pin one per member so the crew has distinct faces)",
        crate::tui::avatar::ids().join(", ")
    )
}

fn available_defs(defs: &[AgentDef]) -> String {
    if defs.is_empty() {
        return "available agent definitions: none — write .bingo/agents/<name>.md first (frontmatter description + the persona as the body)".to_string();
    }
    let list: Vec<String> = defs
        .iter()
        .map(|d| format!("{}{}", d.name, source_badge(d.source)))
        .collect();
    format!("available agent definitions: {}", list.join(", "))
}

impl TeamTool {
    fn tree(&self, cwd: &Path) -> Result<Option<TeamTree>, ToolError> {
        crate::team::load_team_tree(cwd).map_err(|e| ToolError::failed(e.to_string()))
    }

    /// Blueprint + rooms + runtime in one read, over the whole chart. The runtime half
    /// comes from the registry, so a member that was never spawned (or was deleted out
    /// from under the blueprint) reads `offline` rather than going missing.
    fn status(&self, cwd: &Path, defs: &[AgentDef]) -> Result<String, ToolError> {
        let Some(tree) = self.tree(cwd)? else {
            return Ok(format!(
                "no .bingo/team.json in this project — no crew is pinned here.\n{}\n{}\n\
                 Draft one with action=save (the user confirms it before anything is written); \
                 give each member a person's name, not a role code.",
                available_defs(defs),
                available_portraits()
            ));
        };
        let instances = self.session.agents.list();
        let mut out = Vec::new();
        for node in tree.nodes() {
            let def = &node.def;
            let node_defs = crate::agents::load_agent_defs(&self.session.home, &node.dir);
            let view = crate::team::view(def, &node_defs);
            let norms = match crate::team::load_norms(&node.dir) {
                Some(_) => format!(
                    " · working agreement in {} (every member and every hire carries it)",
                    crate::team::NORMS_FILE
                ),
                None => String::new(),
            };
            let rooms: Vec<String> = crate::team::rooms(def)
                .into_iter()
                .map(|r| {
                    let limit = r
                        .message_limit
                        .map(|l| format!(", limit {l}"))
                        .unwrap_or_default();
                    format!(
                        "#{} ({}{limit}): {}",
                        r.name,
                        r.mode.label(),
                        r.members.join(", ")
                    )
                })
                .collect();
            let at = if node.depth == 0 {
                String::new()
            } else {
                format!(" · rooted at {}", node.dir.display())
            };
            out.push(format!(
                "team {} · {} members{at}{norms}\nrooms: {}",
                def.name,
                def.members.len(),
                rooms.join(" · ")
            ));
            let pinned: std::collections::HashMap<&str, &TeamMember> =
                def.members.iter().map(|m| (m.name.as_str(), m)).collect();
            for m in &view.members {
                let state = instances
                    .iter()
                    .find(|a| a.name == m.name)
                    .map(|a| a.state.label())
                    .unwrap_or("offline");
                let member = pinned.get(m.name.as_str());
                let face = member
                    .and_then(|p| p.avatar.as_deref())
                    .map(|a| format!(" · portrait {a}"))
                    .unwrap_or_default();
                // Only what the blueprint pins is reported. An inherited engine is not
                // named here on purpose: it is whatever the session is on at spawn, so
                // printing today's value would read as a pin the file does not hold.
                let engine = member
                    .map(|p| crate::team::engine_label(p))
                    .unwrap_or_default();
                out.push(format!(
                    "- {} -> agent \"{}\"{} · {state}{face}{engine} · {}",
                    m.name,
                    m.agent,
                    source_badge(m.source),
                    m.description
                ));
            }
        }
        if tree.nodes().len() > 1 {
            out.push(format!(
                "{} teams in this chart; every member is addressed by its bare name with SendMessage, from here, with no team prefix.",
                tree.nodes().len()
            ));
        }
        // Hires listed apart from the crew: they are not in the blueprint, they leave when
        // their task does, and reading them as members is what this section prevents (D53).
        let hires: Vec<&crate::agents::AgentStatus> = instances
            .iter()
            .filter(|a| a.kind == crate::agents::AgentKind::Hire)
            .collect();
        if !hires.is_empty() {
            out.push(format!(
                "temporary hires ({}) — not in {}, released once their task is done:",
                hires.len(),
                crate::team::TEAM_FILE
            ));
            for a in hires {
                out.push(format!(
                    "- {} · {} · {}",
                    a.name,
                    a.state.label(),
                    a.description
                ));
            }
        }
        out.push(available_defs(defs));
        out.push(available_portraits());
        Ok(out.join("\n"))
    }

    fn validate(&self, cwd: &Path) -> Result<String, ToolError> {
        let Some(tree) = self.tree(cwd)? else {
            return Ok("no .bingo/team.json in this project — nothing to validate.".to_string());
        };
        match crate::team::validate_tree(&tree, &self.session, &self.session.home) {
            Ok(()) => {
                let rooms: Vec<String> = tree
                    .rooms()
                    .map(|(_, r)| format!("#{} ({})", r.name, r.mode.label()))
                    .collect();
                Ok(format!(
                    "ok: .bingo/team.json passes ({} team(s) · {} members · {}). action=start brings it up.",
                    tree.nodes().len(),
                    tree.members().count(),
                    rooms.join(" · ")
                ))
            }
            Err(e) => Err(ToolError::failed(e.to_string())),
        }
    }

    fn start(&self, cwd: &Path) -> Result<String, ToolError> {
        let Some(tree) = self.tree(cwd)? else {
            return Err(ToolError::failed(
                "no .bingo/team.json in this project — write a blueprint with action=save first",
            ));
        };
        let summary = crate::team::spawn_tree(&self.session, &tree, &self.session.home)
            .map_err(|e| ToolError::failed(e.to_string()))?;
        let rooms: Vec<String> = tree.rooms().map(|(_, r)| format!("#{}", r.name)).collect();
        let mut out = Vec::new();
        if !summary.spawned.is_empty() {
            out.push(format!(
                "spawned {} into {}: {}",
                summary.spawned.len(),
                rooms.join(", "),
                summary.spawned.join(", ")
            ));
        }
        if !summary.reused.is_empty() {
            out.push(format!(
                "reused {} already running: {}",
                summary.reused.len(),
                summary.reused.join(", ")
            ));
        }
        for (member, reason) in &summary.failed {
            out.push(format!(
                "failed {member}: {reason} (fix it, then action=start again — the rest keep running)"
            ));
        }
        out.push(
            "Members stand by idle at zero tokens; a member only runs when you SendMessage it or \
             it is addressed in the channel. Cross-session memory for this branch is restored on spawn."
                .to_string(),
        );
        Ok(out.join("\n"))
    }

    fn stop(&self, cwd: &Path, ctx: &ToolContext) -> Result<String, ToolError> {
        let Some(tree) = self.tree(cwd)? else {
            return Err(ToolError::failed(
                "no .bingo/team.json in this project — nothing to stop",
            ));
        };
        let mut stopped = Vec::new();
        for (_, m) in tree.members() {
            let Ok((watch, _dropped)) = self.session.agents.stop(&m.name) else {
                continue;
            };
            if let Some(id) = watch {
                ctx.watch
                    .set_state(id, WatchState::Cancelled, Some("stopped".to_string()), None);
            }
            stopped.push(m.name.clone());
        }
        let name = &tree.root().def.name;
        if stopped.is_empty() {
            return Ok(format!("{name}: no member is spawned — nothing to stop."));
        }
        Ok(format!(
            "stopped {} of {name}{}: {}. Histories are kept — action=start brings them back where they left off.",
            stopped.len(),
            scope_of(&tree),
            stopped.join(", ")
        ))
    }

    fn save(&self, cwd: &Path, defs: &[AgentDef], params: &TeamInput) -> Result<String, ToolError> {
        let name = params
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .ok_or_else(|| {
                ToolError::failed("save needs name (the team, and its channel, name)")
            })?;
        let members = params
            .members
            .as_ref()
            .filter(|m| !m.is_empty())
            .ok_or_else(|| {
                ToolError::failed(
                    "save needs members: the complete roster, not just the members you are adding",
                )
            })?;
        if let Some(mode) = &params.mode {
            crate::channels::ChannelMode::parse(mode).map_err(ToolError::failed)?;
        }
        let old = blueprint(cwd);
        let had_rooms = old.as_ref().is_some_and(|d| !d.channels.is_empty());
        // Rooms come from the call, or are carried from disk. Configuring the one-room
        // shorthand on a team that declares its own rooms would silently delete them, so
        // it is refused rather than guessed at.
        let (channel, channels) = match &params.channels {
            Some(rooms) => (
                None,
                rooms
                    .iter()
                    .map(|c| ChannelDef {
                        name: c.name.trim().to_string(),
                        mode: trimmed(&c.mode),
                        message_limit: c.message_limit,
                        members: c
                            .members
                            .as_ref()
                            .map(|m| m.iter().map(|n| n.trim().to_string()).collect::<Vec<_>>()),
                    })
                    .collect(),
            ),
            None if had_rooms => {
                if params.mode.is_some() || params.message_limit.is_some() {
                    return Err(ToolError::failed(format!(
                        "{name} declares its own rooms in `channels`, so it has no single channel for mode/message_limit to configure — send the complete `channels` list to change them, or drop those two fields to keep the rooms as they are",
                    )));
                }
                (
                    None,
                    old.as_ref().map(|d| d.channels.clone()).unwrap_or_default(),
                )
            }
            None => (
                (params.mode.is_some() || params.message_limit.is_some()).then(|| ChannelSpec {
                    mode: params.mode.clone(),
                    message_limit: params.message_limit,
                }),
                Vec::new(),
            ),
        };
        let def = TeamDef {
            name: name.to_string(),
            channel,
            channels,
            members: members
                .iter()
                .map(|m| TeamMember {
                    name: m.name.trim().to_string(),
                    agent: m.agent.trim().to_string(),
                    avatar: trimmed(&m.avatar),
                    model: trimmed(&m.model),
                    provider: trimmed(&m.provider),
                    thinking: trimmed(&m.thinking),
                })
                .collect(),
            // The org chart is the user's: set up by hand across directories, pointing at
            // other repos, and no business of a roster edit. It travels every save intact.
            teams: old.map(|d| d.teams).unwrap_or_default(),
        };
        // Reference check before the write: `validate` and `start` share one source, so a
        // blueprint that lands on disk is one that can be brought up. The chart it would
        // root is checked with it — a rewrite that breaks a child's room is refused rather
        // than persisted and then complained about.
        crate::team::build_tree(def.clone(), cwd).map_err(|e| ToolError::failed(e.to_string()))?;
        crate::team::validate(&def, defs, &self.session)
            .map_err(|e| ToolError::failed(e.to_string()))?;
        crate::team::write_team_file(cwd, &def).map_err(|e| ToolError::failed(e.to_string()))?;
        let rooms: Vec<String> = crate::team::rooms(&def)
            .into_iter()
            .map(|r| format!("#{} ({})", r.name, r.mode.label()))
            .collect();
        let kept = if def.teams.is_empty() {
            String::new()
        } else {
            format!(
                "\n{} child team(s) in this chart are unchanged — edit {} in the other directories to change the org chart.",
                def.teams.len(),
                crate::team::TEAM_FILE
            )
        };
        Ok(format!(
            "wrote {} — {name} · {} members ({}) · rooms {}. It passes validation; action=start brings it up.\n\
             Running instances are untouched: members you dropped keep running until AgentControl stops them.{kept}",
            crate::team::TEAM_FILE,
            def.members.len(),
            def.members
                .iter()
                .map(|m| format!("{} -> {}", m.name, m.agent))
                .collect::<Vec<_>>()
                .join(", "),
            rooms.join(", ")
        ))
    }
}

#[async_trait]
impl Tool for TeamTool {
    fn name(&self) -> String {
        "Team".to_string()
    }

    fn description(&self) -> String {
        "Manage the project's agent team — the crew pinned to this repo in .bingo/team.json, whose members are \
         named instances of agent definitions talking in named rooms. A blueprint may declare child teams in \
         other directories, so one session manages a whole org chart; every action here spans it. status reads \
         every team's blueprint, rooms, member runtime states and the definitions available to draft with; \
         validate checks the chart against them; start opens the rooms and spawns every member in the tree \
         (idempotent — members already up are reused, and each one restores its memory for its own team's \
         directory and branch); stop takes them down keeping their histories; save writes this directory's \
         blueprint (whole document: send the complete roster, since whoever you leave out is removed — child \
         teams are the exception and are always carried unchanged). Every action except status and validate is a \
         change the user confirms in person before it happens, so propose it in your reply first and say why — \
         do not call save/start/stop speculatively or in a loop. Spawning is not waking: members stand by at \
         zero tokens until you SendMessage one by its bare name, from anywhere in the chart (that is how you \
         give a member work — this tool does not dispatch tasks) or they are addressed in a room."
            .to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<TeamInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }

    fn is_read_only(&self, input: &serde_json::Value) -> bool {
        matches!(
            input.get("action").and_then(|a| a.as_str()),
            Some("status") | Some("validate")
        )
    }

    fn confirm_reason(&self, input: &serde_json::Value) -> Option<String> {
        // Unparseable input can't reach `call` either (it parses first), so there is nothing
        // to guard yet — let the ordinary gate handle it and the call fail on its own terms.
        let params: TeamInput = parse_input(input).ok()?;
        let project = self.session.cwd();
        match params.action {
            TeamAction::Status | TeamAction::Validate => None,
            TeamAction::Start => Some(start_reason(&project)),
            TeamAction::Stop => Some(stop_reason(&project)),
            TeamAction::Save => Some(save_reason(&project, &params)),
        }
    }

    async fn call(
        &self,
        input: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: TeamInput = parse_input(&input)?;
        let cwd = ctx.cwd.clone();
        let defs = crate::agents::load_agent_defs(&self.session.home, &cwd);
        let text = match params.action {
            TeamAction::Status => self.status(&cwd, &defs)?,
            TeamAction::Validate => self.validate(&cwd)?,
            TeamAction::Start => self.start(&cwd)?,
            TeamAction::Stop => self.stop(&cwd, ctx)?,
            TeamAction::Save => self.save(&cwd, &defs, &params)?,
        };
        Ok(ToolResult {
            content: serde_json::Value::String(text),
            is_error: false,
            diff: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::{PermissionBehavior, PermissionMode, can_use_tool};

    /// A session rooted in a scratch project directory (the tool reads `.bingo/` from the
    /// context cwd, so the test drives it through a real one).
    fn fixture(name: &str) -> (Arc<Session>, std::path::PathBuf) {
        let root =
            std::env::temp_dir().join(format!("bingo-teamtool-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let project = root.join("proj");
        std::fs::create_dir_all(project.join(".bingo/agents")).unwrap_or_default();
        let session = Arc::new(Session {
            client: crate::api::client::Client::new("k".into(), "http://x".into()),
            runtime: crate::query::Runtime::new("m".into(), None, Default::default()),
            permission_mode: PermissionMode::Default,
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
        (session, project)
    }

    fn ctx(session: &Arc<Session>, cwd: &Path) -> ToolContext {
        ToolContext {
            cwd: cwd.to_path_buf(),
            home: session.home.clone(),
            watch: session.watch.clone(),
            http: reqwest::Client::new(),
            tasks: session.tasks.clone(),
            hooks: Default::default(),
            permission_mode: "default".into(),
            expand_tasks: tokio::sync::watch::channel(false).0,
            ask_question: Arc::new(|_t, _q, _o| Box::pin(async { None })),
            instance: None,
        }
    }

    async fn call(tool: &TeamTool, ctx: &ToolContext, input: serde_json::Value) -> String {
        match tool.call(input, ctx).await {
            Ok(result) => result.content.as_str().unwrap_or_default().to_string(),
            Err(e) => panic!("{e}"),
        }
    }

    fn decide(tool: &TeamTool, input: serde_json::Value, mode: PermissionMode) -> PermissionResult {
        can_use_tool(
            tool as &dyn Tool,
            &input,
            mode,
            &[],
            &[],
            &[],
            &std::env::temp_dir(),
        )
    }

    use crate::permission::PermissionResult;

    /// The whole loop the model runs: draft a blueprint, check it, bring the crew up.
    /// save's artifact must pass validate, and start must not fail on config — the same
    /// acceptance chain `/team new → validate → start` asserts, now from the tool side.
    #[tokio::test]
    async fn save_validate_start_chain() {
        let (session, project) = fixture("chain");
        std::fs::write(project.join(".bingo/agents/qa.md"), "You are QA.\n").unwrap_or_default();
        std::fs::write(project.join(".bingo/agents/dev.md"), "You are Dev.\n").unwrap_or_default();
        let tool = TeamTool::new(session.clone());
        let ctx = ctx(&session, &project);

        let out = call(&tool, &ctx, serde_json::json!({"action": "status"})).await;
        assert!(out.contains("no .bingo/team.json"), "{out}");
        assert!(
            out.contains("qa") && out.contains("dev"),
            "definitions are visible: {out}"
        );

        let save = serde_json::json!({
            "action": "save",
            "name": "dev-room",
            "members": [{"name": "qa", "agent": "qa"}, {"name": "dev", "agent": "dev"}],
            "mode": "serial",
        });
        let out = call(&tool, &ctx, save).await;
        assert!(out.contains("wrote .bingo/team.json"), "{out}");

        let out = call(&tool, &ctx, serde_json::json!({"action": "validate"})).await;
        assert!(out.starts_with("ok:"), "{out}");

        let out = call(&tool, &ctx, serde_json::json!({"action": "start"})).await;
        assert!(out.contains("spawned 2"), "{out}");
        assert_eq!(session.agents.list().len(), 2);
        assert!(
            session.channels.info("dev-room").is_some(),
            "channel was created"
        );

        // Idempotent: a second start reuses rather than duplicating.
        let out = call(&tool, &ctx, serde_json::json!({"action": "start"})).await;
        assert!(out.contains("reused 2"), "{out}");
        assert_eq!(session.agents.list().len(), 2);

        let out = call(&tool, &ctx, serde_json::json!({"action": "status"})).await;
        assert!(
            out.contains("team dev-room") && out.contains("· idle ·"),
            "{out}"
        );

        let out = call(&tool, &ctx, serde_json::json!({"action": "stop"})).await;
        assert!(out.contains("stopped 2"), "{out}");
        let _ = std::fs::remove_dir_all(project.parent().unwrap_or(&project));
    }

    /// save is whole-document and reference-checked: a roster naming a definition that
    /// doesn't exist is refused before anything reaches the disk.
    #[tokio::test]
    async fn save_rejects_unknown_definition_without_writing() {
        let (session, project) = fixture("badref");
        std::fs::write(project.join(".bingo/agents/qa.md"), "You are QA.\n").unwrap_or_default();
        let tool = TeamTool::new(session.clone());
        let ctx = ctx(&session, &project);
        let err = tool
            .call(
                serde_json::json!({
                    "action": "save",
                    "name": "t",
                    "members": [{"name": "ghost", "agent": "ghost"}],
                }),
                &ctx,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("ghost") && err.contains("qa"), "{err}");
        assert!(
            !project.join(crate::team::TEAM_FILE).exists(),
            "failed validation must not be persisted"
        );
        let _ = std::fs::remove_dir_all(project.parent().unwrap_or(&project));
    }

    /// A roster edit must not quietly dissolve the org chart. `save` writes this
    /// directory's blueprint whole, but the child teams it declares travel across it
    /// untouched — they live in other directories and are no business of a roster
    /// change. The confirmation line says so, so their absence from the call is not
    /// read as their removal.
    #[tokio::test]
    async fn save_carries_the_org_chart_across_a_roster_edit() {
        let (session, project) = fixture("chart-save");
        std::fs::write(project.join(".bingo/agents/qa.md"), "You are QA.\n").unwrap_or_default();
        std::fs::write(project.join(".bingo/agents/dev.md"), "You are Dev.\n").unwrap_or_default();
        let child = project.join("repos/dept");
        std::fs::create_dir_all(child.join(".bingo/agents")).unwrap_or_default();
        std::fs::write(child.join(".bingo/agents/sre.md"), "You are SRE.\n").unwrap_or_default();
        std::fs::write(
            child.join(crate::team::TEAM_FILE),
            r#"{"name":"dept","members":[{"name":"Kai","agent":"sre"}]}"#,
        )
        .unwrap_or_default();
        std::fs::write(
            project.join(crate::team::TEAM_FILE),
            r#"{"name":"hq","members":[{"name":"Mira","agent":"qa"}],"teams":[{"path":"repos/dept"}]}"#,
        )
        .unwrap_or_default();

        let tool = TeamTool::new(session.clone());
        let ctx = ctx(&session, &project);
        let save = serde_json::json!({
            "action": "save",
            "name": "hq",
            "members": [{"name": "Mira", "agent": "qa"}, {"name": "Linh", "agent": "dev"}],
        });
        // The user reads the delta, and the chart is part of what is at stake.
        let reason = save_reason(
            &project,
            &parse_input::<TeamInput>(&save).unwrap_or_else(|e| panic!("{e}")),
        );
        assert!(
            reason.contains("+Linh") && reason.contains("1 child team(s) kept"),
            "{reason}"
        );

        let out = call(&tool, &ctx, save).await;
        assert!(out.contains("wrote .bingo/team.json"), "{out}");
        let after = crate::team::load_team_file(&project)
            .unwrap_or_default()
            .unwrap_or_else(|| panic!("blueprint should exist"));
        assert_eq!(after.teams.len(), 1, "the child team survived the rewrite");
        assert_eq!(after.teams[0].path, "repos/dept");

        // And the chart still loads, with the department under it.
        let out = call(&tool, &ctx, serde_json::json!({"action": "status"})).await;
        assert!(out.contains("team dept") && out.contains("Kai"), "{out}");
        assert!(out.contains("2 teams in this chart"), "{out}");
        let _ = std::fs::remove_dir_all(project.parent().unwrap_or(&project));
    }

    /// A team may hold several rooms with different rosters. `save` writes them, and
    /// then refuses to configure a single channel that no longer exists — the field
    /// pair that would otherwise silently delete every room it cannot describe.
    #[tokio::test]
    async fn save_writes_rooms_and_refuses_to_flatten_them_by_accident() {
        let (session, project) = fixture("chart-rooms");
        for role in ["qa", "dev"] {
            std::fs::write(
                project.join(format!(".bingo/agents/{role}.md")),
                format!("You are {role}.\n"),
            )
            .unwrap_or_default();
        }
        let tool = TeamTool::new(session.clone());
        let ctx = ctx(&session, &project);

        let out = call(
            &tool,
            &ctx,
            serde_json::json!({
                "action": "save",
                "name": "hq",
                "members": [{"name": "Mira", "agent": "qa"}, {"name": "Linh", "agent": "dev"}],
                "channels": [
                    {"name": "standup", "members": ["Mira", "Linh"]},
                    {"name": "review", "mode": "free", "members": ["Linh"]},
                ],
            }),
        )
        .await;
        assert!(
            out.contains("#standup (serial)") && out.contains("#review (free)"),
            "{out}"
        );
        let after = crate::team::load_team_file(&project)
            .unwrap_or_default()
            .unwrap_or_else(|| panic!("blueprint should exist"));
        assert_eq!(after.channels.len(), 2);
        assert_eq!(
            after.channels[1].members.as_deref(),
            Some(&["Linh".to_string()][..])
        );

        // Omitting channels keeps them; sending mode alongside them is refused rather
        // than guessed at.
        let out = call(
            &tool,
            &ctx,
            serde_json::json!({
                "action": "save",
                "name": "hq",
                "members": [{"name": "Mira", "agent": "qa"}, {"name": "Linh", "agent": "dev"}],
            }),
        )
        .await;
        assert!(out.contains("#standup") && out.contains("#review"), "{out}");

        let err = tool
            .call(
                serde_json::json!({
                    "action": "save",
                    "name": "hq",
                    "members": [{"name": "Mira", "agent": "qa"}],
                    "mode": "free",
                }),
                &ctx,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("declares its own rooms"), "{err}");
        assert_eq!(
            crate::team::load_team_file(&project)
                .unwrap_or_default()
                .map(|d| d.channels.len()),
            Some(2),
            "a refused save changes nothing"
        );

        // A room naming someone who is not in this team's reach is refused too.
        let err = tool
            .call(
                serde_json::json!({
                    "action": "save",
                    "name": "hq",
                    "members": [{"name": "Mira", "agent": "qa"}],
                    "channels": [{"name": "standup", "members": ["Mira", "Ghost"]}],
                }),
                &ctx,
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("Ghost"), "{err}");
        let _ = std::fs::remove_dir_all(project.parent().unwrap_or(&project));
    }

    /// The permission contract: reads pass, changes ask — and the ask survives the modes
    /// that wave everything else through.
    #[test]
    fn changes_always_ask_reads_never_do() {
        let (session, _project) = fixture("gate");
        let tool = TeamTool::new(session);
        for action in ["status", "validate"] {
            let input = serde_json::json!({ "action": action });
            assert_eq!(
                decide(&tool, input, PermissionMode::Default).behavior,
                PermissionBehavior::Allow,
                "{action} is read-only, no ask"
            );
        }
        for action in ["start", "stop", "save"] {
            let input = || serde_json::json!({ "action": action, "name": "t", "members": [{"name": "a", "agent": "a"}] });
            for mode in [
                PermissionMode::Default,
                PermissionMode::AcceptEdits,
                PermissionMode::BypassPermissions,
                PermissionMode::DontAsk,
                PermissionMode::Plan,
            ] {
                assert_eq!(
                    decide(&tool, input(), mode).behavior,
                    PermissionBehavior::Ask,
                    "{action} still requires user confirmation under {mode:?}"
                );
            }
            // An allow rule doesn't pre-authorize it either; only deny outranks it.
            let allow = vec!["Team".to_string()];
            assert_eq!(
                can_use_tool(
                    &tool as &dyn Tool,
                    &input(),
                    PermissionMode::Default,
                    &[],
                    &[],
                    &allow,
                    &std::env::temp_dir()
                )
                .behavior,
                PermissionBehavior::Ask,
                "{action} allow rule must not skip the ask"
            );
            let deny = vec!["Team".to_string()];
            assert_eq!(
                can_use_tool(
                    &tool as &dyn Tool,
                    &input(),
                    PermissionMode::Default,
                    &deny,
                    &[],
                    &[],
                    &std::env::temp_dir()
                )
                .behavior,
                PermissionBehavior::Deny,
                "{action} deny rule still applies"
            );
        }
    }

    /// The confirmation line is what the user actually decides on, so it names the delta:
    /// who joins, who leaves, and how big the crew ends up.
    #[test]
    fn save_reason_reports_the_delta() {
        let project = std::env::temp_dir().join(format!("bingo-teamreason-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        std::fs::create_dir_all(&project).unwrap_or_default();
        let params = |names: &[&str]| TeamInput {
            action: TeamAction::Save,
            name: Some("dev-room".into()),
            members: Some(
                names
                    .iter()
                    .map(|n| MemberInput {
                        name: n.to_string(),
                        agent: n.to_string(),
                        ..Default::default()
                    })
                    .collect(),
            ),
            channels: None,
            mode: None,
            message_limit: None,
        };
        // No file yet: a creation, named in full.
        let reason = save_reason(&project, &params(&["qa", "dev"]));
        assert!(
            reason.contains("Create") && reason.contains("qa, dev"),
            "{reason}"
        );

        crate::team::write_team_file(
            &project,
            &TeamDef {
                name: "dev-room".into(),
                members: vec![
                    TeamMember {
                        name: "qa".into(),
                        agent: "qa".into(),
                        ..Default::default()
                    },
                    TeamMember {
                        name: "ui".into(),
                        agent: "ui".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let reason = save_reason(&project, &params(&["qa", "dev"]));
        assert!(
            reason.contains("-ui") && reason.contains("+dev") && reason.contains("2 member(s)"),
            "{reason}"
        );
        // A no-op rewrite says so rather than implying a change.
        let reason = save_reason(&project, &params(&["qa", "ui"]));
        assert!(reason.contains("members unchanged"), "{reason}");
        assert!(
            !reason.contains("channel"),
            "no change must not mention the channel: {reason}"
        );
        // An omitted mode silently reverts a free room to serial — the line says so.
        crate::team::write_team_file(
            &project,
            &TeamDef {
                name: "dev-room".into(),
                channel: Some(ChannelSpec {
                    mode: Some("free".into()),
                    message_limit: None,
                }),
                members: vec![TeamMember {
                    name: "qa".into(),
                    agent: "qa".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let reason = save_reason(&project, &params(&["qa"]));
        assert!(reason.contains("channel free → serial"), "{reason}");
        let _ = std::fs::remove_dir_all(&project);
    }

    #[test]
    fn schema_shape() {
        let schema = super::super::schema_for::<TeamInput>();
        assert_eq!(schema["required"], serde_json::json!(["action"]));
        assert_eq!(schema["additionalProperties"], serde_json::json!(false));
        assert!(schema["definitions"]["MemberInput"].is_object(), "{schema}");
    }
}

//! Agent teams: project-level squads (D31).
//!
//! Mental model: the team is the blueprint (persistent definition in
//! `.bingo/team.json`), the room is the construction site (runtime instances +
//! channels). This module = three thin layers: team.json parsing and validation
//! (validate and start share the same source: if validate passes, start must
//! succeed), `spawn_team` orchestration (reuses the existing Agent spawn +
//! ChannelRegistry; idempotency key = instance name), and team memory (key =
//! project-path hash + branch, persisted across sessions and *pointed at* rather
//! than preloaded — see [`member_memory_note`], D51).
//!
//! Members reference AgentDefs rather than inlining personas — the single source
//! of truth for a persona stays in `.bingo/agents/<name>.md`; the team is only a
//! formation layer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agents::{AgentDef, AgentDefSource};
use crate::channels::ChannelMode;
use crate::error::ErrorCode;
use crate::query::Session;

/// Team config file (project-level `.bingo/team.json`, checked into version control).
pub const TEAM_FILE: &str = ".bingo/team.json";
/// Memory root directory: `~/.config/bingo/teams/`.
const TEAM_MEMORY_ROOT: &str = "teams";

#[derive(Debug, Error)]
pub enum TeamError {
    #[error("team.json read failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("team.json parse failed: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}

impl ErrorCode for TeamError {
    fn error_code(&self) -> &'static str {
        match self {
            TeamError::Io(_) | TeamError::Parse(_) | TeamError::Invalid(_) => "CONFIG_INVALID",
        }
    }
}

impl TeamError {
    fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }
}

/// Room spec (reuses the existing Channel vocabulary; no new concepts invented).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSpec {
    /// Speaking mode: serial (default) | free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Total message cap per channel (default 500, see ChannelLimits).
    #[serde(
        rename = "messageLimit",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub message_limit: Option<u64>,
}

/// A single member: `name` (instance name) + `agent` (referenced AgentDef name),
/// plus the portrait it wears. The face is part of the blueprint because a crew is
/// a standing cast: pinned here, a member keeps one face across sessions instead of
/// whatever a hash of its instance name happens to land on.
///
/// The engine is pinned here for the same reason. Which model does which job is a
/// property of the formation — the reviewer on a cheap fast endpoint, the architect
/// on the expensive one — so it belongs in the committed blueprint rather than being
/// re-decided at every spawn. All three are optional and, when absent, defer to the
/// agent definition and then to the parent session, exactly as an explicit `Agent`
/// call's parameters do (see [`crate::tool::agent::build_sub_session`]).
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
pub struct TeamMember {
    pub name: String,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
}

/// Team definition (blueprint). Parsing and writing share this one struct: the file
/// format has a single source, so a written blueprint reads back as the same value.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<ChannelSpec>,
    pub members: Vec<TeamMember>,
}

/// Parse `.bingo/team.json`: returns Ok(None) if absent; otherwise parses + structural
/// validation (invalid types/enums are errors). Existence of referenced AgentDefs is
/// checked by `validate` (needs the loaded definition list; not done in pure parsing).
pub fn load_team_file(project_dir: &Path) -> Result<Option<TeamDef>, TeamError> {
    let path = project_dir.join(TEAM_FILE);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let def: TeamDef = serde_json::from_str(&raw)?;
    validate_structure(&def, &path)?;
    Ok(Some(def))
}

/// Structural validation (no AgentDef list needed): name/channel mode/member constraints.
/// Shares the error format with `validate` (three parts: file path + field path + expectation).
fn validate_structure(def: &TeamDef, path: &Path) -> Result<(), TeamError> {
    let file = path.display();
    if def.name.trim().is_empty() {
        return Err(TeamError::invalid(format!(
            "{file}: name: must not be empty (a team needs a name to be distinguishable)"
        )));
    }
    if def.members.is_empty() {
        return Err(TeamError::invalid(format!(
            "{file}: members: must not be empty (an empty team is meaningless; a single-member team is fine)"
        )));
    }
    if let Some(spec) = &def.channel {
        if let Some(mode) = &spec.mode {
            ChannelMode::parse(mode)
                .map_err(|e| TeamError::invalid(format!("{file}: channel.mode: {e}")))?;
        }
        if let Some(limit) = spec.message_limit
            && limit == 0
        {
            return Err(TeamError::invalid(format!(
                "{file}: channel.messageLimit: must be a positive integer"
            )));
        }
    }
    let mut seen = std::collections::HashSet::new();
    for (i, m) in def.members.iter().enumerate() {
        if m.name.trim().is_empty() {
            return Err(TeamError::invalid(format!(
                "{file}: members[{i}].name: must not be empty"
            )));
        }
        if m.agent.trim().is_empty() {
            return Err(TeamError::invalid(format!(
                "{file}: members[{i}].agent: must not be empty (must reference an AgentDef)"
            )));
        }
        if !seen.insert(m.name.as_str()) {
            return Err(TeamError::invalid(format!(
                "{file}: members[{i}].name: duplicate \"{}\" within the config (member names must be unique)",
                m.name
            )));
        }
    }
    Ok(())
}

/// Write the blueprint to `.bingo/team.json` (creating `.bingo/` if needed).
/// Structural validation runs first and shares its source with `load_team_file`:
/// what this writes must parse back, so a written file can never be one the reader
/// rejects. Reference validation (`validate`) stays with the caller — it needs the
/// AgentDef list, which the format itself doesn't carry.
pub fn write_team_file(project_dir: &Path, def: &TeamDef) -> Result<(), TeamError> {
    let path = project_dir.join(TEAM_FILE);
    validate_structure(def, &path)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut json = serde_json::to_string_pretty(def)?;
    json.push('\n');
    std::fs::write(&path, json)?;
    Ok(())
}

/// Reference validation: each member's agent must exist in the definition list
/// (project + user layers), and the engine it pins must be one this session can
/// actually start. Shared by `/team validate` and `spawn_team` (same source:
/// if validate passes, start must succeed).
///
/// The engine checks mirror [`crate::tool::agent::build_sub_session`] instead of
/// inventing a stricter rule of their own, because that function is what `start`
/// runs: a blueprint accepted here that then failed to spawn would leave the
/// invariant a slogan. They are judged against the session's *current* endpoint,
/// so switching provider afterwards can change the verdict — exactly as it
/// changes what `start` would do.
pub fn validate(def: &TeamDef, defs: &[AgentDef], session: &Session) -> Result<(), TeamError> {
    let by_name: HashMap<&str, &AgentDef> = defs.iter().map(|d| (d.name.as_str(), d)).collect();
    let current = session.runtime.provider.borrow().clone();
    for (i, m) in def.members.iter().enumerate() {
        let Some(agent_def) = by_name.get(m.agent.as_str()) else {
            let known: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
            let hint = if known.is_empty() {
                "no AgentDef available (project-level `.bingo/agents/*.md` or user-level `~/.config/bingo/agents/*.md`)"
                    .to_string()
            } else {
                format!("available: {}", known.join(", "))
            };
            return Err(TeamError::invalid(format!(
                "{TEAM_FILE}: members[{i}].agent: references a non-existent AgentDef \"{}\"; {hint}",
                m.agent
            )));
        };
        let field = |name: &str| format!("{TEAM_FILE}: members[{i}].{name}");
        // Absent and "default" both mean the session's own endpoint, so only a
        // named one is looked up — the same filter build_sub_session applies.
        if let Some(provider) = m
            .provider
            .as_deref()
            .or(agent_def.provider.as_deref())
            .filter(|p| *p != "default")
        {
            if let Err(e) = session.client.with_provider(provider) {
                return Err(TeamError::invalid(format!("{}: {e}", field("provider"))));
            }
            if provider != current && m.model.is_none() && agent_def.model.is_none() {
                return Err(TeamError::invalid(format!(
                    "{}: provider \"{provider}\" needs a model: a cross-provider member does not \
                     inherit the session's (current provider = \"{current}\") — add model, or drop provider",
                    field("model")
                )));
            }
        }
        if let Some(level) = m.thinking.as_deref().or(agent_def.thinking.as_deref())
            && let Err(e) = crate::tool::agent::normalize_thinking(level)
        {
            return Err(TeamError::invalid(format!("{}: {e}", field("thinking"))));
        }
    }
    Ok(())
}

/// For display: the team definition + the definitions its members reference (/team list definitions section).
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

/// Definitions-section view: when a member's reference is missing, source is Unknown
/// and description is empty (no error — the display layer tolerates bad references;
/// rejection happens at spawn).
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
                        .unwrap_or_else(|| "(missing definition)".to_string()),
                    source: agent.map(|a| a.source).unwrap_or(AgentDefSource::Unknown),
                }
            })
            .collect(),
    }
}

/// What a member's blueprint pins about its engine, as a display suffix. Empty
/// when it pins nothing — the common case, where the member runs whatever its
/// agent definition or the session runs.
///
/// Only what the file holds is reported. An inherited engine is deliberately not
/// named: it is whatever the session happens to be on when the member spawns, so
/// printing today's value would read as a pin the blueprint does not have.
pub fn engine_label(m: &TeamMember) -> String {
    let parts: Vec<String> = [
        m.provider.as_deref().map(|p| format!("provider {p}")),
        m.model.as_deref().map(|m| format!("model {m}")),
        m.thinking.as_deref().map(|t| format!("thinking {t}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        return String::new();
    }
    format!(" · {}", parts.join(" · "))
}

/// Channel mode parsing (defaults to serial).
pub fn channel_mode(def: &TeamDef) -> ChannelMode {
    def.channel
        .as_ref()
        .and_then(|s| s.mode.as_deref())
        .and_then(|m| ChannelMode::parse(m).ok())
        .unwrap_or(ChannelMode::Serial)
}

// ---- team memory (key = project-path hash + branch) ----

/// Memory root directory: `~/.config/bingo/teams/` (user level, not in version control by default).
pub fn team_memory_root(home: &Path) -> PathBuf {
    let config = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home.join(".config"));
    config.join("bingo").join(TEAM_MEMORY_ROOT)
}

/// Project key: `<dir name>-<full path hash>` (same key family as project memory
/// `memory_file`; naturally isolated across worktrees — different worktree paths →
/// different keys).
pub fn project_key(project_dir: &Path) -> String {
    let name = project_dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "root".to_string());
    let name: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{name}-{}", crate::memory::path_hash(project_dir))
}

/// Current branch name (falls back to "detached" when not a git repo / no branch).
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

/// Memory directory of a team under a project + branch:
/// `~/.config/bingo/teams/<project_key>/<branch>/<team>/`.
pub fn team_memory_dir(home: &Path, project_dir: &Path, branch: &str, team: &str) -> PathBuf {
    team_memory_root(home)
        .join(project_key(project_dir))
        .join(branch)
        .join(team)
}

/// Member history record (the exact messages, so the choice not to preload them
/// stays reversible and `/team memory` has something lossless to work from).
pub fn member_history_path(dir: &Path, member: &str) -> PathBuf {
    dir.join(format!("{}.json", sanitize_name(member)))
}

/// Member transcript: the readable view of the record beside it, and the file a
/// spawning member is pointed at.
pub fn member_transcript_path(dir: &Path, member: &str) -> PathBuf {
    dir.join(format!("{}.md", sanitize_name(member)))
}

/// Decision log file (append-only, `sources` pipe-separated, reuses the frontmatter convention).
pub fn decisions_path(dir: &Path) -> PathBuf {
    dir.join("decisions.md")
}

fn sanitize_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---- spawn orchestration (spawn_team) ----

/// Summary of one spawn (shared by /team start output and the event log).
#[derive(Debug, Clone, Default)]
pub struct SpawnSummary {
    /// Newly spawned instance names.
    pub spawned: Vec<String>,
    /// Reused existing instances (idempotent: not re-spawned).
    pub reused: Vec<String>,
    /// Failed members: (instance name, reason).
    pub failed: Vec<(String, String)>,
}

impl SpawnSummary {
    /// Event wording (QA acceptance: `spawned ×N` vs `reused ×N` are greppable/assertable).
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

/// Spawn a team (D31): create the channel (idempotent) + spawn/reuse member instances
/// ("spawn ≠ wake" — members sit in Idle standby, zero tokens, zero turns, only
/// starting on SendMessage/channel messages). Memory restore goes through the same
/// path: persisted history is preloaded (no wake-up), missing files silently fall
/// back to empty history. Member-level failure isolation: one failure doesn't sink
/// the whole team; the failed member stays in `failed` and can be re-spawned alone.
/// Returns `Err` only on config validation failure (validate and start share the same source).
pub fn spawn_team(
    session: &Arc<Session>,
    def: &TeamDef,
    defs: &[AgentDef],
    home: &Path,
    project_dir: &Path,
    branch: &str,
) -> Result<SpawnSummary, TeamError> {
    validate(def, defs, session)?;
    let mut summary = SpawnSummary::default();

    // Channel idempotency: create-if-not-exists.
    let channel_name = &def.name;
    let channel_exists = session.channels.info(channel_name).is_some();
    if !channel_exists
        && let Err(e) = session
            .channels
            .create(channel_name, Vec::new(), channel_mode(def))
    {
        summary.failed.push((channel_name.clone(), e));
    }
    if let Some(limit) = def.channel.as_ref().and_then(|s| s.message_limit) {
        let _ = session.channels.set_message_limit(channel_name, limit);
    }

    let by_name: HashMap<&str, &AgentDef> = defs.iter().map(|d| (d.name.as_str(), d)).collect();
    for member in &def.members {
        // Idempotency key = instance name: already exists (Idle/Running) → reuse, no re-spawn.
        let exists = session.agents.list().iter().any(|a| a.name == member.name);
        if exists {
            summary.reused.push(member.name.clone());
            continue;
        }
        let Some(agent_def) = by_name.get(member.agent.as_str()) else {
            summary.failed.push((
                member.name.clone(),
                format!("references a non-existent AgentDef \"{}\"", member.agent),
            ));
            continue;
        };
        let name = session.agents.claim_name(&member.name);
        // Memory is a pointer, not a preload (D51): the member is told where its
        // past is and starts with an empty context.
        ensure_transcript(home, project_dir, branch, &def.name, &name);
        let memory = member_memory_note(home, project_dir, branch, &def.name, &name);
        let sub = match crate::tool::agent::build_sub_session(
            session,
            member.model.clone(),
            member.provider.clone(),
            member.thinking.clone(),
            Some(agent_def),
            &name,
            memory,
        ) {
            Ok(s) => s,
            Err(e) => {
                summary.failed.push((member.name.clone(), e.to_string()));
                continue;
            }
        };
        let description = agent_def.description.clone();
        session
            .agents
            .insert(&name, Some(member.agent.clone()), description, sub);
        // Spawn ≠ wake: mark Idle after insert (zero-token standby; the turn only starts with SendMessage).
        session.agents.mark_idle(&name);
        // Join the channel (late joiners get no backlog; they listen from the current head).
        let _ = session.channels.invite(channel_name, &name);
        summary.spawned.push(name);
    }
    Ok(summary)
}

// ---- memory read/write (cross-session restore) ----

/// Save a member's history: the JSON record plus the readable transcript beside
/// it. One writer for both, so the two can never drift — the JSON is the data the
/// runtime wrote, the transcript is the view of it a reader is pointed at
/// ([`member_memory_note`]). Failures are silent — memory is an enhancement, not
/// a contract.
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
    if let Ok(json) = serde_json::to_string_pretty(history) {
        let _ = std::fs::write(member_history_path(&dir, member), json);
    }
    let _ = std::fs::write(
        member_transcript_path(&dir, member),
        transcript(member, history),
    );
}

/// A history as prose. Pointing a reader at serialized `Message` structs — content
/// blocks, tool_use/tool_result envelopes, base64 image payloads — would be a
/// promise that fails on contact, so the thing the note names is written for
/// reading: who said what, with tool calls as one line each and image payloads
/// named rather than inlined.
pub fn transcript(member: &str, history: &[crate::api::types::Message]) -> String {
    use crate::api::types::{ContentBlock, Role};
    let mut out = format!(
        "# {member} — {} messages\n\nWritten by bingo when the session ended. \
         Prose is verbatim; tool calls are summarized to one line.\n",
        history.len()
    );
    for (i, message) in history.iter().enumerate() {
        let who = match message.role {
            Role::User => "user",
            Role::Assistant => member,
        };
        out.push_str(&format!("\n## {}. {who}\n\n", i + 1));
        for block in &message.content {
            match block {
                ContentBlock::Text { text } if !text.trim().is_empty() => {
                    out.push_str(text.trim_end());
                    out.push_str("\n\n");
                }
                ContentBlock::Text { .. } => {}
                ContentBlock::Thinking { .. } => {
                    // Reasoning is not a decision and does not survive as one.
                    out.push_str("_(thinking)_\n\n");
                }
                ContentBlock::ToolUse { name, input, .. } => {
                    out.push_str(&format!(
                        "- called `{name}` — {}\n",
                        one_line_json(input, 160)
                    ));
                }
                ContentBlock::ToolResult {
                    content, is_error, ..
                } => {
                    let mark = if *is_error { "failed" } else { "returned" };
                    out.push_str(&format!("  - {mark}: {}\n", one_line_json(content, 200)));
                }
                ContentBlock::Image { source } => {
                    out.push_str(&format!(
                        "- image ({}, {} bytes of base64, not stored here)\n",
                        source.media_type,
                        source.data.len()
                    ));
                }
            }
        }
    }
    out
}

/// A JSON value as one clipped line: tool inputs and results are for orientation
/// here, not replay — the record next to it holds the exact bytes.
fn one_line_json(value: &serde_json::Value, max: usize) -> String {
    let raw = match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    let flat: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    // A nested value arrives already escaped, so its line breaks are the two
    // characters `\` and `n` rather than control characters — flattening only the
    // real ones would leave the noise this function exists to remove.
    let flat = flat
        .replace("\\n", " ")
        .replace("\\t", " ")
        .replace("\\r", " ");
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max {
        format!("{}…", flat.chars().take(max).collect::<String>())
    } else {
        flat
    }
}

/// Render the transcript from the record when it is missing. Histories written
/// before D51 are JSON only, and a note pointing at a file that does not exist is
/// worse than no note — so the readable view materializes the first time a member
/// with an older past spawns.
pub fn ensure_transcript(home: &Path, project_dir: &Path, branch: &str, team: &str, member: &str) {
    let dir = team_memory_dir(home, project_dir, branch, team);
    let path = member_transcript_path(&dir, member);
    if path.exists() {
        return;
    }
    let history = load_member_history(home, project_dir, branch, team, member);
    if history.is_empty() {
        return;
    }
    let _ = std::fs::write(path, transcript(member, &history));
}

/// What a spawning member is told about its own past, or `None` when it has none.
///
/// The history is deliberately *not* loaded into the member's context (D51). It is
/// unbounded and monotonic — every session appends and nothing prunes — so
/// preloading it charged a growing, invisible toll on the member's first turn, for
/// relevance that decays fast. The member is a capable reader with file tools:
/// telling it where its past is costs a couple of dozen tokens and lets it decide
/// whether the past is worth the read.
pub fn member_memory_note(
    home: &Path,
    project_dir: &Path,
    branch: &str,
    team: &str,
    member: &str,
) -> Option<String> {
    let dir = team_memory_dir(home, project_dir, branch, team);
    let path = member_transcript_path(&dir, member);
    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() == 0 {
        return None;
    }
    let count = load_member_history(home, project_dir, branch, team, member).len();
    let scale = if count == 0 {
        String::new()
    } else {
        format!(" ({count} messages)")
    };
    Some(format!(
        "Your earlier work with this crew on branch \"{branch}\" is on disk at \
         {}{scale}. It is NOT in this conversation — you are starting fresh on \
         purpose. Read that file when you need what was already decided, tried or \
         ruled out; do not re-litigate it from memory, and do not read it \
         speculatively when the task in front of you does not depend on it.",
        path.display()
    ))
}

/// Load member history (missing/corrupt → empty, silently fall back).
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

/// Append a decision record (append-only, zero model cost; frontmatter pipe-separated
/// convention `sources: a|b|c`, `type` lives at entry level). Failures are silent.
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
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
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
        // Empty members.
        let path = write_team(&dir, r#"{"name":"t","members":[]}"#);
        let err = load_team_file(&dir).unwrap_err().to_string();
        assert!(
            err.contains("members") && err.contains("must not be empty"),
            "{err}"
        );
        // Duplicate names within the config.
        write_team(
            &dir,
            r#"{"name":"t","members":[{"name":"a","agent":"x"},{"name":"a","agent":"y"}]}"#,
        );
        let err = load_team_file(&dir).unwrap_err().to_string();
        assert!(
            err.contains("duplicate") && err.contains("members[1]"),
            "{err}"
        );
        // Invalid channel mode.
        write_team(
            &dir,
            r#"{"name":"t","channel":{"mode":"bogus"},"members":[{"name":"a","agent":"x"}]}"#,
        );
        let err = load_team_file(&dir).unwrap_err().to_string();
        assert!(err.contains("channel.mode"), "{err}");
        let _ = path;
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Write → read is the identity: the blueprint the tool saves is the blueprint the
    /// loader returns, and a structurally invalid one never reaches the disk.
    #[test]
    fn write_team_file_round_trips_and_rejects_invalid() {
        let dir = tmp("write");
        let def = TeamDef {
            name: "dev-room".into(),
            channel: Some(ChannelSpec {
                mode: Some("free".into()),
                message_limit: Some(80),
            }),
            members: vec![TeamMember {
                name: "qa".into(),
                agent: "qa".into(),
                avatar: Some("sora".into()),
                model: Some("sub-model".into()),
                provider: Some("ds".into()),
                thinking: Some("xhigh".into()),
            }],
        };
        write_team_file(&dir, &def).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(load_team_file(&dir).unwrap().as_ref(), Some(&def));
        // camelCase is preserved on the way out (the file stays hand-editable).
        let raw = std::fs::read_to_string(dir.join(TEAM_FILE)).unwrap();
        assert!(raw.contains("\"messageLimit\": 80"), "{raw}");

        let bad = TeamDef {
            name: "t".into(),
            channel: None,
            members: Vec::new(),
        };
        let err = write_team_file(&dir, &bad).unwrap_err().to_string();
        assert!(err.contains("members"), "{err}");
        assert_eq!(
            load_team_file(&dir).unwrap().as_ref(),
            Some(&def),
            "rejected writes are not persisted"
        );
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
                ..Default::default()
            }],
        };
        let err = validate(&def, &[], &session()).unwrap_err().to_string();
        assert!(
            err.contains("ghost") && err.contains("no AgentDef available"),
            "{err}"
        );
        let known = AgentDef {
            name: "real".into(),
            description: "d".into(),
            model: None,
            provider: None,
            thinking: None,
            system: "s".into(),
            inherit_system: true,
            source: AgentDefSource::Project,
        };
        let ok = TeamDef {
            name: "t".into(),
            channel: None,
            members: vec![TeamMember {
                name: "a".into(),
                agent: "real".into(),
                ..Default::default()
            }],
        };
        assert!(validate(&ok, &[known], &session()).is_ok());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The file the note names has to be worth opening: prose verbatim, tool calls
    /// as one line, and image payloads named rather than inlined. Pointing a reader
    /// at serialized content blocks would be a promise that fails on contact.
    #[test]
    fn transcript_is_written_for_reading() {
        use crate::api::types::{ContentBlock, Message, Role};
        let history = vec![
            Message::user_text("ship the release"),
            Message {
                role: Role::Assistant,
                content: vec![
                    ContentBlock::Thinking {
                        thinking: "long private reasoning".into(),
                        signature: "sig".into(),
                    },
                    ContentBlock::Text {
                        text: "Cutting v0.3.3 now.".into(),
                    },
                    ContentBlock::ToolUse {
                        id: "1".into(),
                        name: "Bash".into(),
                        input: serde_json::json!({"command": "cargo test\n--locked"}),
                    },
                ],
            },
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "1".into(),
                    content: serde_json::json!("all green"),
                    is_error: false,
                }],
            },
        ];
        let out = transcript("deploy", &history);
        assert!(out.starts_with("# deploy — 3 messages"), "{out}");
        assert!(out.contains("## 1. user"), "{out}");
        assert!(
            out.contains("## 2. deploy"),
            "the member speaks under its own name: {out}"
        );
        assert!(out.contains("Cutting v0.3.3 now."), "prose verbatim: {out}");
        assert!(out.contains("called `Bash`"), "{out}");
        assert!(
            out.contains("cargo test --locked"),
            "a multi-line command flattens to one line: {out}"
        );
        assert!(out.contains("returned: all green"), "{out}");
        assert!(
            !out.contains("long private reasoning"),
            "reasoning is not a decision and does not survive as one: {out}"
        );
        // Nothing in it is JSON envelope noise.
        assert!(!out.contains("tool_use_id"), "{out}");
    }

    #[test]
    fn memory_dir_scopes_by_project_and_branch() {
        let home = std::path::Path::new("/tmp/home");
        let a = team_memory_dir(home, std::path::Path::new("/work/alpha"), "main", "dev");
        let b = team_memory_dir(home, std::path::Path::new("/work/beta"), "main", "dev");
        assert_ne!(a, b, "different projects are isolated");
        // Same project, different branches are isolated (worktree scenario).
        let c = team_memory_dir(
            home,
            std::path::Path::new("/work/alpha"),
            "agent-team",
            "dev",
        );
        assert_ne!(a, c, "different branches are isolated");
        assert!(a.starts_with(team_memory_root(home)));
        assert!(a.to_string_lossy().contains("dev"), "{a:?}");
    }

    #[test]
    fn project_key_is_stable_and_path_scoped() {
        let p = std::path::Path::new("/tmp/h/proj");
        assert_eq!(project_key(p), project_key(p), "stable");
        assert!(
            project_key(std::path::Path::new("/a/web"))
                != project_key(std::path::Path::new("/b/web")),
            "same-named dirs of different projects do not collide"
        );
    }

    fn def(name: &str) -> AgentDef {
        AgentDef {
            name: name.into(),
            description: format!("{name} description"),
            model: None,
            provider: None,
            thinking: None,
            system: format!("You are {name}."),
            inherit_system: true,
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
                    ..Default::default()
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
            user_config_dir: std::env::temp_dir().join(".config"),
            quiet: true,
            compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "t")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: AgentRegistry::new(),
            channels: ChannelRegistry::new(ChannelLimits::default()),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
        })
    }

    /// Spawn ≠ wake: newly spawned members are Idle (zero turns), the room is built;
    /// repeated start is an idempotent reuse.
    #[test]
    fn spawn_team_is_idempotent_and_members_idle() {
        let s = session();
        let mem_home = tmp("spawn-mem");
        let defs = vec![def("dev-ex"), def("ui/ux"), def("dev")];
        let team = team_def(
            "dev-room",
            &[("dev-ex", "dev-ex"), ("ui", "ui/ux"), ("dev", "dev")],
        );

        let first = spawn_team(&s, &team, &defs, &mem_home, &mem_home, "main")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(first.spawned.len(), 3, "{first:?}");
        assert!(first.reused.is_empty());
        assert!(first.failed.is_empty());
        // Members in Idle standby (zero tokens, no turn started); channel built with hub/user + three members.
        let states = s.agents.list();
        assert_eq!(states.len(), 3);
        assert!(
            states
                .iter()
                .all(|a| a.state == crate::agents::AgentState::Idle)
        );
        let ch = s
            .channels
            .info("dev-room")
            .unwrap_or_else(|| panic!("channel should exist"));
        assert_eq!(ch.members, vec!["main", "user", "dev-ex", "ui", "dev"]);

        // Repeated start: everything is reused, nothing re-spawned.
        let second = spawn_team(&s, &team, &defs, &mem_home, &mem_home, "main")
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(second.spawned.is_empty());
        assert_eq!(second.reused.len(), 3, "{second:?}");
        assert_eq!(s.agents.list().len(), 3, "no duplicate instances");
        std::fs::remove_dir_all(&mem_home).unwrap();
    }

    /// The model a member pins is the model its instance ends up running, and a
    /// member pinning nothing keeps the session's. This is the wiring assertion:
    /// were the blueprint's fields dropped on the way to `build_sub_session`,
    /// every member would report the session's model and nothing else would fail.
    #[test]
    fn spawn_team_applies_the_member_engine() {
        let s = session();
        let mem_home = tmp("spawn-engine");
        let defs = vec![def("qa"), def("dev")];
        let team = TeamDef {
            name: "t".into(),
            channel: None,
            members: vec![
                TeamMember {
                    name: "qa".into(),
                    agent: "qa".into(),
                    model: Some("pinned-model".into()),
                    ..Default::default()
                },
                TeamMember {
                    name: "dev".into(),
                    agent: "dev".into(),
                    ..Default::default()
                },
            ],
        };
        spawn_team(&s, &team, &defs, &mem_home, &mem_home, "main")
            .unwrap_or_else(|e| panic!("{e}"));
        let engine = |who: &str| {
            s.agents
                .list()
                .into_iter()
                .find(|a| a.name == who)
                .map(|a| a.model)
                .unwrap_or_default()
        };
        assert_eq!(engine("qa"), "pinned-model", "the pin reaches the instance");
        assert_eq!(engine("dev"), "m", "no pin keeps the session's model");
        std::fs::remove_dir_all(&mem_home).unwrap();
    }

    /// An engine this session could not actually start is a config error, caught
    /// before anything spawns rather than per member at spawn: `validate` and
    /// `start` share one source, so the invariant "validate passes ⇒ start
    /// succeeds" has to hold for the engine too, not just for agent references.
    #[test]
    fn validate_rejects_an_engine_the_session_cannot_start() {
        let s = session();
        let defs = vec![def("qa")];
        let member = |m: TeamMember| TeamDef {
            name: "t".into(),
            channel: None,
            members: vec![m],
        };
        let base = || TeamMember {
            name: "qa".into(),
            agent: "qa".into(),
            ..Default::default()
        };

        let err = validate(
            &member(TeamMember {
                provider: Some("nope".into()),
                model: Some("m".into()),
                ..base()
            }),
            &defs,
            &s,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("nope") && err.contains("members[0].provider"),
            "{err}"
        );

        let err = validate(
            &member(TeamMember {
                thinking: Some("bogus".into()),
                ..base()
            }),
            &defs,
            &s,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("bogus") && err.contains("members[0].thinking"),
            "{err}"
        );

        // Pinning nothing is the common case and stays valid; so does a model on
        // its own, which needs no endpoint of its own to be startable.
        assert!(validate(&member(base()), &defs, &s).is_ok());
        assert!(
            validate(
                &member(TeamMember {
                    model: Some("other".into()),
                    thinking: Some("high".into()),
                    ..base()
                }),
                &defs,
                &s
            )
            .is_ok()
        );
    }

    /// A rejected blueprint spawns nothing at all — the whole point of catching it
    /// in validate rather than letting members fail one by one.
    #[test]
    fn spawn_team_refuses_a_blueprint_with_a_bad_engine() {
        let s = session();
        let mem_home = tmp("spawn-bad-engine");
        let defs = vec![def("qa"), def("dev")];
        let team = TeamDef {
            name: "t".into(),
            channel: None,
            members: vec![
                TeamMember {
                    name: "qa".into(),
                    agent: "qa".into(),
                    ..Default::default()
                },
                TeamMember {
                    name: "dev".into(),
                    agent: "dev".into(),
                    thinking: Some("bogus".into()),
                    ..Default::default()
                },
            ],
        };
        let err = spawn_team(&s, &team, &defs, &mem_home, &mem_home, "main")
            .unwrap_err()
            .to_string();
        assert!(err.contains("bogus"), "{err}");
        assert!(s.agents.list().is_empty(), "no member is left half-spawned");
        std::fs::remove_dir_all(&mem_home).unwrap();
    }

    /// Memory is a pointer, not a preload (D51): a member spawns with an empty
    /// context and a note saying where its past is. The old behaviour charged a
    /// growing, invisible toll on the member's first turn — unbounded, monotonic,
    /// and mostly stale — for a file the member can read when it actually needs it.
    #[test]
    fn spawn_team_points_at_memory_instead_of_loading_it() {
        let s = session();
        let mem_home = tmp("spawn-restore");
        let defs = vec![def("qa")];
        let team = team_def("t", &[("qa", "qa")]);
        let msgs = vec![crate::api::types::Message::user_text(
            "last round's conclusion",
        )];
        save_member_history(&mem_home, &mem_home, "main", "t", "qa", &msgs);

        spawn_team(&s, &team, &defs, &mem_home, &mem_home, "main")
            .unwrap_or_else(|e| panic!("{e}"));
        let (history, _, state) = s
            .agents
            .view_of("qa")
            .unwrap_or_else(|| panic!("instance should exist"));
        assert!(
            history.is_empty(),
            "the past is not in the context: {history:?}"
        );
        assert_eq!(
            state,
            crate::agents::AgentState::Idle,
            "spawning still does not wake"
        );

        // What it gets instead: the file, named, with what is in it.
        let note = member_memory_note(&mem_home, &mem_home, "main", "t", "qa")
            .unwrap_or_else(|| panic!("a member with a past gets a note"));
        let path =
            member_transcript_path(&team_memory_dir(&mem_home, &mem_home, "main", "t"), "qa");
        assert!(note.contains(&path.display().to_string()), "{note}");
        assert!(note.contains("1 message"), "{note}");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap_or_default()
                .contains("last round's conclusion"),
            "the file it points at is readable and holds the history"
        );

        // A member with no past is told nothing at all.
        assert!(
            member_memory_note(&mem_home, &mem_home, "main", "t", "ghost").is_none(),
            "no past, no note"
        );

        // A history written before D51 is JSON only; the note would otherwise name
        // a file that does not exist, so the transcript materializes on spawn.
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("{e}"));
        assert!(member_memory_note(&mem_home, &mem_home, "main", "t", "qa").is_none());
        ensure_transcript(&mem_home, &mem_home, "main", "t", "qa");
        assert!(path.exists(), "an older record renders on first sight");
        assert!(member_memory_note(&mem_home, &mem_home, "main", "t", "qa").is_some());
        std::fs::remove_dir_all(&mem_home).unwrap();
    }

    /// Config validation failure (all references missing) → Err, nothing is spawned
    /// (validate and start share the same source).
    #[test]
    fn spawn_team_returns_err_on_invalid_config() {
        let s = session();
        let mem_home = tmp("spawn-err");
        let team = team_def("t", &[("x", "nope")]);
        let err = spawn_team(&s, &team, &[], &mem_home, &mem_home, "main")
            .unwrap_err()
            .to_string();
        assert!(err.contains("nope"), "{err}");
        assert!(
            s.agents.list().is_empty(),
            "failed validation has no side effects"
        );
        std::fs::remove_dir_all(&mem_home).unwrap();
    }

    #[test]
    fn memory_roundtrip_and_decision_append() {
        let home = tmp("mem");
        let project = home.join("proj");
        let branch = "agent-team";
        let team = "dev-room";
        let msgs = vec![
            crate::api::types::Message::user_text("round one"),
            crate::api::types::Message::user_text("round two"),
        ];
        save_member_history(&home, &project, branch, team, "dev", &msgs);
        let loaded = load_member_history(&home, &project, branch, team, "dev");
        assert_eq!(loaded.len(), 2, "roundtrip equality");
        assert_eq!(loaded[0].content, msgs[0].content);
        // Missing/corrupt falls back to empty.
        assert!(load_member_history(&home, &project, branch, team, "ghost").is_empty());
        // Decision log is append-only.
        append_decision(
            &home,
            &project,
            branch,
            team,
            "decision",
            "JSON, not YAML",
            &["dev", "qa"],
        );
        append_decision(
            &home,
            &project,
            branch,
            team,
            "decision",
            "second case",
            &["ui/ux"],
        );
        let raw = std::fs::read_to_string(decisions_path(&team_memory_dir(
            &home, &project, branch, team,
        )))
        .unwrap();
        assert_eq!(
            raw.matches("type: decision").count(),
            2,
            "two entries appended"
        );
        assert!(raw.contains("sources: dev|qa"), "pipe-separated sources");
        std::fs::remove_dir_all(&home).unwrap();
    }
}

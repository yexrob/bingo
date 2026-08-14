//! Agent teams: project-level squads (D31), organised as a tree (D54).
//!
//! Mental model: the team is the blueprint (persistent definition in
//! `.bingo/team.json`), the room is the construction site (runtime instances +
//! channels). This module = three thin layers: team.json parsing and validation
//! (validate and start share the same source: if validate passes, start must
//! succeed), `spawn_team`/`spawn_tree` orchestration (reuses the existing Agent
//! spawn + ChannelRegistry; idempotency key = instance name), and team memory
//! (key = project-path hash + branch, persisted across sessions and *pointed at*
//! rather than preloaded — see [`member_memory_note`], D51).
//!
//! Members reference AgentDefs rather than inlining personas — the single source
//! of truth for a persona stays in `.bingo/agents/<name>.md`; the team is only a
//! formation layer.
//!
//! A blueprint may name child blueprints in other directories (`teams`), so one
//! session at the root manages a whole org chart. Each node keeps its own agent
//! definitions, its own working agreement and its own memory, rooted at its own
//! directory — reaching a team from above is the same team you get by opening a
//! session inside it. Names (teams, members, rooms) are unique across the tree,
//! so `SendMessage` addresses a member by its bare name from anywhere.

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
pub const TEAM_SCHEMA_VERSION: u8 = 2;
/// The crew's working agreement (project-level `.bingo/team-norms.md`, checked into
/// version control beside the blueprint). Prose rather than a schema on purpose: it is
/// read by models and reviewed by people, and neither wants a config format (D53).
pub const NORMS_FILE: &str = ".bingo/team-norms.md";
pub const AVATAR_DIR: &str = ".bingo/assets/avatars";
/// Memory root directory: `~/.config/bingo/teams/`.
const TEAM_MEMORY_ROOT: &str = "teams";

pub(crate) fn lock_team_file(project_dir: &Path) -> Result<std::fs::File, TeamError> {
    let directory = project_dir.join(".bingo");
    std::fs::create_dir_all(&directory)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(directory.join(".team.lock"))?;
    file.lock()?;
    Ok(file)
}

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

pub fn import_avatar(project_dir: &Path, bytes: &[u8]) -> Result<String, TeamError> {
    let (id, encoded) = normalize_avatar(bytes)?;
    let hash = id
        .strip_prefix("project:")
        .ok_or_else(|| TeamError::invalid("normalized avatar id is invalid"))?;
    let dir = project_dir.join(AVATAR_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{hash}.png"));
    if !path.exists() {
        crate::storage::write_atomic(&path, &encoded)?;
    }
    Ok(id)
}

pub(crate) fn normalize_avatar(bytes: &[u8]) -> Result<(String, Vec<u8>), TeamError> {
    if bytes.is_empty() || bytes.len() > 20 * 1024 * 1024 {
        return Err(TeamError::invalid(
            "avatar image must be between 1 byte and 20 MiB",
        ));
    }
    let image = image::load_from_memory(bytes)
        .map_err(|error| TeamError::invalid(format!("avatar image is invalid: {error}")))?;
    let normalized = image.resize_to_fill(512, 512, image::imageops::FilterType::Lanczos3);
    let mut encoded = std::io::Cursor::new(Vec::new());
    normalized
        .write_to(&mut encoded, image::ImageFormat::Png)
        .map_err(|error| TeamError::invalid(format!("avatar normalization failed: {error}")))?;
    let encoded = encoded.into_inner();
    let hash = crate::update::sha256_hex(&encoded);
    let id = format!("project:{}", &hash[..24]);
    Ok((id, encoded))
}

pub fn project_avatar_ids(project_dir: &Path) -> Result<Vec<String>, TeamError> {
    let dir = project_dir.join(AVATAR_DIR);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut ids = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().is_some_and(|extension| extension == "png"))
                .then(|| {
                    path.file_stem()?
                        .to_str()
                        .map(|stem| format!("project:{stem}"))
                })
                .flatten()
        })
        .collect::<Vec<_>>();
    ids.sort();
    Ok(ids)
}

pub fn project_avatar_path(project_dir: &Path, id: &str) -> Option<PathBuf> {
    let hash = id.strip_prefix("project:")?;
    if hash.len() != 24 || !hash.chars().all(|character| character.is_ascii_hexdigit()) {
        return None;
    }
    Some(project_dir.join(AVATAR_DIR).join(format!("{hash}.png")))
}

pub fn team_avatar_thumbnail(tree: &TeamTree, id: &str, size: u32) -> Result<Vec<u8>, TeamError> {
    if size == 0 || size > 512 {
        return Err(TeamError::invalid("avatar thumbnail size must be 1 to 512"));
    }
    let path = tree
        .nodes()
        .iter()
        .find_map(|node| project_avatar_path(&node.dir, id).filter(|path| path.is_file()))
        .ok_or_else(|| TeamError::invalid("avatar is not available in the current team tree"))?;
    let bytes = std::fs::read(path)?;
    let image = image::load_from_memory(&bytes)
        .map_err(|error| TeamError::invalid(format!("avatar image is invalid: {error}")))?;
    let thumbnail = image.resize_to_fill(size, size, image::imageops::FilterType::Lanczos3);
    let mut output = std::io::Cursor::new(Vec::new());
    thumbnail
        .write_to(&mut output, image::ImageFormat::Png)
        .map_err(|error| TeamError::invalid(format!("avatar thumbnail failed: {error}")))?;
    Ok(output.into_inner())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn runtime_member_configuration_key(
    member_id: &str,
    agent: &str,
    system: &str,
    inherit_system: bool,
    provider: &str,
    model: &str,
    thinking: Option<&str>,
    profile: &MemberProfile,
) -> String {
    let value = serde_json::json!({
        "memberId": member_id,
        "agent": agent,
        "system": system,
        "inheritSystem": inherit_system,
        "provider": provider,
        "model": model,
        "thinking": thinking,
        "profile": profile,
    });
    crate::update::sha256_hex(&serde_json::to_vec(&value).unwrap_or_default())
}

/// How deep a team tree may go. A cap rather than none, because a blueprint is a
/// hand-written file and a runaway chain of them is a mistake worth naming early.
const MAX_TEAM_DEPTH: usize = 8;

/// Room spec for the shorthand form: a team that declares no `channels` gets one
/// room, named after the team, holding every member (reuses the existing Channel
/// vocabulary; no new concepts invented).
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

/// One named room a team declares. A team may hold several, each with its own
/// roster — a department has a standup, a release channel and a design review,
/// and the same person is in some of them and not others.
///
/// `members` reaches the declaring team and any team below it, never a parent or
/// a sibling: a manager may convene their own subtree, a peer may not conscript
/// another department. That is also what keeps a subtree loadable on its own —
/// open a session inside it and it validates unchanged.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(
        rename = "messageLimit",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub message_limit: Option<u64>,
    /// Who is in this room. Absent means every member of the declaring team.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub members: Option<Vec<String>>,
}

/// A child team, by where its blueprint lives. The path is relative to the
/// declaring team's own directory — a committed org chart has to travel with the
/// repo, so a path starting at a filesystem root is refused (absolute *or* merely
/// rooted: on Windows `/etc/x` is the second and not the first).
///
/// It may point at the directory that holds a blueprint (`repos/marketing`) or at
/// the blueprint itself (`repos/marketing/.bingo/team.json`); both name the same
/// team. `name`, when given, is checked against the child's own name: an org chart
/// that disagrees with the blueprint it points at is worse than one with no labels.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub path: String,
}

/// A room as the runtime needs it: resolved name, mode, budget and roster,
/// whether it came from the `channels` list or from the one-room shorthand.
#[derive(Debug, Clone, PartialEq)]
pub struct Room {
    pub name: String,
    pub mode: ChannelMode,
    pub message_limit: Option<u64>,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberIdentity {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberCommunication {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verbosity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}

fn prompt_enforcement() -> String {
    "prompt".to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BehaviorConstraint {
    pub kind: String,
    pub instruction: String,
    #[serde(default = "prompt_enforcement")]
    pub enforcement: String,
}

impl Default for BehaviorConstraint {
    fn default() -> Self {
        Self {
            kind: "custom".to_string(),
            instruction: String::new(),
            enforcement: prompt_enforcement(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<MemberIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub communication: Option<MemberCommunication>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<BehaviorConstraint>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preferences: Vec<String>,
}

impl MemberProfile {
    pub fn merged(defaults: &Self, overrides: &Self) -> Self {
        let identity = merge_identity(defaults.identity.as_ref(), overrides.identity.as_ref());
        let communication = merge_communication(
            defaults.communication.as_ref(),
            overrides.communication.as_ref(),
        );
        let mut constraints = defaults.constraints.clone();
        constraints.extend(overrides.constraints.clone());
        let mut preferences = defaults.preferences.clone();
        preferences.extend(overrides.preferences.clone());
        Self {
            identity,
            personality: non_empty(overrides.personality.clone())
                .or_else(|| non_empty(defaults.personality.clone())),
            communication,
            constraints: dedupe_constraints(constraints),
            preferences: dedupe_strings(preferences),
        }
    }

    pub fn with_constraints(&self, constraints: &[BehaviorConstraint]) -> Self {
        let mut profile = self.clone();
        profile.constraints.extend_from_slice(constraints);
        profile.constraints = dedupe_constraints(profile.constraints);
        profile
    }

    pub fn prompt_block(&self, member_name: &str) -> Option<String> {
        let mut lines = vec![format!("Fixed team member profile for {member_name}:")];
        if let Some(identity) = &self.identity {
            if let Some(title) = non_empty(identity.title.clone()) {
                lines.push(format!("Identity title: {title}"));
            }
            if let Some(background) = non_empty(identity.background.clone()) {
                lines.push(format!("Identity background: {background}"));
            }
        }
        if let Some(personality) = non_empty(self.personality.clone()) {
            lines.push(format!("Personality: {personality}"));
        }
        if let Some(communication) = &self.communication {
            if let Some(language) = non_empty(communication.language.clone()) {
                lines.push(format!("Conversation language: {language}"));
            }
            if let Some(tone) = non_empty(communication.tone.clone()) {
                lines.push(format!("Conversation tone: {tone}"));
            }
            if let Some(verbosity) = non_empty(communication.verbosity.clone()) {
                lines.push(format!("Conversation verbosity: {verbosity}"));
            }
            if let Some(instructions) = non_empty(communication.instructions.clone()) {
                lines.push(format!("Conversation style instructions: {instructions}"));
            }
        }
        let constraints = self
            .constraints
            .iter()
            .filter_map(|constraint| non_empty(Some(constraint.instruction.clone())))
            .collect::<Vec<_>>();
        if !constraints.is_empty() {
            lines.push(
                "MUST behavior constraints (prompt guidance, not a security sandbox):".to_string(),
            );
            lines.extend(constraints.into_iter().map(|value| format!("- {value}")));
            lines.push(
                "If a task requires breaking one of these constraints, stop and report the conflict to the user."
                    .to_string(),
            );
        }
        let preferences = self
            .preferences
            .iter()
            .filter_map(|preference| non_empty(Some(preference.clone())))
            .collect::<Vec<_>>();
        if !preferences.is_empty() {
            lines.push("SHOULD working preferences:".to_string());
            lines.extend(preferences.into_iter().map(|value| format!("- {value}")));
        }
        (lines.len() > 1).then(|| lines.join("\n"))
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn merge_identity(
    defaults: Option<&MemberIdentity>,
    overrides: Option<&MemberIdentity>,
) -> Option<MemberIdentity> {
    let value = MemberIdentity {
        title: overrides
            .and_then(|value| non_empty(value.title.clone()))
            .or_else(|| defaults.and_then(|value| non_empty(value.title.clone()))),
        background: overrides
            .and_then(|value| non_empty(value.background.clone()))
            .or_else(|| defaults.and_then(|value| non_empty(value.background.clone()))),
    };
    (value.title.is_some() || value.background.is_some()).then_some(value)
}

fn merge_communication(
    defaults: Option<&MemberCommunication>,
    overrides: Option<&MemberCommunication>,
) -> Option<MemberCommunication> {
    let choose = |field: fn(&MemberCommunication) -> &Option<String>| {
        overrides
            .and_then(|value| non_empty(field(value).clone()))
            .or_else(|| defaults.and_then(|value| non_empty(field(value).clone())))
    };
    let value = MemberCommunication {
        language: choose(|value| &value.language),
        tone: choose(|value| &value.tone),
        verbosity: choose(|value| &value.verbosity),
        instructions: choose(|value| &value.instructions),
    };
    (value.language.is_some()
        || value.tone.is_some()
        || value.verbosity.is_some()
        || value.instructions.is_some())
    .then_some(value)
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    values
        .into_iter()
        .filter_map(|value| non_empty(Some(value)))
        .filter(|value| seen.insert(value.clone()))
        .collect()
}

fn dedupe_constraints(values: Vec<BehaviorConstraint>) -> Vec<BehaviorConstraint> {
    let mut seen = std::collections::HashSet::new();
    values
        .into_iter()
        .filter_map(|mut value| {
            value.kind = non_empty(Some(value.kind)).unwrap_or_else(|| "custom".to_string());
            value.instruction = non_empty(Some(value.instruction))?;
            value.enforcement = "prompt".to_string();
            Some(value)
        })
        .filter(|value| seen.insert((value.kind.clone(), value.instruction.clone())))
        .collect()
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
#[serde(rename_all = "camelCase")]
pub struct TeamMember {
    #[serde(default, alias = "member_id", skip_serializing_if = "String::is_empty")]
    pub member_id: String,
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
    #[serde(default, skip_serializing_if = "profile_is_empty")]
    pub profile: MemberProfile,
}

pub(crate) fn profile_is_empty(profile: &MemberProfile) -> bool {
    profile == &MemberProfile::default()
}

/// Team definition (blueprint). Parsing and writing share this one struct: the file
/// format has a single source, so a written blueprint reads back as the same value.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamDef {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub team_id: String,
    pub name: String,
    /// Default task leader. Omitted blueprints fall back to the first selected member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leader: Option<String>,
    /// One-room shorthand, used only when `channels` is empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<ChannelSpec>,
    /// Declared rooms. Non-empty means these are the team's rooms — there is no
    /// additional room named after the team, because a room nobody asked for is
    /// one nobody reads.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<ChannelDef>,
    pub members: Vec<TeamMember>,
    /// Child teams, by blueprint location (recursive).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub teams: Vec<TeamRef>,
}

/// The rooms a team opens: its declared `channels`, or — when it declares none —
/// the one room named after the team holding every member. One resolver so the
/// runtime, the views and validation never disagree about what a blueprint means.
pub fn rooms(def: &TeamDef) -> Vec<Room> {
    let everyone = || def.members.iter().map(|m| m.name.clone()).collect();
    if def.channels.is_empty() {
        return vec![Room {
            name: def.name.clone(),
            mode: channel_mode(def),
            message_limit: def.channel.as_ref().and_then(|s| s.message_limit),
            members: everyone(),
        }];
    }
    def.channels
        .iter()
        .map(|c| Room {
            name: c.name.clone(),
            mode: c
                .mode
                .as_deref()
                .and_then(|m| ChannelMode::parse(m).ok())
                .unwrap_or(ChannelMode::Serial),
            message_limit: c.message_limit,
            members: c.members.clone().unwrap_or_else(everyone),
        })
        .collect()
}

/// Parse `.bingo/team.json`: returns Ok(None) if absent; otherwise parses + structural
/// validation (invalid types/enums are errors). Existence of referenced AgentDefs is
/// checked by `validate` (needs the loaded definition list; not done in pure parsing),
/// and child teams are followed by [`load_team_tree`] — this reads one file.
pub fn load_team_file(project_dir: &Path) -> Result<Option<TeamDef>, TeamError> {
    read_team_file(&project_dir.join(TEAM_FILE))
}

/// One blueprint, by file path (the tree loader's unit: errors name the file that
/// actually holds the mistake, which in a tree is rarely the one you opened).
fn read_team_file(path: &Path) -> Result<Option<TeamDef>, TeamError> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(&raw)?;
    let schema_version = value
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    if schema_version != 1 && schema_version != u64::from(TEAM_SCHEMA_VERSION) {
        return Err(TeamError::invalid(format!(
            "{}: schemaVersion: unsupported version {schema_version} (this bingo supports versions 1 and {TEAM_SCHEMA_VERSION})",
            path.display()
        )));
    }
    let mut def: TeamDef = serde_json::from_value(value)?;
    migrate_team_ids(&mut def, path, schema_version);
    validate_structure(&def, path)?;
    Ok(Some(def))
}

fn migrate_team_ids(def: &mut TeamDef, path: &Path, schema_version: u64) {
    if schema_version == u64::from(TEAM_SCHEMA_VERSION) {
        return;
    }
    if def.team_id.trim().is_empty() {
        def.team_id = stable_blueprint_id("team", path, &def.name);
    }
    for member in &mut def.members {
        if member.member_id.trim().is_empty() {
            member.member_id = stable_blueprint_id("member", path, &member.name);
        }
    }
}

fn stable_blueprint_id(kind: &str, path: &Path, name: &str) -> String {
    let source = format!("{kind}\0{}\0{name}", path.display());
    format!(
        "{kind}-{}",
        crate::update::sha256_hex(source.as_bytes())
            .chars()
            .take(24)
            .collect::<String>()
    )
}

/// Structural validation (no AgentDef list needed): name/channel mode/member constraints.
/// Shares the error format with `validate` (three parts: file path + field path + expectation).
pub(crate) fn validate_structure(def: &TeamDef, path: &Path) -> Result<(), TeamError> {
    let file = path.display();
    if !valid_stable_id(&def.team_id) {
        return Err(TeamError::invalid(format!(
            "{file}: teamId: must contain 1-128 letters, numbers, hyphens, or underscores"
        )));
    }
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
    let mut member_ids = std::collections::HashSet::new();
    for (i, m) in def.members.iter().enumerate() {
        if !valid_stable_id(&m.member_id) {
            return Err(TeamError::invalid(format!(
                "{file}: members[{i}].memberId: must contain 1-128 letters, numbers, hyphens, or underscores"
            )));
        }
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
        if !member_ids.insert(m.member_id.as_str()) {
            return Err(TeamError::invalid(format!(
                "{file}: members[{i}].memberId: duplicate \"{}\" within the config",
                m.member_id
            )));
        }
        validate_profile(&m.profile, &format!("{file}: members[{i}].profile"))?;
    }
    if let Some(leader) = def.leader.as_deref()
        && !def.members.iter().any(|member| member.name == leader)
    {
        return Err(TeamError::invalid(format!(
            "{file}: leader: \"{leader}\" must name a root team member"
        )));
    }
    let mut rooms_seen = std::collections::HashSet::new();
    for (i, c) in def.channels.iter().enumerate() {
        if c.name.trim().is_empty() {
            return Err(TeamError::invalid(format!(
                "{file}: channels[{i}].name: must not be empty (a room is addressed by name)"
            )));
        }
        if !rooms_seen.insert(c.name.as_str()) {
            return Err(TeamError::invalid(format!(
                "{file}: channels[{i}].name: duplicate \"{}\" within the config (room names must be unique)",
                c.name
            )));
        }
        if let Some(mode) = &c.mode {
            ChannelMode::parse(mode)
                .map_err(|e| TeamError::invalid(format!("{file}: channels[{i}].mode: {e}")))?;
        }
        if let Some(limit) = c.message_limit
            && limit == 0
        {
            return Err(TeamError::invalid(format!(
                "{file}: channels[{i}].messageLimit: must be a positive integer"
            )));
        }
        // An explicit empty roster is a room nobody can speak in; an omitted one
        // means "the whole team", which is the useful default. They are different
        // statements, so only the first is refused.
        if let Some(members) = &c.members {
            if members.is_empty() {
                return Err(TeamError::invalid(format!(
                    "{file}: channels[{i}].members: must not be empty (omit the field to put the whole team in the room)"
                )));
            }
            for (j, m) in members.iter().enumerate() {
                if m.trim().is_empty() {
                    return Err(TeamError::invalid(format!(
                        "{file}: channels[{i}].members[{j}]: must not be empty"
                    )));
                }
            }
        }
    }
    for (i, t) in def.teams.iter().enumerate() {
        if t.path.trim().is_empty() {
            return Err(TeamError::invalid(format!(
                "{file}: teams[{i}].path: must not be empty (name the directory holding the child blueprint, or the file itself)"
            )));
        }
        // `has_root` as well as `is_absolute`, because they disagree exactly where it
        // matters: on Windows "/etc/team.json" is rooted but not absolute (it has no
        // drive), and letting it through would mean the rule held on one platform and
        // not the other for the same committed file.
        let path = Path::new(t.path.trim());
        if path.is_absolute() || path.has_root() {
            return Err(TeamError::invalid(format!(
                "{file}: teams[{i}].path: \"{}\" starts at a filesystem root; use a path relative to this team's directory (a committed org chart has to travel with the repo)",
                t.path
            )));
        }
    }
    Ok(())
}

fn valid_stable_id(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.chars().count() <= 128
        && value
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_'))
}

pub fn validate_profile(profile: &MemberProfile, field: &str) -> Result<(), TeamError> {
    if let Some(verbosity) = profile
        .communication
        .as_ref()
        .and_then(|communication| communication.verbosity.as_deref())
        && !matches!(verbosity, "concise" | "balanced" | "detailed")
    {
        return Err(TeamError::invalid(format!(
            "{field}.communication.verbosity: expected concise, balanced, or detailed"
        )));
    }
    for (index, constraint) in profile.constraints.iter().enumerate() {
        if !matches!(
            constraint.kind.as_str(),
            "noNetwork" | "noShell" | "readOnly" | "reviewOnly" | "custom"
        ) {
            return Err(TeamError::invalid(format!(
                "{field}.constraints[{index}].kind: unsupported behavior constraint kind"
            )));
        }
        if constraint.instruction.trim().is_empty() {
            return Err(TeamError::invalid(format!(
                "{field}.constraints[{index}].instruction: must not be empty"
            )));
        }
        if constraint.enforcement != "prompt" {
            return Err(TeamError::invalid(format!(
                "{field}.constraints[{index}].enforcement: only prompt is supported"
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
    let _lock = lock_team_file(project_dir)?;
    let path = project_dir.join(TEAM_FILE);
    let mut def = def.clone();
    migrate_team_ids(&mut def, &path, 1);
    validate_structure(&def, &path)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut value = serde_json::to_value(def)?;
    value["schemaVersion"] = serde_json::Value::from(TEAM_SCHEMA_VERSION);
    let mut json = serde_json::to_string_pretty(&value)?;
    json.push('\n');
    crate::storage::write_atomic(&path, json.as_bytes())?;
    Ok(())
}

// ---- the team tree (D54) ----

/// One team in the tree: its blueprint plus where that blueprint lives. The
/// directory is the node's own project root — its agent definitions, its working
/// agreement and its memory all hang off it, so a team reached from above is the
/// same team you get by opening a session inside it.
#[derive(Debug, Clone)]
pub struct TeamNode {
    pub def: TeamDef,
    pub dir: PathBuf,
    pub file: PathBuf,
    /// 0 for the root; a child is one deeper than its parent.
    pub depth: usize,
}

/// A loaded org chart, in depth-first pre-order with the root first. Pre-order is
/// load-bearing rather than cosmetic: a node's subtree is the contiguous run that
/// follows it, which is what makes "a room reaches its own subtree" a slice check
/// instead of a graph walk.
#[derive(Debug, Clone)]
pub struct TeamTree {
    nodes: Vec<TeamNode>,
}

impl TeamTree {
    pub fn nodes(&self) -> &[TeamNode] {
        &self.nodes
    }

    /// The team the session is rooted at. Every tree has one — the loader returns
    /// `None` rather than an empty tree when there is no blueprint at all.
    pub fn root(&self) -> &TeamNode {
        match self.nodes.first() {
            Some(node) => node,
            // Unreachable by construction; a panic here would be a crash in the
            // one place that must never crash, so the empty tree gets a name.
            None => &EMPTY_NODE,
        }
    }

    /// Every member in the tree, with the team it belongs to.
    pub fn members(&self) -> impl Iterator<Item = (&TeamNode, &TeamMember)> {
        self.nodes
            .iter()
            .flat_map(|n| n.def.members.iter().map(move |m| (n, m)))
    }

    /// Where a member sits, by its bare name — names are unique across the tree,
    /// which is exactly what lets `SendMessage` address anyone from anywhere.
    pub fn find_member(&self, name: &str) -> Option<(&TeamNode, &TeamMember)> {
        self.members().find(|(_, m)| m.name == name)
    }

    /// Node `index` and everything under it.
    pub fn subtree(&self, index: usize) -> Vec<&TeamNode> {
        let Some(root) = self.nodes.get(index) else {
            return Vec::new();
        };
        std::iter::once(root)
            .chain(
                self.nodes[index + 1..]
                    .iter()
                    .take_while(|n| n.depth > root.depth),
            )
            .collect()
    }

    /// Every room in the tree, with the team that declared it.
    pub fn rooms(&self) -> impl Iterator<Item = (&TeamNode, Room)> {
        self.nodes
            .iter()
            .flat_map(|n| rooms(&n.def).into_iter().map(move |r| (n, r)))
    }
}

static EMPTY_NODE: std::sync::LazyLock<TeamNode> = std::sync::LazyLock::new(|| TeamNode {
    def: TeamDef::default(),
    dir: PathBuf::new(),
    file: PathBuf::new(),
    depth: 0,
});

/// Load the whole org chart rooted at this project: the blueprint here, then every
/// blueprint it names, recursively. `Ok(None)` when this project pins no team.
///
/// Everything a tree can be wrong about is caught here rather than at spawn: a
/// reference to a blueprint that isn't there, a cycle, a chart deeper than
/// [`MAX_TEAM_DEPTH`], a name used twice, a room reaching outside its own subtree.
/// Reference validation of members against agent definitions stays with
/// [`validate_tree`], which needs a session to judge engines against.
pub fn load_team_tree(project_dir: &Path) -> Result<Option<TeamTree>, TeamError> {
    let Some(def) = read_team_file(&project_dir.join(TEAM_FILE))? else {
        return Ok(None);
    };
    Ok(Some(build_tree(def, project_dir)?))
}

/// The tree a blueprint would root, without it having to be on disk yet: children are
/// read from disk, this one is the value in hand. What `Team save` checks before it
/// writes, so a rewrite that would break the chart is refused rather than persisted
/// and then complained about.
pub fn build_tree(mut def: TeamDef, project_dir: &Path) -> Result<TeamTree, TeamError> {
    let file = project_dir.join(TEAM_FILE);
    migrate_team_ids(&mut def, &file, 1);
    validate_structure(&def, &file)?;
    let mut nodes = Vec::new();
    let mut visited = std::collections::HashSet::new();
    visited.insert(std::fs::canonicalize(&file).unwrap_or_else(|_| file.clone()));
    add_node(
        &mut nodes,
        &mut visited,
        def,
        project_dir.to_path_buf(),
        file,
        0,
    )?;
    let tree = TeamTree { nodes };
    check_unique_names(&tree)?;
    check_room_scope(&tree)?;
    Ok(tree)
}

/// Where a child reference points: (project directory, blueprint file). A path
/// ending in `.json` is the blueprint; anything else is the directory holding one.
fn child_paths(parent_dir: &Path, raw: &str) -> (PathBuf, PathBuf) {
    let trimmed = raw.trim();
    let joined = parent_dir.join(trimmed);
    if !trimmed.ends_with(".json") {
        let file = joined.join(TEAM_FILE);
        return (joined, file);
    }
    // `<dir>/.bingo/team.json` belongs to `<dir>`; a blueprint kept anywhere else
    // belongs to the directory it sits in.
    let dir = match joined.parent() {
        Some(p) if p.file_name() == Some(std::ffi::OsStr::new(".bingo")) => {
            p.parent().unwrap_or(p).to_path_buf()
        }
        Some(p) => p.to_path_buf(),
        None => parent_dir.to_path_buf(),
    };
    (dir, joined)
}

fn add_node(
    nodes: &mut Vec<TeamNode>,
    visited: &mut std::collections::HashSet<PathBuf>,
    def: TeamDef,
    dir: PathBuf,
    file: PathBuf,
    depth: usize,
) -> Result<(), TeamError> {
    let refs = def.teams.clone();
    nodes.push(TeamNode {
        def,
        dir: dir.clone(),
        file: file.clone(),
        depth,
    });
    let here = file.display();
    for (i, child) in refs.iter().enumerate() {
        if depth + 1 >= MAX_TEAM_DEPTH {
            return Err(TeamError::invalid(format!(
                "{here}: teams[{i}]: the team tree is deeper than {MAX_TEAM_DEPTH} levels (a chart this deep is usually a reference pointing back into itself)"
            )));
        }
        let (child_dir, child_file) = child_paths(&dir, &child.path);
        let Some(child_def) = read_team_file(&child_file)? else {
            return Err(TeamError::invalid(format!(
                "{here}: teams[{i}].path: no blueprint at {} (\"{}\" must name the directory holding a {TEAM_FILE}, or that file itself)",
                child_file.display(),
                child.path
            )));
        };
        let canonical =
            std::fs::canonicalize(&child_file).unwrap_or_else(|_| child_file.to_path_buf());
        if !visited.insert(canonical) {
            return Err(TeamError::invalid(format!(
                "{here}: teams[{i}].path: {} is already in this tree (a team cannot contain itself, directly or through a cycle)",
                child_file.display()
            )));
        }
        if let Some(label) = child
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            && label != child_def.name
        {
            return Err(TeamError::invalid(format!(
                "{here}: teams[{i}].name: says \"{label}\" but {} is named \"{}\" (an org chart that disagrees with the blueprint it points at is worse than one with no labels — fix one of them)",
                child_file.display(),
                child_def.name
            )));
        }
        add_node(nodes, visited, child_def, child_dir, child_file, depth + 1)?;
    }
    Ok(())
}

/// Teams, members and rooms are each unique across the whole tree.
///
/// This is what buys bare-name addressing: `SendMessage` takes "Linh", not
/// "研发部/Linh", from any session in the chart. It is also not optional at the
/// runtime level — the agent registry and the channel registry are flat maps, so
/// two teams claiming one name would silently be one entry.
fn check_unique_names(tree: &TeamTree) -> Result<(), TeamError> {
    let mut teams: HashMap<&str, &Path> = HashMap::new();
    let mut members: HashMap<&str, &Path> = HashMap::new();
    let mut member_ids: HashMap<&str, &Path> = HashMap::new();
    let mut rooms_seen: HashMap<String, &Path> = HashMap::new();
    let clash = |what: &str, name: &str, first: &Path, second: &Path, why: &str| {
        TeamError::invalid(format!(
            "{}: duplicate {what} \"{name}\", already declared in {} ({why})",
            second.display(),
            first.display()
        ))
    };
    for node in &tree.nodes {
        if let Some(first) = teams.insert(&node.def.name, &node.file) {
            return Err(clash(
                "team name",
                &node.def.name,
                first,
                &node.file,
                "team names are unique across the tree: they name a memory partition and a row in every status view",
            ));
        }
        for m in &node.def.members {
            if let Some(first) = members.insert(&m.name, &node.file) {
                return Err(clash(
                    "member",
                    &m.name,
                    first,
                    &node.file,
                    "member names are unique across the tree: a name is how SendMessage reaches a member from anywhere in it",
                ));
            }
            if let Some(first) = member_ids.insert(&m.member_id, &node.file) {
                return Err(clash(
                    "memberId",
                    &m.member_id,
                    first,
                    &node.file,
                    "stable member identities are unique across the tree so experience and task occupancy cannot cross-link",
                ));
            }
        }
        for room in rooms(&node.def) {
            if let Some(first) = rooms_seen.insert(room.name.clone(), &node.file) {
                return Err(clash(
                    "room",
                    &room.name,
                    first,
                    &node.file,
                    "room names are unique across the tree: a channel is addressed by name and there is one channel registry",
                ));
            }
        }
    }
    Ok(())
}

/// A room reaches its own team and the teams below it — never a parent, never a
/// sibling. A manager may convene their subtree; a peer may not conscript another
/// department. It is also what keeps a subtree loadable on its own: open a session
/// inside a child and its rooms still resolve.
fn check_room_scope(tree: &TeamTree) -> Result<(), TeamError> {
    for (index, node) in tree.nodes.iter().enumerate() {
        let reachable: std::collections::HashSet<&str> = tree
            .subtree(index)
            .into_iter()
            .flat_map(|n| n.def.members.iter().map(|m| m.name.as_str()))
            .collect();
        for (i, channel) in node.def.channels.iter().enumerate() {
            for (j, name) in channel.members.iter().flatten().enumerate() {
                if reachable.contains(name.as_str()) {
                    continue;
                }
                let elsewhere = tree
                    .find_member(name)
                    .map(|(owner, _)| {
                        format!(
                            "\"{name}\" is on {} ({}), which is not under {} — a room reaches its own team and the teams below it, never a parent or a sibling",
                            owner.def.name,
                            owner.file.display(),
                            node.def.name
                        )
                    })
                    .unwrap_or_else(|| {
                        format!("no member named \"{name}\" anywhere in this team tree")
                    });
                return Err(TeamError::invalid(format!(
                    "{}: channels[{i}].members[{j}]: {elsewhere}",
                    node.file.display()
                )));
            }
        }
    }
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
    validate_at(def, defs, session, TEAM_FILE)
}

/// [`validate`] with the file it is judging named explicitly — in a tree the
/// blueprint holding the mistake is rarely the one the session was opened at, so
/// the error has to say which file to go and edit.
fn validate_at(
    def: &TeamDef,
    defs: &[AgentDef],
    session: &Session,
    file: &str,
) -> Result<(), TeamError> {
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
                "{file}: members[{i}].agent: references a non-existent AgentDef \"{}\"; {hint}",
                m.agent
            )));
        };
        let field = |name: &str| format!("{file}: members[{i}].{name}");
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

/// [`validate`] over the whole tree. Each node is judged against *its own* agent
/// definitions: a department's members play roles written in that department's
/// `.bingo/agents/`, which is what makes a subtree a thing you can move.
pub fn validate_tree(tree: &TeamTree, session: &Session, home: &Path) -> Result<(), TeamError> {
    for node in &tree.nodes {
        let defs = crate::agents::load_agent_defs(home, &node.dir);
        validate_at(&node.def, &defs, session, &node.file.display().to_string())?;
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

// ---- team norms: the crew's working agreement (D53) ----

/// The starter agreement `/team new` writes beside a fresh blueprint. A norms file
/// nobody writes is a feature that never runs, and an empty template is one nobody
/// edits — so the scaffold ships with rules worth keeping and a header saying they
/// are meant to be rewritten.
pub const NORMS_TEMPLATE: &str = "\
# Team norms

The working agreement for this project's crew. Every member carries it from the moment
it spawns, and so does anyone hired for a single task. Edit it — this file is a starting
point, not a standard.

## Working agreement

- Report outcomes as they are. Say what you ran, what passed, what you did not check.
  Unverified work is not finished work.
- Stay inside the task you were given. Something else that needs doing is worth naming
  in your reply, not fixing on the way past.
- Say it once, to the person who needs it. Do not restate a colleague's conclusion, and
  do not report progress nobody is blocked on.
- When you are stuck, say what you tried and what you need. Silence reads as a hang.
- Follow the shape of the code and the docs already here before introducing your own.
";

/// The crew's working agreement, or None when this project has not written one.
/// Whitespace-only counts as absent: a file of blank lines is not an agreement, and
/// injecting it would spend context saying nothing.
pub fn load_norms(project_dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(project_dir.join(NORMS_FILE)).ok()?;
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Write the starter agreement, leaving an existing one alone. Returns whether it wrote.
pub fn write_norms_template(project_dir: &Path) -> Result<bool, TeamError> {
    let path = project_dir.join(NORMS_FILE);
    if path.exists() {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, NORMS_TEMPLATE)?;
    Ok(true)
}

/// The agreement as the system block every member carries.
///
/// It lives in the system prompt rather than in the wake-up payload for the reason
/// `CHANNEL_NOTE` gives (D48): compaction rewrites the message history and never touches
/// `Session::system`, so the norms are still there on turn fifty.
///
/// The precedence line is the load-bearing part. Norms that outrank an instruction
/// would make the crew unusable, and norms an instruction silently voids are
/// decoration — so the block says exactly which one wins, and that everything the
/// instruction did not speak to still holds.
pub fn norms_block(team: &str, norms: &str) -> String {
    format!(
        "# Team norms ({team})\n\n\
         The crew's working agreement, from {NORMS_FILE} in this project. It applies to \
         every turn you take here without being repeated to you.\n\n\
         A direct instruction outranks it: when the task you are given says otherwise, do \
         what the task says. That exception is narrow — it covers the point the instruction \
         actually makes, and every other norm still holds. Nothing here licenses ignoring \
         the agreement because it is inconvenient.\n\n{norms}"
    )
}

/// What the hub is told about the crew standing behind it: who is on it, which rooms
/// reach whom, and the rule that decides between giving a member work and hiring
/// someone new.
///
/// Without this the crew is invisible at the moment it matters. The hub sees a list of
/// *agent definitions* in the Agent tool's description and spawns from it, so a pinned
/// crew — already spawned, already carrying this branch's memory, already paid for —
/// sits idle while a fresh subagent redoes what a member knows.
///
/// The whole tree is named, not just the root: a department the hub cannot see is a
/// department it will re-hire from scratch. Each team is listed under its own directory,
/// because that is where its work is and a member of it is not sitting in the session's
/// cwd.
pub fn crew_note(tree: &TeamTree, home: &Path) -> String {
    let root = tree.root();
    let mut roster = String::new();
    for node in tree.nodes() {
        let defs = crate::agents::load_agent_defs(home, &node.dir);
        if node.depth > 0 {
            let dir = relative_dir(&root.dir, &node.dir);
            roster.push_str(&format!("\n## {} — {dir}\n", node.def.name));
        }
        for m in view(&node.def, &defs).members {
            roster.push_str(&format!("- {} — {}\n", m.name, m.description));
        }
    }
    let rooms_line: Vec<String> = tree
        .rooms()
        .map(|(_, r)| format!("#{} ({})", r.name, r.members.join(", ")))
        .collect();
    let subtree_note = if tree.nodes().len() > 1 {
        format!(
            " It is the root of {} teams declared across the tree in `teams`; every one of them \
             is listed below and every member is addressed by its bare name, from here, with no \
             team prefix.",
            tree.nodes().len()
        )
    } else {
        String::new()
    };
    let norms = if load_norms(&root.dir).is_some() {
        format!(
            " The crew works to the agreement in {NORMS_FILE}, which every member carries; \
             read it before you overrule it."
        )
    } else {
        String::new()
    };
    format!(
        "# This project has a standing crew\n\n\
         `{}` is pinned to this project in {TEAM_FILE}.{subtree_note} Its members stand by idle \
         at zero tokens once the crew is up — which happens at startup unless it was turned off, \
         and the Team tool's `start` brings it up either way. They are the workforce here, not a \
         fallback.{norms}\n\n{roster}\nRooms: {}\n\n\
         - **Give the work to a member first.** Match the job to the roster above and send it \
         with SendMessage. A member wakes with its own persona, its own engine and its own \
         memory of this branch; spawning a fresh subagent for work a member covers leaves the \
         crew idle and throws that away.\n\
         - **Hire from outside only for what no member covers.** A hire serves the one task: \
         it never enters {TEAM_FILE}, it does not join a room, and it is released once its \
         result is in. When you hire, say in your reply which member's scope the work fell \
         outside of.\n\
         - **Who is on the crew is the user's decision.** Propose a change and let the Team \
         tool ask; do not route around it by hiring a permanent-looking stand-in.",
        root.def.name,
        rooms_line.join(" · ")
    )
}

/// A child team's directory as the root sees it — the path a reader can act on.
/// Falls back to the absolute path when the child is not under the root (a sibling
/// repo reached with `..`), which is still a path a reader can act on.
fn relative_dir(root: &Path, dir: &Path) -> String {
    dir.strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| dir.display().to_string())
}

/// What a member of a team rooted elsewhere is told about where its work is.
pub fn team_root_note(team: &str, dir: &Path) -> String {
    format!(
        "# Your team is rooted at {}\n\n\
         You are on `{team}`, whose blueprint, agent definitions and working agreement live in \
         that directory. It is this session's working directory, so relative tool paths resolve \
         from there. Work outside it only when the task you were given says so.",
        dir.display()
    )
}

/// What a temporary hire is told about its own standing. The crew note tells the hub how
/// to treat a hire; this tells the hire, so "temporary" is a fact it can plan against
/// rather than a bookkeeping detail it never learns.
pub fn hire_note(team: &str) -> String {
    format!(
        "# You are a temporary hire\n\n\
         This project has a standing crew ({team}) and you are not on it. You were brought in \
         for one task because no member covered it: you are not written into {TEAM_FILE}, you \
         are not in the crew's channel, and you are released once your result is in and the hub \
         has had its chance to follow up. Put everything worth keeping in your final text — \
         there is no next session in which you are asked again."
    )
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

/// Current Git scope: branch name, detached commit SHA, or `no-git`.
pub fn current_branch(project_dir: &Path) -> String {
    let inside = std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .is_some_and(|value| value.trim() == "true");
    if !inside {
        return "no-git".to_string();
    }
    if let Some(branch) = std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["branch", "--show-current"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        return branch;
    }
    std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "no-git".to_string())
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

// ---- spawn orchestration (spawn_team / spawn_tree) ----

/// Summary of one spawn (shared by /team start output and the event log).
#[derive(Debug, Clone, Default)]
pub struct SpawnSummary {
    /// Newly spawned instance names.
    pub spawned: Vec<String>,
    /// Reused existing instances (idempotent: not re-spawned, definition unchanged).
    pub reused: Vec<String>,
    /// Existing instances whose definition had moved and was re-read (D69): same
    /// instance, same history, new system prompt/model/provider/thinking.
    pub refreshed: Vec<String>,
    /// Failed members: (instance name, reason).
    pub failed: Vec<(String, String)>,
}

impl SpawnSummary {
    /// Every member the start left standing, whatever it had to do to get there.
    pub fn ready(&self) -> usize {
        self.spawned.len() + self.reused.len() + self.refreshed.len()
    }

    /// Event wording (QA acceptance: `spawned ×N` / `refreshed ×N` / `reused ×N` are
    /// greppable/assertable).
    pub fn events(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.spawned.is_empty() {
            out.push(format!("spawned ×{}", self.spawned.len()));
        }
        if !self.refreshed.is_empty() {
            out.push(format!("refreshed ×{}", self.refreshed.len()));
        }
        if !self.reused.is_empty() {
            out.push(format!("reused ×{}", self.reused.len()));
        }
        out
    }
}

/// Spawn the org chart (D31 for one team, D54 for a tree): every team's members
/// first, then every team's rooms. "Spawn ≠ wake" — members sit in Idle standby,
/// zero tokens, zero turns, only starting on SendMessage/channel messages; memory is
/// a pointer rather than a preload (D51). Member-level failure isolation: one failure
/// doesn't sink the rest; the failed member stays in `failed` and can be re-spawned
/// alone. Returns `Err` only on config validation failure (validate and start share
/// one source, so a chart that validates is one that starts).
///
/// The two phases are not cosmetic — a parent may convene a room holding a child's
/// members, so no room can be opened until the tree has finished spawning.
///
/// Start is also where an instance already up catches up with its files (D69): a member
/// that is not mid-turn has its definition re-read and re-applied, keeping the history it
/// has built. Editing a member used to mean deleting it and losing that history.
///
/// Each node is spawned against its own directory: its agent definitions, its
/// working agreement, its git branch and its memory partition all come from there,
/// so reaching a team from the root gives the same crew as opening a session inside
/// it. Whole-tree validation runs first, so a chart with a bad reference anywhere
/// spawns nothing at all.
pub fn spawn_tree(
    session: &Arc<Session>,
    tree: &TeamTree,
    home: &Path,
) -> Result<SpawnSummary, TeamError> {
    validate_tree(tree, session, home)?;
    let mut summary = SpawnSummary::default();
    // The name a member ends up running under, when the registry had to claim a
    // different one. Rooms are declared in blueprint names, so without this a
    // renamed member would come up outside every room that names it.
    let mut claimed = HashMap::new();
    for node in tree.nodes() {
        let defs = crate::agents::load_agent_defs(home, &node.dir);
        let branch = current_branch(&node.dir);
        // The note remains useful context even though D56 now gives the member that cwd directly.
        let standing = (node.depth > 0).then(|| team_root_note(&node.def.name, &node.dir));
        spawn_members(
            session,
            &node.def,
            &defs,
            home,
            &node.dir,
            &branch,
            standing,
            &mut claimed,
            &mut summary,
        );
    }
    for node in tree.nodes() {
        open_rooms(session, &node.def, &claimed, &mut summary);
    }
    Ok(summary)
}

#[allow(clippy::too_many_arguments)]
fn spawn_members(
    session: &Arc<Session>,
    def: &TeamDef,
    defs: &[AgentDef],
    home: &Path,
    project_dir: &Path,
    branch: &str,
    standing: Option<String>,
    claimed: &mut HashMap<String, String>,
    summary: &mut SpawnSummary,
) {
    // Read once for the whole crew: the agreement is one file and every member carries the
    // same block, so re-reading it per member would only add ways for them to disagree.
    let norms = load_norms(project_dir).map(|n| norms_block(&def.name, &n));

    let by_name: HashMap<&str, &AgentDef> = defs.iter().map(|d| (d.name.as_str(), d)).collect();
    for member in &def.members {
        let Some(agent_def) = by_name.get(member.agent.as_str()) else {
            summary.failed.push((
                member.name.clone(),
                format!("references a non-existent AgentDef \"{}\"", member.agent),
            ));
            continue;
        };
        // Idempotency key = instance name. An instance that is already here is never
        // re-spawned — but it is brought up to date (D69): the definition files are what
        // the user edits between runs, and the alternative was deleting the instance,
        // which took its history with it.
        let existing = session
            .agents
            .state_in_project(&member.name, project_dir)
            .is_some();
        let name = if existing {
            member.name.clone()
        } else {
            session.agents.claim_name(&member.name)
        };
        if name != member.name {
            claimed.insert(member.name.clone(), name.clone());
        }
        let sub = match build_member(
            session,
            &def.name,
            member,
            agent_def,
            home,
            project_dir,
            branch,
            norms.clone(),
            standing.clone(),
            &name,
        ) {
            Ok(s) => s,
            Err(e) => {
                summary.failed.push((member.name.clone(), e));
                continue;
            }
        };
        let description = agent_def.description.clone();
        let profile = MemberProfile::merged(&agent_def.profile, &member.profile);
        let configuration_key = runtime_member_configuration_key(
            &member.member_id,
            &member.agent,
            &agent_def.system,
            agent_def.inherit_system,
            &sub.runtime.provider.borrow(),
            &sub.runtime.model.borrow(),
            sub.runtime.thinking.borrow().as_deref(),
            &profile,
        );
        if existing {
            match session
                .agents
                .refresh(&name, Some(member.agent.clone()), description, sub)
            {
                crate::agents::Refresh::Refreshed => {
                    session
                        .agents
                        .set_configuration_key(&name, configuration_key);
                    summary.refreshed.push(name);
                }
                crate::agents::Refresh::Unchanged => {
                    session
                        .agents
                        .set_configuration_key(&name, configuration_key);
                    summary.reused.push(name);
                }
                // A member mid-turn keeps the definition it started under, and a hire
                // that claimed the name is not rewritten as a standing crew member.
                _ => summary.reused.push(name),
            }
            continue;
        }
        session.agents.insert(
            &name,
            crate::agents::AgentKind::Crew,
            Some(member.agent.clone()),
            description,
            sub,
        );
        session
            .agents
            .set_configuration_key(&name, configuration_key);
        // Spawn ≠ wake: mark Idle after insert (zero-token standby; the turn only starts with SendMessage).
        session.agents.mark_idle(&name);
        summary.spawned.push(name);
    }
}

/// The session one member runs under, built from what is on disk right now: its
/// definition, the blueprint's per-member overrides, the crew's agreement, and the pointer
/// to its own past (memory is a pointer, not a preload — D51).
///
/// One builder for both paths on purpose. A refresh that built its instance differently
/// from a spawn would mean "start" and "start again" produce different members, and the
/// comparison that decides whether anything changed would be comparing two dialects.
#[allow(clippy::too_many_arguments)]
fn build_member(
    session: &Arc<Session>,
    team: &str,
    member: &TeamMember,
    agent_def: &AgentDef,
    home: &Path,
    project_dir: &Path,
    branch: &str,
    norms: Option<String>,
    standing: Option<String>,
    name: &str,
) -> Result<Arc<Session>, String> {
    ensure_transcript(home, project_dir, branch, team, name);
    let task_note = session.team_tasks.member_context_note(&member.name);
    let standing = [standing, task_note]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n\n");
    let context = crate::tool::agent::MemberContext {
        memory: member_memory_note(home, project_dir, branch, team, name),
        norms,
        standing: (!standing.is_empty()).then_some(standing),
        profile: Some(member.profile.clone()),
        experience: member_experience_note(home, project_dir, &member.member_id),
        cwd: Some(project_dir.to_path_buf()),
    };
    crate::tool::agent::build_sub_session(
        session,
        member.model.clone(),
        member.provider.clone(),
        member.thinking.clone(),
        Some(agent_def),
        name,
        context,
    )
    .map_err(|e| e.to_string())
}

/// Open a team's rooms, idempotently: create the ones that aren't there, and invite
/// into the ones that are. Only members that actually spawned are put in a room —
/// a roster listing someone who failed to start reads as a room that can reach them
/// — and a member the registry had to rename joins under the name it is running as,
/// not the one the blueprint used to ask for it.
fn open_rooms(
    session: &Arc<Session>,
    def: &TeamDef,
    claimed: &HashMap<String, String>,
    summary: &mut SpawnSummary,
) {
    let running = session.agents.list();
    for room in rooms(def) {
        let live: Vec<String> = room
            .members
            .iter()
            .map(|m| claimed.get(m).unwrap_or(m).clone())
            .filter(|m| running.iter().any(|a| &a.name == m))
            .collect();
        if session.channels.info(&room.name).is_none() {
            if let Err(e) = session.channels.create(&room.name, live, room.mode) {
                summary.failed.push((room.name.clone(), e));
                continue;
            }
        } else {
            // Late joiners get no backlog; they listen from the current head.
            for member in live {
                let _ = session.channels.invite(&room.name, &member);
            }
        }
        if let Some(limit) = room.message_limit {
            let _ = session.channels.set_message_limit(&room.name, limit);
        }
    }
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

fn member_experience_path(home: &Path, project_dir: &Path, member_id: &str) -> PathBuf {
    crate::storage::team_member_experience_dir(home)
        .join(project_key(project_dir))
        .join(format!("{member_id}.md"))
}

pub fn member_experience_note(home: &Path, project_dir: &Path, member_id: &str) -> Option<String> {
    let path = member_experience_path(home, project_dir, member_id);
    path.is_file().then(|| {
        format!(
            "Confirmed project experience for this fixed identity is stored at `{}`. Read it only when it is relevant to the current work.",
            path.display()
        )
    })
}

pub fn append_member_experience(
    home: &Path,
    project_dir: &Path,
    member_id: &str,
    task_id: &str,
    title: &str,
    summary: &str,
) -> Result<PathBuf, TeamError> {
    if !valid_stable_id(member_id) {
        return Err(TeamError::invalid(
            "member experience requires a stable memberId",
        ));
    }
    let path = member_experience_path(home, project_dir, member_id);
    let parent = path
        .parent()
        .ok_or_else(|| TeamError::invalid("member experience path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(parent.join(".lock"))?;
    lock_file.lock()?;
    let mut content = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| format!("# Confirmed experience for `{member_id}`\n"));
    content.push_str(&format!(
        "\n## {} · `{}`\n\n- Confirmed at: {}\n- Task: {}\n\n{}\n",
        title.trim(),
        task_id,
        crate::channels::now_unix(),
        title.trim(),
        summary.trim()
    ));
    crate::storage::write_atomic(&path, content.as_bytes())?;
    Ok(path)
}

pub fn reconcile_member_experience(
    home: &Path,
    project_dir: &Path,
    previous_member_ids: &[String],
    current_member_ids: &[String],
) -> Result<(), TeamError> {
    let previous = previous_member_ids
        .iter()
        .collect::<std::collections::HashSet<_>>();
    let current = current_member_ids
        .iter()
        .collect::<std::collections::HashSet<_>>();
    for member_id in previous.iter().chain(current.iter()) {
        if !valid_stable_id(member_id) {
            return Err(TeamError::invalid(
                "member experience reconciliation requires stable memberIds",
            ));
        }
    }
    let removed = previous.difference(&current).copied().collect::<Vec<_>>();
    let restored = current.difference(&previous).copied().collect::<Vec<_>>();
    if removed.is_empty() && restored.is_empty() {
        return Ok(());
    }
    let directory = crate::storage::team_member_experience_dir(home).join(project_key(project_dir));
    std::fs::create_dir_all(&directory)?;
    let archive = directory.join("archived");
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(directory.join(".lock"))?;
    lock_file.lock()?;

    for member_id in removed {
        let source = directory.join(format!("{member_id}.md"));
        if !source.is_file() {
            continue;
        }
        std::fs::create_dir_all(&archive)?;
        let content = std::fs::read(&source)?;
        crate::storage::write_atomic(&archive.join(format!("{member_id}.md")), &content)?;
    }
    for member_id in restored {
        let destination = directory.join(format!("{member_id}.md"));
        let archived = archive.join(format!("{member_id}.md"));
        if !destination.exists() && archived.is_file() {
            crate::storage::write_atomic(&destination, &std::fs::read(archived)?)?;
        }
    }
    Ok(())
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
    use crate::channels::ChannelLimits;

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

    /// An agent definition on disk, where the loader actually looks for one. The
    /// tree resolves definitions per team directory, so the spawn tests have to
    /// write real files rather than hand a fabricated list in.
    fn write_agent(dir: &std::path::Path, name: &str) {
        let agents = dir.join(".bingo/agents");
        std::fs::create_dir_all(&agents).unwrap_or_else(|e| panic!("{e}"));
        let file = agents.join(format!("{}.md", sanitize_name(name)));
        std::fs::write(
            &file,
            format!("---\nname: {name}\ndescription: {name} description\n---\n\nYou are {name}.\n"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    }

    fn tree_of(dir: &std::path::Path) -> TeamTree {
        match load_team_tree(dir) {
            Ok(Some(tree)) => tree,
            Ok(None) => panic!("no blueprint at {}", dir.display()),
            Err(e) => panic!("{e}"),
        }
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
    fn member_profile_merge_preserves_role_defaults_and_appends_prompt_rules() {
        let role = MemberProfile {
            identity: Some(MemberIdentity {
                title: Some("Staff engineer".into()),
                background: Some("Distributed systems".into()),
            }),
            personality: Some("Methodical".into()),
            communication: Some(MemberCommunication {
                language: Some("auto".into()),
                tone: Some("neutral".into()),
                verbosity: Some("balanced".into()),
                instructions: None,
            }),
            constraints: vec![BehaviorConstraint {
                kind: "noNetwork".into(),
                instruction: "Do not access the network.".into(),
                enforcement: "prompt".into(),
            }],
            preferences: vec!["Run focused tests first.".into()],
        };
        let member = MemberProfile {
            identity: Some(MemberIdentity {
                title: Some("Release lead".into()),
                background: None,
            }),
            personality: Some("Calm and direct".into()),
            communication: Some(MemberCommunication {
                language: Some("zh-CN".into()),
                tone: None,
                verbosity: Some("concise".into()),
                instructions: Some("Lead with the outcome.".into()),
            }),
            constraints: vec![
                role.constraints[0].clone(),
                BehaviorConstraint {
                    kind: "reviewOnly".into(),
                    instruction: "Only review the proposed changes.".into(),
                    enforcement: "prompt".into(),
                },
            ],
            preferences: vec![
                "Run focused tests first.".into(),
                "Call out unresolved risks.".into(),
            ],
        };

        let merged = MemberProfile::merged(&role, &member);
        let identity = merged
            .identity
            .as_ref()
            .unwrap_or_else(|| panic!("identity"));
        assert_eq!(identity.title.as_deref(), Some("Release lead"));
        assert_eq!(identity.background.as_deref(), Some("Distributed systems"));
        assert_eq!(merged.personality.as_deref(), Some("Calm and direct"));
        assert_eq!(merged.constraints.len(), 2);
        assert_eq!(merged.preferences.len(), 2);
        let prompt = merged
            .prompt_block("Lin")
            .unwrap_or_else(|| panic!("prompt"));
        let profile_at = prompt.find("Fixed team member profile").unwrap();
        let must_at = prompt.find("MUST behavior constraints").unwrap();
        let should_at = prompt.find("SHOULD working preferences").unwrap();
        assert!(profile_at < must_at && must_at < should_at, "{prompt}");
        assert!(prompt.contains("prompt guidance, not a security sandbox"));
        assert!(prompt.contains("stop and report the conflict"));
    }

    #[test]
    fn legacy_team_v1_gets_stable_ids_and_saves_as_v2() {
        let dir = tmp("legacy-v1");
        write_team(
            &dir,
            r#"{"name":"legacy","members":[{"name":"lead","agent":"lead"}]}"#,
        );
        let original = std::fs::read(dir.join(TEAM_FILE)).unwrap_or_else(|error| panic!("{error}"));
        let first = load_team_file(&dir).unwrap().unwrap();
        let second = load_team_file(&dir).unwrap().unwrap();
        assert_eq!(first.team_id, second.team_id);
        assert_eq!(first.members[0].member_id, second.members[0].member_id);
        assert!(first.members[0].avatar.is_none());
        assert_eq!(
            std::fs::read(dir.join(TEAM_FILE)).unwrap_or_else(|error| panic!("{error}")),
            original,
            "reading a legacy member without an avatar must not rewrite the project file"
        );
        assert!(first.team_id.starts_with("team-"));
        assert!(first.members[0].member_id.starts_with("member-"));

        write_team_file(&dir, &first).unwrap_or_else(|error| panic!("{error}"));
        let raw = std::fs::read_to_string(dir.join(TEAM_FILE)).unwrap();
        assert!(raw.contains("\"schemaVersion\": 2"), "{raw}");
        assert!(raw.contains("\"memberId\""), "{raw}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn avatar_import_normalizes_content_and_reuses_hash() {
        let dir = tmp("avatar-import");
        let mut source = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(16, 8)
            .write_to(&mut source, image::ImageFormat::Png)
            .unwrap_or_else(|error| panic!("{error}"));
        let first = import_avatar(&dir, source.get_ref()).unwrap_or_else(|error| panic!("{error}"));
        let second =
            import_avatar(&dir, source.get_ref()).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(first, second);
        assert_eq!(
            project_avatar_ids(&dir).unwrap_or_else(|error| panic!("{error}")),
            vec![first.clone()]
        );
        let image = image::open(project_avatar_path(&dir, &first).unwrap())
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!((image.width(), image.height()), (512, 512));
        assert!(import_avatar(&dir, b"not an image").is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn team_avatar_thumbnail_reads_only_registered_images_in_the_current_tree() {
        let root = tmp("avatar-tree-read");
        write_chart(&root);
        let child_dir = root.join("repos/engineering");
        let mut source = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            16,
            8,
            image::Rgba([210, 40, 80, 255]),
        ))
        .write_to(&mut source, image::ImageFormat::Png)
        .unwrap_or_else(|error| panic!("{error}"));
        let id =
            import_avatar(&child_dir, source.get_ref()).unwrap_or_else(|error| panic!("{error}"));
        let tree = tree_of(&root);
        let thumbnail =
            team_avatar_thumbnail(&tree, &id, 128).unwrap_or_else(|error| panic!("{error}"));
        let decoded = image::load_from_memory(&thumbnail).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!((decoded.width(), decoded.height()), (128, 128));
        assert!(team_avatar_thumbnail(&tree, "project:../../outside", 128).is_err());
        assert!(team_avatar_thumbnail(&tree, "sora", 128).is_err());

        let outside = tmp("avatar-tree-outside");
        let mut other = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            8,
            16,
            image::Rgba([20, 60, 220, 255]),
        ))
        .write_to(&mut other, image::ImageFormat::Png)
        .unwrap_or_else(|error| panic!("{error}"));
        let outside_id =
            import_avatar(&outside, other.get_ref()).unwrap_or_else(|error| panic!("{error}"));
        assert!(team_avatar_thumbnail(&tree, &outside_id, 128).is_err());

        std::fs::remove_dir_all(&outside).unwrap_or_else(|error| panic!("{error}"));
        std::fs::remove_dir_all(&root).unwrap_or_else(|error| panic!("{error}"));
    }

    #[test]
    fn confirmed_experience_is_scoped_to_project_and_stable_member_id() {
        let home = tmp("experience-home");
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let path = append_member_experience(
            &home,
            &project,
            "member-reviewer",
            "task-1",
            "Review release",
            "Found and confirmed the compatibility fix.",
        )
        .unwrap_or_else(|error| panic!("{error}"));
        append_member_experience(
            &home,
            &project,
            "member-reviewer",
            "task-2",
            "Verify release",
            "Confirmed the final package.",
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Review release") && content.contains("Verify release"));
        assert!(member_experience_note(&home, &project, "member-reviewer").is_some());
        reconcile_member_experience(&home, &project, &["member-reviewer".to_string()], &[])
            .unwrap_or_else(|error| panic!("{error}"));
        let archived = path
            .parent()
            .unwrap_or_else(|| panic!("experience directory"))
            .join("archived/member-reviewer.md");
        assert_eq!(
            std::fs::read(&archived).unwrap(),
            std::fs::read(&path).unwrap()
        );
        let held = path.with_extension("held");
        std::fs::rename(&path, &held).unwrap_or_else(|error| panic!("{error}"));
        reconcile_member_experience(&home, &project, &[], &["member-reviewer".to_string()])
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            std::fs::read(&archived).unwrap()
        );
        assert!(append_member_experience(&home, &project, "", "task", "title", "summary").is_err());
        std::fs::remove_dir_all(&home).unwrap();
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
            team_id: "team-dev".into(),
            name: "dev-room".into(),
            channel: Some(ChannelSpec {
                mode: Some("free".into()),
                message_limit: Some(80),
            }),
            members: vec![TeamMember {
                member_id: "member-qa".into(),
                name: "qa".into(),
                agent: "qa".into(),
                avatar: Some("sora".into()),
                model: Some("sub-model".into()),
                provider: Some("ds".into()),
                thinking: Some("xhigh".into()),
                profile: MemberProfile::default(),
            }],
            ..Default::default()
        };
        write_team_file(&dir, &def).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(load_team_file(&dir).unwrap().as_ref(), Some(&def));
        // camelCase is preserved on the way out (the file stays hand-editable).
        let raw = std::fs::read_to_string(dir.join(TEAM_FILE)).unwrap();
        assert!(raw.contains("\"messageLimit\": 80"), "{raw}");

        let bad = TeamDef {
            name: "t".into(),
            members: Vec::new(),
            ..Default::default()
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
            members: vec![TeamMember {
                name: "a".into(),
                agent: "ghost".into(),
                ..Default::default()
            }],
            ..Default::default()
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
            profile: MemberProfile::default(),
            source: AgentDefSource::Project,
        };
        let ok = TeamDef {
            name: "t".into(),
            members: vec![TeamMember {
                name: "a".into(),
                agent: "real".into(),
                ..Default::default()
            }],
            ..Default::default()
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
            profile: MemberProfile::default(),
            source: AgentDefSource::Project,
        }
    }

    fn team_def(name: &str, members: &[(&str, &str)]) -> TeamDef {
        TeamDef {
            name: name.into(),
            members: members
                .iter()
                .map(|(n, a)| TeamMember {
                    name: n.to_string(),
                    agent: a.to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
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
            cwd: Arc::new(std::sync::Mutex::new(std::env::temp_dir())),
            home: std::env::temp_dir(),
            user_config_dir: std::env::temp_dir().join(".config"),
            quiet: true,
            compact_failures: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            watch: crate::watch::WatchRegistry::new(),
            tasks: Arc::new(crate::tasks::TaskStore::new(&std::env::temp_dir(), "t")),
            expand_tasks: tokio::sync::watch::channel(false).0,
            agents: AgentRegistry::new(),
            channels: crate::channels::ChannelRegistry::new(ChannelLimits::default()),
            team_tasks: crate::team_tasks::TeamTaskRegistry::transient(),
            instance: None,
            attachments: crate::api::image::Attachments::new(),
        })
    }

    /// Spawn ≠ wake: newly spawned members are Idle (zero turns), the room is built;
    /// repeated start is an idempotent reuse.
    #[test]
    fn spawn_is_idempotent_and_members_idle() {
        let s = session();
        let home = tmp("spawn-mem");
        let project = home.join("proj");
        s.set_cwd(project.clone());
        for role in ["dev-ex", "ui/ux", "dev"] {
            write_agent(&project, role);
        }
        write_team_file(
            &project,
            &team_def(
                "dev-room",
                &[("dev-ex", "dev-ex"), ("ui", "ui/ux"), ("dev", "dev")],
            ),
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let first = spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
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
        let second = spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        assert!(second.spawned.is_empty());
        assert_eq!(second.reused.len(), 3, "{second:?}");
        assert!(second.refreshed.is_empty(), "nothing moved: {second:?}");
        assert_eq!(s.agents.list().len(), 3, "no duplicate instances");

        let _ = s.agents.stop("ui");
        let third = spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            third.spawned.is_empty(),
            "stopped members keep their instance"
        );
        assert_eq!(third.reused.len(), 3, "{third:?}");
        assert!(third.refreshed.is_empty(), "the definition did not move");
        assert_eq!(
            s.agents
                .list()
                .into_iter()
                .find(|member| member.name == "ui")
                .map(|member| member.state),
            Some(crate::agents::AgentState::Idle)
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// One member's history, as a turn would have left it.
    fn worked(s: &Arc<Session>, who: &str, said: &str) -> Vec<crate::api::types::Message> {
        use crate::api::types::{ContentBlock, Message, Role};
        let history = vec![Message {
            role: Role::User,
            content: vec![ContentBlock::Text { text: said.into() }],
        }];
        s.agents.finish(who, history.clone(), said.len());
        history
    }

    fn history_of(s: &Arc<Session>, who: &str) -> Vec<crate::api::types::Message> {
        s.agents.view_of(who).map(|v| v.0).unwrap_or_default()
    }

    fn prompt_of(s: &Arc<Session>, who: &str) -> String {
        s.agents
            .session_of(who)
            .map(|sub| {
                sub.system
                    .iter()
                    .map(|b| b.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    fn write_persona(project: &std::path::Path, name: &str, front: &str, body: &str) {
        let agents = project.join(".bingo/agents");
        std::fs::create_dir_all(&agents).unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(
            agents.join(format!("{}.md", sanitize_name(name))),
            format!("---\nname: {name}\n{front}---\n\n{body}\n"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
    }

    /// D69: a member whose definition moved is brought up to date where it stands —
    /// new prompt, new engine, same instance, same history. The old idempotency read
    /// the instance name and stopped there, so the only way to change a member was to
    /// delete it, which threw away everything it had done for the crew.
    #[test]
    fn start_re_reads_a_definition_that_moved_and_keeps_the_history() {
        let s = session();
        let home = tmp("spawn-refresh");
        let project = home.join("proj");
        s.set_cwd(project.clone());
        write_agent(&project, "qa");
        write_team_file(&project, &team_def("t", &[("qa", "qa")]))
            .unwrap_or_else(|e| panic!("{e}"));
        let first = spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(first.spawned, vec!["qa".to_string()], "{first:?}");
        let history = worked(&s, "qa", "shipped v0.3.3");

        // The user rewrites the persona and pins a different engine.
        write_persona(
            &project,
            "qa",
            "description: sharper qa\nmodel: pinned-model\n",
            "You are qa, and you read the tests before the diff.",
        );

        let second = spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(second.refreshed, vec!["qa".to_string()], "{second:?}");
        assert!(
            second.spawned.is_empty() && second.reused.is_empty(),
            "a refresh is neither a spawn nor a plain reuse: {second:?}"
        );
        assert_eq!(s.agents.list().len(), 1, "in place, not a second instance");
        assert!(
            prompt_of(&s, "qa").contains("read the tests before the diff"),
            "the new prompt is what the member now runs under: {}",
            prompt_of(&s, "qa")
        );
        let status = s.agents.list().remove(0);
        assert_eq!(status.model, "pinned-model", "and the new engine");
        assert_eq!(status.description, "sharper qa");
        assert_eq!(status.state, crate::agents::AgentState::Idle);
        assert_eq!(
            history_of(&s, "qa"),
            history,
            "the point of refreshing rather than re-creating: the past stays"
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// A start that changes nothing must say nothing changed. Were the comparison
    /// unreliable, every start would report a refresh and the word would stop
    /// meaning anything — including for a member that has already worked, whose
    /// history is the input most likely to drift under the comparison.
    #[test]
    fn start_reports_no_refresh_when_the_definition_is_untouched() {
        let s = session();
        let home = tmp("spawn-refresh-noop");
        let project = home.join("proj");
        s.set_cwd(project.clone());
        write_agent(&project, "qa");
        write_team_file(&project, &team_def("t", &[("qa", "qa")]))
            .unwrap_or_else(|e| panic!("{e}"));
        spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        worked(&s, "qa", "a turn happened");

        // Rewriting the same bytes is not a change either — a mtime is not a definition.
        write_agent(&project, "qa");
        let again = spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(again.reused, vec!["qa".to_string()], "{again:?}");
        assert!(again.refreshed.is_empty(), "{again:?}");
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// A member mid-turn keeps the definition it started under: swapping a persona
    /// under a running turn is a character change mid-sentence, and the turn is
    /// already holding the old session anyway. The next start catches it.
    #[test]
    fn start_leaves_a_member_mid_turn_on_its_old_definition() {
        let s = session();
        let home = tmp("spawn-refresh-busy");
        let project = home.join("proj");
        s.set_cwd(project.clone());
        write_agent(&project, "qa");
        write_team_file(&project, &team_def("t", &[("qa", "qa")]))
            .unwrap_or_else(|e| panic!("{e}"));
        spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        s.agents
            .deliver(
                "qa",
                crate::channels::HUB_NAME,
                "look at #41",
                Vec::new(),
                None,
            )
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            s.agents.flush_pending().len(),
            1,
            "the member is now running"
        );

        write_persona(
            &project,
            "qa",
            "description: qa description\n",
            "You are qa, rewritten mid-turn.",
        );
        let mid = spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(mid.reused, vec!["qa".to_string()], "{mid:?}");
        assert!(mid.refreshed.is_empty(), "no hot swap: {mid:?}");
        assert!(
            !prompt_of(&s, "qa").contains("rewritten mid-turn"),
            "the running turn keeps its persona: {}",
            prompt_of(&s, "qa")
        );

        // Turn over → the next start applies what the user wrote.
        s.agents.finish("qa", Vec::new(), 0);
        let after = spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(after.refreshed, vec!["qa".to_string()], "{after:?}");
        assert!(prompt_of(&s, "qa").contains("rewritten mid-turn"));
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// The documented loop, end to end: stop the member, edit it, start again. Stop
    /// promises start brings it back, so start must both revive it and hand it the
    /// definition the user went to the trouble of editing.
    #[test]
    fn a_stopped_member_comes_back_on_the_edited_definition() {
        let s = session();
        let home = tmp("spawn-refresh-stopped");
        let project = home.join("proj");
        s.set_cwd(project.clone());
        write_agent(&project, "qa");
        write_team_file(&project, &team_def("t", &[("qa", "qa")]))
            .unwrap_or_else(|e| panic!("{e}"));
        spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        let history = worked(&s, "qa", "before the edit");
        s.agents.stop("qa").unwrap_or_else(|e| panic!("{e}"));

        write_persona(
            &project,
            "qa",
            "description: qa description\n",
            "You are qa, with the new brief.",
        );
        let back = spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(back.refreshed, vec!["qa".to_string()], "{back:?}");
        let status = s.agents.list().remove(0);
        assert_eq!(
            status.state,
            crate::agents::AgentState::Idle,
            "/team stop says start brings it back"
        );
        assert!(prompt_of(&s, "qa").contains("with the new brief"));
        assert_eq!(history_of(&s, "qa"), history);
        // And it can be worked again: a revived member that refuses mail is not back.
        assert!(
            s.agents
                .deliver(
                    "qa",
                    crate::channels::HUB_NAME,
                    "carry on",
                    Vec::new(),
                    None
                )
                .is_ok()
        );
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// A hire holding a member's name is left exactly as it was. A hire is not a
    /// member (D53): it was spawned for one task, it is released when that task is
    /// done, and a blueprint reaching in to rewrite its persona would change the
    /// job someone is in the middle of asking for.
    #[test]
    fn start_does_not_rewrite_a_hire_that_holds_the_name() {
        let s = session();
        let home = tmp("spawn-refresh-hire");
        let project = home.join("proj");
        s.set_cwd(project.clone());
        write_agent(&project, "qa");
        write_team_file(&project, &team_def("t", &[("qa", "qa")]))
            .unwrap_or_else(|e| panic!("{e}"));
        // The name was taken by an ad-hoc spawn in this same project before the crew came up.
        s.agents.insert(
            "qa",
            crate::agents::AgentKind::Hire,
            None,
            "hired for one task".into(),
            s.clone(),
        );

        let out = spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(out.reused, vec!["qa".to_string()], "{out:?}");
        assert!(out.refreshed.is_empty(), "{out:?}");
        let status = s.agents.list().remove(0);
        assert_eq!(status.kind, crate::agents::AgentKind::Hire);
        assert_eq!(status.description, "hired for one task");
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// Start counts what it did in three buckets, and the event line is the wording
    /// QA greps for.
    #[test]
    fn summary_events_name_all_three_outcomes() {
        let summary = SpawnSummary {
            spawned: vec!["a".into()],
            reused: vec!["b".into(), "c".into()],
            refreshed: vec!["d".into()],
            failed: vec![("e".into(), "no def".into())],
        };
        assert_eq!(
            summary.events(),
            vec![
                "spawned ×1".to_string(),
                "refreshed ×1".to_string(),
                "reused ×2".to_string()
            ]
        );
        assert_eq!(summary.ready(), 4, "the failed member is not standing");
        assert!(SpawnSummary::default().events().is_empty());
    }

    /// The model a member pins is the model its instance ends up running, and a
    /// member pinning nothing keeps the session's. This is the wiring assertion:
    /// were the blueprint's fields dropped on the way to `build_sub_session`,
    /// every member would report the session's model and nothing else would fail.
    #[test]
    fn spawn_applies_the_member_engine() {
        let s = session();
        let home = tmp("spawn-engine");
        let project = home.join("proj");
        write_agent(&project, "qa");
        write_agent(&project, "dev");
        let team = TeamDef {
            name: "t".into(),
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
            ..Default::default()
        };
        write_team_file(&project, &team).unwrap_or_else(|e| panic!("{e}"));
        spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
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
        std::fs::remove_dir_all(&home).unwrap();
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
            members: vec![m],
            ..Default::default()
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
    fn spawn_refuses_a_blueprint_with_a_bad_engine() {
        let s = session();
        let home = tmp("spawn-bad-engine");
        let project = home.join("proj");
        write_agent(&project, "qa");
        write_agent(&project, "dev");
        let team = TeamDef {
            name: "t".into(),
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
            ..Default::default()
        };
        write_team_file(&project, &team).unwrap_or_else(|e| panic!("{e}"));
        let err = spawn_tree(&s, &tree_of(&project), &home)
            .unwrap_err()
            .to_string();
        assert!(err.contains("bogus"), "{err}");
        assert!(s.agents.list().is_empty(), "no member is left half-spawned");
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// Memory is a pointer, not a preload (D51): a member spawns with an empty
    /// context and a note saying where its past is. The old behaviour charged a
    /// growing, invisible toll on the member's first turn — unbounded, monotonic,
    /// and mostly stale — for a file the member can read when it actually needs it.
    #[test]
    fn spawn_points_at_memory_instead_of_loading_it() {
        let s = session();
        let home = tmp("spawn-restore");
        let project = home.join("proj");
        write_agent(&project, "qa");
        write_team_file(&project, &team_def("t", &[("qa", "qa")]))
            .unwrap_or_else(|e| panic!("{e}"));
        // A team's memory is keyed by its own directory and its own branch, which is
        // what the spawn will look under.
        let mem_home = home.clone();
        let branch = current_branch(&project);
        let branch = branch.as_str();
        let msgs = vec![crate::api::types::Message::user_text(
            "last round's conclusion",
        )];
        save_member_history(&mem_home, &project, branch, "t", "qa", &msgs);

        spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        let (history, _, _, _, state) = s
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
        let note = member_memory_note(&mem_home, &project, branch, "t", "qa")
            .unwrap_or_else(|| panic!("a member with a past gets a note"));
        let path = member_transcript_path(&team_memory_dir(&mem_home, &project, branch, "t"), "qa");
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
            member_memory_note(&mem_home, &project, branch, "t", "ghost").is_none(),
            "no past, no note"
        );

        // A history written before D51 is JSON only; the note would otherwise name
        // a file that does not exist, so the transcript materializes on spawn.
        std::fs::remove_file(&path).unwrap_or_else(|e| panic!("{e}"));
        assert!(member_memory_note(&mem_home, &project, branch, "t", "qa").is_none());
        ensure_transcript(&mem_home, &project, branch, "t", "qa");
        assert!(path.exists(), "an older record renders on first sight");
        assert!(member_memory_note(&mem_home, &project, branch, "t", "qa").is_some());
        std::fs::remove_dir_all(&home).unwrap();
    }

    /// The agreement reaches every member's system prompt at spawn (D53), carrying the one
    /// clause that makes it usable: a direct instruction outranks it, and everything the
    /// instruction did not speak to still holds. Were the block dropped on the way to
    /// `build_sub_session`, the crew would simply behave as it did before and no other
    /// assertion would notice.
    #[test]
    fn spawn_gives_every_member_the_working_agreement() {
        let s = session();
        let home = tmp("norms-spawn");
        let project = home.join("proj");
        write_agent(&project, "qa");
        write_agent(&project, "dev");
        std::fs::write(
            project.join(NORMS_FILE),
            "# Team norms\n\n- Report outcomes as they are.\n",
        )
        .unwrap_or_else(|e| panic!("{e}"));

        write_team_file(&project, &team_def("crew", &[("qa", "qa"), ("dev", "dev")]))
            .unwrap_or_else(|e| panic!("{e}"));
        spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));

        for who in ["qa", "dev"] {
            let sub = s
                .agents
                .session_of(who)
                .unwrap_or_else(|| panic!("{who} should exist"));
            let block = sub
                .system
                .iter()
                .find(|b| b.text.starts_with("# Team norms"))
                .unwrap_or_else(|| panic!("{who} carries no agreement: {:?}", sub.system));
            assert!(
                block.text.contains("Report outcomes as they are."),
                "the file's own text is what is carried: {}",
                block.text
            );
            assert!(
                block.text.contains("A direct instruction outranks it"),
                "the precedence rule travels with it: {}",
                block.text
            );
            assert!(
                block.text.contains(NORMS_FILE),
                "and it says where it came from, so it can be changed: {}",
                block.text
            );
        }
        std::fs::remove_dir_all(&home).unwrap_or_else(|e| panic!("{e}"));
    }

    /// No file, or a file of blank lines, is not an agreement: a member gets nothing rather
    /// than a block that spends context saying nothing.
    #[test]
    fn an_empty_agreement_is_no_agreement() {
        let project = tmp("norms-empty");
        std::fs::create_dir_all(project.join(".bingo")).unwrap_or_else(|e| panic!("{e}"));
        assert!(load_norms(&project).is_none(), "no file, no agreement");
        std::fs::write(project.join(NORMS_FILE), "  \n\n\t\n").unwrap_or_else(|e| panic!("{e}"));
        assert!(load_norms(&project).is_none(), "blank lines are not rules");

        // And the scaffold never overwrites one that is already there.
        std::fs::write(project.join(NORMS_FILE), "mine\n").unwrap_or_else(|e| panic!("{e}"));
        assert!(
            !write_norms_template(&project).unwrap_or_else(|e| panic!("{e}")),
            "an existing agreement is left alone"
        );
        assert_eq!(load_norms(&project).as_deref(), Some("mine"));
        std::fs::remove_dir_all(&project).unwrap_or_else(|e| panic!("{e}"));
    }

    /// What the hub is told: who is on the crew, which rooms reach whom, and the rule
    /// that sends work to a member before it spawns a stand-in for one.
    #[test]
    fn the_crew_note_names_the_roster_and_the_routing_rule() {
        let home = tmp("crew-note");
        let project = home.join("proj");
        write_agent(&project, "qa");
        write_agent(&project, "dev");
        write_team_file(
            &project,
            &team_def("dev-room", &[("Mira", "qa"), ("Linh", "dev")]),
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let note = crew_note(&tree_of(&project), &home);
        assert!(note.contains("dev-room"), "{note}");
        assert!(
            note.contains("Mira — qa description") && note.contains("Linh — dev description"),
            "each member is named with what it is for: {note}"
        );
        assert!(
            note.contains("Rooms: #dev-room (Mira, Linh)"),
            "and which room reaches whom: {note}"
        );
        assert!(
            note.contains("SendMessage"),
            "how to hand work over: {note}"
        );
        assert!(
            note.contains("Hire from outside only for what no member covers"),
            "{note}"
        );
        assert!(
            note.contains(TEAM_FILE) && note.contains("never enters"),
            "a hire does not become a member: {note}"
        );
        assert!(
            !note.contains(NORMS_FILE),
            "a crew with no written agreement is not pointed at one: {note}"
        );
        std::fs::write(project.join(NORMS_FILE), "- Say it once.\n")
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(crew_note(&tree_of(&project), &home).contains(NORMS_FILE));
        std::fs::remove_dir_all(&home).unwrap_or_else(|e| panic!("{e}"));
    }

    // ---- the team tree (D54) ----

    fn member(name: &str, agent: &str) -> TeamMember {
        TeamMember {
            name: name.into(),
            agent: agent.into(),
            ..Default::default()
        }
    }

    fn child(path: &str) -> TeamRef {
        TeamRef {
            name: None,
            path: path.into(),
        }
    }

    /// A four-team org on disk: the root, a department reached by naming its
    /// directory, a grandchild under that one (so "recursive" is asserted rather
    /// than assumed) and a second department reached by naming its blueprint file.
    fn write_chart(root: &Path) {
        write_agent(root, "lead");
        write_team_file(
            root,
            &TeamDef {
                name: "hq".into(),
                members: vec![member("Wen", "lead")],
                teams: vec![
                    TeamRef {
                        name: Some("engineering".into()),
                        path: "repos/engineering".into(),
                    },
                    child("strategy/.bingo/team.json"),
                ],
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let engineering = root.join("repos/engineering");
        write_agent(&engineering, "dev");
        write_team_file(
            &engineering,
            &TeamDef {
                name: "engineering".into(),
                members: vec![member("Linh", "dev"), member("Mira", "dev")],
                teams: vec![child("platform")],
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let platform = engineering.join("platform");
        write_agent(&platform, "sre");
        write_team_file(
            &platform,
            &TeamDef {
                name: "platform".into(),
                members: vec![member("Kai", "sre")],
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let strategy = root.join("strategy");
        write_agent(&strategy, "analyst");
        write_team_file(
            &strategy,
            &TeamDef {
                name: "strategy".into(),
                members: vec![member("Sora", "analyst")],
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
    }

    /// The whole chart loads from the root, in pre-order, whichever way a child is
    /// named — and a member is found by its bare name from anywhere in it, which is
    /// what makes `SendMessage` work without a team prefix.
    #[test]
    fn a_chart_loads_every_team_and_finds_members_by_bare_name() {
        let root = tmp("chart");
        write_chart(&root);
        let tree = tree_of(&root);

        let names: Vec<&str> = tree.nodes().iter().map(|n| n.def.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["hq", "engineering", "platform", "strategy"],
            "depth-first pre-order, root first"
        );
        assert_eq!(
            tree.nodes().iter().map(|n| n.depth).collect::<Vec<_>>(),
            vec![0, 1, 2, 1],
            "a grandchild is two deep: the chart recurses"
        );
        // Each node is rooted at its own directory, whichever spelling reached it.
        assert_eq!(tree.nodes()[1].dir, root.join("repos/engineering"));
        assert_eq!(
            tree.nodes()[3].dir,
            root.join("strategy"),
            "naming the blueprint file resolves to the directory that holds it"
        );

        let (node, _) = tree
            .find_member("Kai")
            .unwrap_or_else(|| panic!("a grandchild's member is addressable from the root"));
        assert_eq!(node.def.name, "platform");
        assert_eq!(tree.members().count(), 5);

        // A subtree is the contiguous run after its node — engineering plus platform,
        // not strategy.
        let under: Vec<&str> = tree
            .subtree(1)
            .into_iter()
            .map(|n| n.def.name.as_str())
            .collect();
        assert_eq!(under, vec!["engineering", "platform"]);
        assert_eq!(tree.subtree(0).len(), 4);

        // Opening a session inside a department gives that department's chart, unchanged.
        let inner = tree_of(&root.join("repos/engineering"));
        assert_eq!(inner.root().def.name, "engineering");
        assert_eq!(inner.nodes().len(), 2, "a subtree stands on its own");
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    /// Every way a reference can be wrong is caught at load, naming the file that
    /// holds the mistake — in a chart that is rarely the one the session opened.
    #[test]
    fn a_broken_reference_is_refused_and_named() {
        let root = tmp("chart-bad");
        write_chart(&root);
        let hq = |teams: Vec<TeamRef>| TeamDef {
            name: "hq".into(),
            members: vec![member("Wen", "lead")],
            teams,
            ..Default::default()
        };
        let err = |teams: Vec<TeamRef>| {
            write_team_file(&root, &hq(teams)).unwrap_or_else(|e| panic!("{e}"));
            match load_team_tree(&root) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("should have been refused"),
            }
        };

        let missing = err(vec![child("repos/ghost")]);
        assert!(
            missing.contains("no blueprint at") && missing.contains("ghost"),
            "{missing}"
        );

        // A team cannot contain itself, directly or through a cycle.
        let itself = err(vec![child(".")]);
        assert!(itself.contains("already in this tree"), "{itself}");

        // A label that disagrees with the blueprint it points at is a lie in the chart.
        let mislabelled = err(vec![TeamRef {
            name: Some("marketing".into()),
            path: "strategy".into(),
        }]);
        assert!(
            mislabelled.contains("marketing") && mislabelled.contains("strategy"),
            "{mislabelled}"
        );

        // A path that starts at a filesystem root would not travel with the repo — and
        // it is refused a step earlier, by the structural check the writer and the
        // reader share, so it never reaches the disk at all. "/etc/team.json" is the
        // case the two Path predicates disagree on: on Windows it is rooted but not
        // absolute, and it has to be refused there too.
        let rooted = write_team_file(&root, &hq(vec![child("/etc/team.json")]))
            .unwrap_err()
            .to_string();
        assert!(rooted.contains("starts at a filesystem root"), "{rooted}");
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    /// A cycle between two blueprints ends the walk instead of running it forever.
    #[test]
    fn a_cycle_is_caught_rather_than_followed() {
        let root = tmp("chart-cycle");
        write_agent(&root, "lead");
        write_agent(&root.join("dept"), "dev");
        write_team_file(
            &root,
            &TeamDef {
                name: "hq".into(),
                members: vec![member("Wen", "lead")],
                teams: vec![child("dept")],
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        write_team_file(
            &root.join("dept"),
            &TeamDef {
                name: "dept".into(),
                members: vec![member("Linh", "dev")],
                teams: vec![child("..")],
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let err = match load_team_tree(&root) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a cycle should be refused"),
        };
        assert!(err.contains("already in this tree"), "{err}");
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    /// Bare-name addressing is only true if names are unique, so the loader enforces
    /// it across files and says which two files disagree.
    #[test]
    fn names_are_unique_across_the_whole_chart() {
        let root = tmp("chart-dupes");
        write_chart(&root);
        let strategy = root.join("strategy");
        let rewrite = |def: TeamDef| {
            write_team_file(&strategy, &def).unwrap_or_else(|e| panic!("{e}"));
            match load_team_tree(&root) {
                Err(e) => e.to_string(),
                Ok(_) => panic!("should have been refused"),
            }
        };

        let root_member_id = tree_of(&root).root().def.members[0].member_id.clone();
        let mut same_identity = member("Sora", "analyst");
        same_identity.member_id = root_member_id;
        let dup_member_id = rewrite(TeamDef {
            name: "strategy".into(),
            members: vec![same_identity],
            ..Default::default()
        });
        assert!(
            dup_member_id.contains("duplicate memberId"),
            "{dup_member_id}"
        );

        let dup_member = rewrite(TeamDef {
            name: "strategy".into(),
            members: vec![member("Linh", "analyst")],
            ..Default::default()
        });
        assert!(
            dup_member.contains("duplicate member") && dup_member.contains("already declared in"),
            "{dup_member}"
        );

        let dup_team = rewrite(TeamDef {
            name: "engineering".into(),
            members: vec![member("Sora", "analyst")],
            ..Default::default()
        });
        assert!(dup_team.contains("duplicate team name"), "{dup_team}");

        // A room takes its team's name by default, so a room can collide too.
        let dup_room = rewrite(TeamDef {
            name: "strategy".into(),
            members: vec![member("Sora", "analyst")],
            channels: vec![ChannelDef {
                name: "engineering".into(),
                mode: None,
                message_limit: None,
                members: None,
            }],
            ..Default::default()
        });
        assert!(dup_room.contains("duplicate room"), "{dup_room}");
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    /// A team may hold several rooms with different rosters, and a room reaches its
    /// own team and the teams below it — never a parent, never a sibling. That is
    /// what keeps a department loadable on its own.
    #[test]
    fn rooms_are_per_team_and_reach_only_their_own_subtree() {
        let root = tmp("chart-rooms");
        write_chart(&root);
        // The root convenes a cross-department room, and keeps a private one.
        write_team_file(
            &root,
            &TeamDef {
                name: "hq".into(),
                members: vec![member("Wen", "lead")],
                channels: vec![
                    ChannelDef {
                        name: "exec".into(),
                        mode: None,
                        message_limit: Some(50),
                        members: None,
                    },
                    ChannelDef {
                        name: "release".into(),
                        mode: Some("free".into()),
                        message_limit: None,
                        members: Some(vec!["Wen".into(), "Linh".into(), "Kai".into()]),
                    },
                ],
                teams: vec![
                    child("repos/engineering"),
                    child("strategy/.bingo/team.json"),
                ],
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let tree = tree_of(&root);
        let hq_rooms = rooms(&tree.root().def);
        assert_eq!(
            hq_rooms.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["exec", "release"],
            "declaring rooms replaces the one named after the team"
        );
        assert_eq!(
            hq_rooms[0].members,
            vec!["Wen"],
            "an omitted roster is the whole team"
        );
        assert_eq!(hq_rooms[0].message_limit, Some(50));
        assert_eq!(hq_rooms[1].mode, ChannelMode::Free);
        assert_eq!(
            hq_rooms[1].members,
            vec!["Wen", "Linh", "Kai"],
            "a room may reach down into the subtree"
        );
        // A team that declares nothing still gets its one room, holding everybody.
        let engineering = rooms(&tree.nodes()[1].def);
        assert_eq!(engineering.len(), 1);
        assert_eq!(engineering[0].name, "engineering");
        assert_eq!(engineering[0].members, vec!["Linh", "Mira"]);

        // Sideways is refused, and the error says where that member actually is.
        write_team_file(
            &root.join("repos/engineering"),
            &TeamDef {
                name: "engineering".into(),
                members: vec![member("Linh", "dev"), member("Mira", "dev")],
                channels: vec![ChannelDef {
                    name: "engineering".into(),
                    mode: None,
                    message_limit: None,
                    members: Some(vec!["Linh".into(), "Sora".into()]),
                }],
                teams: vec![child("platform")],
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let err = match load_team_tree(&root) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a sibling's member is out of reach"),
        };
        assert!(
            err.contains("Sora")
                && err.contains("strategy")
                && err.contains("never a parent or a sibling"),
            "{err}"
        );
        assert!(err.contains("channels[0].members[1]"), "{err}");
        std::fs::remove_dir_all(&root).unwrap_or_else(|e| panic!("{e}"));
    }

    /// The acceptance criterion: from one session at the root, `start` brings up every
    /// member in the chart and opens every room, and running it again reuses instead of
    /// duplicating. Each member is spawned against its own team's directory, so a
    /// department in another repo carries its own past and knows where it is.
    #[test]
    fn spawn_tree_brings_up_the_whole_chart_and_is_idempotent() {
        let s = session();
        let home = tmp("chart-spawn");
        let root = home.join("proj");
        write_chart(&root);
        // Joined the way the loader joins it (one child reference at a time), so the
        // path compares equal on a platform whose separator is not the one written in
        // the reference itself.
        let platform = root.join("repos/engineering").join("platform");
        // A past filed under the grandchild's own directory and branch is the past
        // that member is pointed at — the partition follows the team, not the session.
        let branch = current_branch(&platform);
        save_member_history(
            &home,
            &platform,
            &branch,
            "platform",
            "Kai",
            &[crate::api::types::Message::user_text("last sprint's call")],
        );

        let first = spawn_tree(&s, &tree_of(&root), &home).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(first.spawned.len(), 5, "{first:?}");
        assert!(first.failed.is_empty(), "{first:?}");
        for who in ["Wen", "Linh", "Mira", "Kai", "Sora"] {
            assert!(
                s.agents.list().iter().any(|a| a.name == who),
                "{who} should be up"
            );
        }
        for room in ["hq", "engineering", "platform", "strategy"] {
            let info = s
                .channels
                .info(room)
                .unwrap_or_else(|| panic!("#{room} should exist"));
            assert!(info.members.contains(&"main".to_string()));
        }
        assert_eq!(
            s.channels
                .info("platform")
                .map(|c| c.members)
                .unwrap_or_default(),
            vec!["main", "user", "Kai"],
            "a room holds its own team"
        );

        let kai = s
            .agents
            .session_of("Kai")
            .unwrap_or_else(|| panic!("Kai should exist"));
        assert_eq!(
            kai.cwd(),
            platform,
            "a member runs from its own team's directory"
        );
        let standing = kai
            .system
            .iter()
            .find(|b| b.text.starts_with("# Your team is rooted at"))
            .unwrap_or_else(|| {
                panic!(
                    "a member of a team rooted elsewhere is told where: {:?}",
                    kai.system
                )
            });
        assert!(
            standing.text.contains(&platform.display().to_string()),
            "{}",
            standing.text
        );
        let memory_note = kai
            .system
            .iter()
            .find(|b| b.text.contains("Your earlier work with this crew"))
            .unwrap_or_else(|| panic!("Kai is pointed at its own team's memory: {:?}", kai.system));
        assert!(
            memory_note.text.contains(
                &platform
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            ),
            "under the grandchild's own partition: {}",
            memory_note.text
        );
        let wen = s
            .agents
            .session_of("Wen")
            .unwrap_or_else(|| panic!("Wen should exist"));
        assert!(
            !wen.system
                .iter()
                .any(|b| b.text.starts_with("# Your team is rooted at")),
            "the root team is already in the session's directory: no note"
        );

        let second = spawn_tree(&s, &tree_of(&root), &home).unwrap_or_else(|e| panic!("{e}"));
        assert!(second.spawned.is_empty(), "{second:?}");
        assert_eq!(second.reused.len(), 5, "{second:?}");
        assert_eq!(s.agents.list().len(), 5, "no duplicate instances");
        std::fs::remove_dir_all(&home).unwrap_or_else(|e| panic!("{e}"));
    }

    /// A room a parent convenes holds members that spawn under a different team, so
    /// rooms cannot be opened until the whole chart has spawned. Were the two phases
    /// collapsed, this room would come up missing everyone below the root.
    #[test]
    fn a_parents_room_holds_members_from_below() {
        let s = session();
        let home = tmp("chart-cross-room");
        let root = home.join("proj");
        write_chart(&root);
        write_team_file(
            &root,
            &TeamDef {
                name: "hq".into(),
                members: vec![member("Wen", "lead")],
                channels: vec![ChannelDef {
                    name: "release".into(),
                    mode: None,
                    message_limit: None,
                    members: Some(vec!["Wen".into(), "Kai".into(), "Sora".into()]),
                }],
                teams: vec![
                    child("repos/engineering"),
                    child("strategy/.bingo/team.json"),
                ],
                ..Default::default()
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));

        spawn_tree(&s, &tree_of(&root), &home).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            s.channels
                .info("release")
                .map(|c| c.members)
                .unwrap_or_default(),
            vec!["main", "user", "Wen", "Kai", "Sora"],
            "the cross-department room is complete"
        );
        assert!(
            s.channels.info("hq").is_none(),
            "declaring rooms replaces the one named after the team"
        );
        std::fs::remove_dir_all(&home).unwrap_or_else(|e| panic!("{e}"));
    }

    /// A blueprint may ask for a name the registry cannot give — `main` and `user` are
    /// reserved for the hub and the human — so the member runs under a claimed one.
    /// Rooms are declared in blueprint names and have to follow that rename: otherwise
    /// the member comes up running but outside every room that asked for it, which
    /// reads as a member that never started.
    #[test]
    fn a_renamed_member_still_joins_its_room() {
        let s = session();
        let home = tmp("chart-rename");
        let project = home.join("proj");
        write_agent(&project, "dev");
        write_team_file(
            &project,
            &team_def("crew", &[("main", "dev"), ("Linh", "dev")]),
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let summary = spawn_tree(&s, &tree_of(&project), &home).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            summary.spawned,
            vec!["main-2", "Linh"],
            "the reserved name is claimed as a free one: {summary:?}"
        );
        assert_eq!(
            s.channels
                .info("crew")
                .map(|c| c.members)
                .unwrap_or_default(),
            vec!["main", "user", "main-2", "Linh"],
            "and the room holds the name the member is actually running as"
        );
        std::fs::remove_dir_all(&home).unwrap_or_else(|e| panic!("{e}"));
    }

    /// What the hub is told about a chart: every department under its own directory,
    /// and the fact that a bare name reaches anyone in it.
    #[test]
    fn the_crew_note_names_the_whole_chart() {
        let home = tmp("chart-note");
        let root = home.join("proj");
        write_chart(&root);
        let note = crew_note(&tree_of(&root), &home);
        assert!(note.contains("`hq` is pinned"), "{note}");
        // Derived from the directories the fixture actually built, not written as a
        // literal: the note renders a real path, and its separator is the platform's.
        let under = |team: &str, dir: &Path| {
            let rel = dir.strip_prefix(&root).unwrap_or(dir);
            format!("## {team} — {}", rel.display())
        };
        let engineering = root.join("repos/engineering");
        assert!(
            note.contains(&under("engineering", &engineering))
                && note.contains(&under("platform", &engineering.join("platform")))
                && note.contains(&under("strategy", &root.join("strategy"))),
            "each department is named under its own directory: {note}"
        );
        assert!(
            note.contains("Kai — sre description"),
            "and its members: {note}"
        );
        assert!(
            note.contains("4 teams") && note.contains("no team prefix"),
            "the addressing rule is stated where the roster is: {note}"
        );
        std::fs::remove_dir_all(&home).unwrap_or_else(|e| panic!("{e}"));
    }

    /// Config validation failure (all references missing) → Err, nothing is spawned
    /// (validate and start share the same source).
    #[test]
    fn spawn_returns_err_on_invalid_config() {
        let s = session();
        let home = tmp("spawn-err");
        let project = home.join("proj");
        write_team_file(&project, &team_def("t", &[("x", "nope")]))
            .unwrap_or_else(|e| panic!("{e}"));
        let err = spawn_tree(&s, &tree_of(&project), &home)
            .unwrap_err()
            .to_string();
        assert!(err.contains("nope"), "{err}");
        assert!(
            s.agents.list().is_empty(),
            "failed validation has no side effects"
        );
        std::fs::remove_dir_all(&home).unwrap();
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

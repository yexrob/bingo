//! Portable `.bingo-team` bundles for Team v2 blueprints.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agents::{
    AgentDef, AgentDefSource, AgentDefinitionScope, definition_path, document_from_raw,
};
use crate::error::ErrorCode;

const PRESET_SCHEMA_VERSION: u8 = 1;

#[derive(Debug, Error)]
pub enum TeamPresetError {
    #[error("team preset storage failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("team preset is invalid: {0}")]
    Invalid(String),
    #[error("team preset serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("team preset has unresolved conflicts: {0}")]
    Conflict(String),
}

impl ErrorCode for TeamPresetError {
    fn error_code(&self) -> &'static str {
        match self {
            Self::Io(_) | Self::Serialize(_) => "STORAGE_ERROR",
            Self::Conflict(_) => "CONFIG_CONFLICT",
            Self::Invalid(_) => "CONFIG_INVALID",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TeamPreset {
    schema_version: u8,
    team: serde_json::Value,
    roles: Vec<PresetRole>,
    avatars: Vec<PresetAvatar>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PresetRole {
    id: String,
    scope: String,
    name: String,
    content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PresetAvatar {
    id: String,
    data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamPresetItem {
    pub key: String,
    pub kind: String,
    pub name: String,
    pub action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamPresetMember {
    pub member_id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    pub needs_mapping: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamPresetModelMapping {
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub thinking: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamPresetPreview {
    pub schema_version: u8,
    pub team_id: String,
    pub team_name: String,
    pub member_count: usize,
    pub role_count: usize,
    pub avatar_count: usize,
    pub items: Vec<TeamPresetItem>,
    pub members: Vec<TeamPresetMember>,
}

pub fn export(home: &Path, project_dir: &Path) -> Result<Vec<u8>, TeamPresetError> {
    let tree = crate::team::load_team_tree(project_dir)
        .map_err(|error| TeamPresetError::Invalid(error.to_string()))?
        .ok_or_else(|| TeamPresetError::Invalid("team.json is not configured".to_string()))?;
    let referenced = tree
        .members()
        .map(|(_, member)| member.agent.clone())
        .collect::<HashSet<_>>();
    let mut roles = Vec::new();
    let mut exported_names = HashSet::new();
    for document in crate::agents::list_agent_definition_documents(home, project_dir)
        .map_err(|error| TeamPresetError::Invalid(error.to_string()))?
    {
        if !referenced.contains(&document.name) || !exported_names.insert(document.name.clone()) {
            continue;
        }
        let content = std::fs::read_to_string(&document.path)?;
        roles.push(PresetRole {
            id: document.id,
            scope: document.source,
            name: document.name,
            content,
        });
    }
    let avatar_ids = tree
        .members()
        .filter_map(|(_, member)| member.avatar.clone())
        .filter(|avatar| avatar.starts_with("project:"))
        .collect::<HashSet<_>>();
    let mut avatars = Vec::new();
    for id in avatar_ids {
        let path = crate::team::project_avatar_path(project_dir, &id)
            .ok_or_else(|| TeamPresetError::Invalid(format!("invalid avatar id {id}")))?;
        avatars.push(PresetAvatar {
            id,
            data: BASE64.encode(std::fs::read(path)?),
        });
    }
    let mut team = serde_json::to_value(&tree.root().def)?;
    team["schemaVersion"] = serde_json::Value::from(crate::team::TEAM_SCHEMA_VERSION);
    let preset = TeamPreset {
        schema_version: PRESET_SCHEMA_VERSION,
        team,
        roles,
        avatars,
    };
    let mut encoded = serde_json::to_vec_pretty(&preset)?;
    encoded.push(b'\n');
    Ok(encoded)
}

pub fn inspect(
    home: &Path,
    project_dir: &Path,
    bytes: &[u8],
) -> Result<TeamPresetPreview, TeamPresetError> {
    let preset = parse(bytes)?;
    preview(home, project_dir, &preset)
}

pub fn import(
    session: &crate::query::Session,
    home: &Path,
    project_dir: &Path,
    bytes: &[u8],
    base_revision: &str,
    resolutions: &HashMap<String, String>,
    model_mappings: &HashMap<String, TeamPresetModelMapping>,
) -> Result<TeamPresetPreview, TeamPresetError> {
    let mut preset = parse(bytes)?;
    let initial_preview = preview(home, project_dir, &preset)?;
    let _team_lock = crate::team::lock_team_file(project_dir)
        .map_err(|error| TeamPresetError::Io(std::io::Error::other(error.to_string())))?;
    let team_path = project_dir.join(crate::team::TEAM_FILE);
    let current = std::fs::read(&team_path).unwrap_or_default();
    if crate::update::sha256_hex(&current) != base_revision {
        return Err(TeamPresetError::Conflict(
            "team definition changed on disk; inspect the preset again".to_string(),
        ));
    }
    for item in initial_preview
        .items
        .iter()
        .filter(|item| item.action == "update")
    {
        if !matches!(
            resolutions.get(&item.key).map(String::as_str),
            Some("update" | "keep")
        ) {
            return Err(TeamPresetError::Conflict(item.key.clone()));
        }
    }
    let team_key = format!(
        "team:{}",
        preset
            .team
            .get("teamId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
    );
    let write_team = should_write(&initial_preview, &team_key, resolutions);
    let mut experience_member_ids = None;
    if write_team {
        apply_model_mappings(&mut preset, model_mappings)?;
        let mapped_preview = preview(home, project_dir, &preset)?;
        let missing = mapped_preview
            .members
            .iter()
            .filter(|member| member.needs_mapping)
            .map(|member| format!("{} ({})", member.name, member.member_id))
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(TeamPresetError::Invalid(format!(
                "provider and model mapping is required for: {}",
                missing.join(", ")
            )));
        }
        validate_import(session, home, project_dir, &preset, resolutions)?;
        let previous_member_ids = crate::team::load_team_file(project_dir)
            .map_err(|error| TeamPresetError::Invalid(error.to_string()))?
            .map(|definition| {
                definition
                    .members
                    .into_iter()
                    .map(|member| member.member_id)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let current: crate::team::TeamDef = serde_json::from_value(preset.team.clone())?;
        let current_member_ids = current
            .members
            .into_iter()
            .map(|member| member.member_id)
            .collect::<Vec<_>>();
        experience_member_ids = Some((previous_member_ids, current_member_ids));
    }

    let mut role_writes = Vec::new();
    for role in &preset.roles {
        let scope = AgentDefinitionScope::parse(&role.scope)
            .map_err(|error| TeamPresetError::Invalid(error.to_string()))?;
        let path = definition_path(home, project_dir, scope, &role.id);
        let raw = role.content.as_bytes();
        document_from_raw(scope, &role.id, &path, raw, false)
            .map_err(|error| TeamPresetError::Invalid(error.to_string()))?;
        let key = format!("role:{}:{}", role.scope, role.id);
        if should_write(&initial_preview, &key, resolutions) {
            role_writes.push((path, raw.to_vec()));
        }
    }
    let mut avatar_writes = Vec::new();
    for avatar in &preset.avatars {
        let data = BASE64
            .decode(&avatar.data)
            .map_err(|error| TeamPresetError::Invalid(format!("avatar {}: {error}", avatar.id)))?;
        let (imported, encoded) = crate::team::normalize_avatar(&data)
            .map_err(|error| TeamPresetError::Invalid(error.to_string()))?;
        if imported != avatar.id {
            return Err(TeamPresetError::Invalid(format!(
                "avatar {} hash does not match its content",
                avatar.id
            )));
        }
        let key = format!("avatar:{}", avatar.id);
        if should_write(&initial_preview, &key, resolutions) {
            let path =
                crate::team::project_avatar_path(project_dir, &avatar.id).ok_or_else(|| {
                    TeamPresetError::Invalid(format!("invalid avatar id {}", avatar.id))
                })?;
            avatar_writes.push((path, encoded));
        }
    }
    for (path, content) in role_writes {
        write_atomic(&path, &content)?;
    }
    for (path, content) in avatar_writes {
        write_atomic(&path, &content)?;
    }
    if write_team {
        let mut content = serde_json::to_vec_pretty(&preset.team)?;
        content.push(b'\n');
        write_atomic(&team_path, &content)?;
    }
    if let Some((previous_member_ids, current_member_ids)) = experience_member_ids {
        crate::team::reconcile_member_experience(
            home,
            project_dir,
            &previous_member_ids,
            &current_member_ids,
        )
        .map_err(|error| TeamPresetError::Invalid(error.to_string()))?;
    }
    preview(home, project_dir, &preset)
}

fn should_write(
    preview: &TeamPresetPreview,
    key: &str,
    resolutions: &HashMap<String, String>,
) -> bool {
    preview
        .items
        .iter()
        .find(|item| item.key == key)
        .is_some_and(|item| match item.action.as_str() {
            "add" => true,
            "update" => resolutions
                .get(key)
                .is_some_and(|choice| choice == "update"),
            "keep" => false,
            _ => false,
        })
}

fn parse(bytes: &[u8]) -> Result<TeamPreset, TeamPresetError> {
    if bytes.is_empty() || bytes.len() > 32 * 1024 * 1024 {
        return Err(TeamPresetError::Invalid(
            "preset must be between 1 byte and 32 MiB".to_string(),
        ));
    }
    let preset: TeamPreset = serde_json::from_slice(bytes)?;
    if preset.schema_version != PRESET_SCHEMA_VERSION {
        return Err(TeamPresetError::Invalid(format!(
            "unsupported schemaVersion {}; expected {PRESET_SCHEMA_VERSION}",
            preset.schema_version
        )));
    }
    if contains_credential_key(&preset.team) {
        return Err(TeamPresetError::Invalid(
            "preset team data contains a credential-like field".to_string(),
        ));
    }
    let schema_version = preset
        .team
        .get("schemaVersion")
        .and_then(serde_json::Value::as_u64);
    let _definition: crate::team::TeamDef = serde_json::from_value(preset.team.clone())?;
    if schema_version != Some(u64::from(crate::team::TEAM_SCHEMA_VERSION)) {
        return Err(TeamPresetError::Invalid(
            "preset must contain a Team v2 blueprint".to_string(),
        ));
    }
    Ok(preset)
}

fn validate_import(
    session: &crate::query::Session,
    home: &Path,
    project_dir: &Path,
    preset: &TeamPreset,
    resolutions: &HashMap<String, String>,
) -> Result<(), TeamPresetError> {
    let definition: crate::team::TeamDef = serde_json::from_value(preset.team.clone())?;
    crate::team::validate_structure(&definition, &project_dir.join(crate::team::TEAM_FILE))
        .map_err(|error| TeamPresetError::Invalid(error.to_string()))?;
    let mut definitions = crate::agents::load_agent_defs(home, project_dir)
        .into_iter()
        .map(|definition| (definition.name.clone(), definition))
        .collect::<HashMap<_, _>>();
    for role in &preset.roles {
        let scope = AgentDefinitionScope::parse(&role.scope)
            .map_err(|error| TeamPresetError::Invalid(error.to_string()))?;
        let path = definition_path(home, project_dir, scope, &role.id);
        let key = format!("role:{}:{}", role.scope, role.id);
        let will_write = !path.exists()
            || resolutions
                .get(&key)
                .is_none_or(|choice| choice == "update");
        if !will_write {
            continue;
        }
        let document = document_from_raw(scope, &role.id, &path, role.content.as_bytes(), false)
            .map_err(|error| TeamPresetError::Invalid(error.to_string()))?;
        let source = if role.scope == "project" {
            AgentDefSource::Project
        } else {
            AgentDefSource::User
        };
        let incoming = AgentDef {
            name: document.name.clone(),
            description: document.description,
            model: document.model,
            provider: document.provider,
            thinking: document.thinking,
            system: document.system,
            inherit_system: document.inherit_system,
            profile: document.profile,
            source,
        };
        let project_definition_exists = definitions
            .get(&document.name)
            .is_some_and(|definition| definition.source == AgentDefSource::Project);
        if source == AgentDefSource::Project || !project_definition_exists {
            definitions.insert(document.name, incoming);
        }
    }
    crate::team::validate(
        &definition,
        &definitions.into_values().collect::<Vec<_>>(),
        session,
    )
    .map_err(|error| TeamPresetError::Invalid(error.to_string()))
}

fn preview(
    home: &Path,
    project_dir: &Path,
    preset: &TeamPreset,
) -> Result<TeamPresetPreview, TeamPresetError> {
    let definition: crate::team::TeamDef = serde_json::from_value(preset.team.clone())?;
    let role_engines = preset_role_engines(preset)?;
    let current_team = std::fs::read(project_dir.join(crate::team::TEAM_FILE)).ok();
    let preset_team = serde_json::to_vec(&preset.team)?;
    let mut items = vec![TeamPresetItem {
        key: format!("team:{}", definition.team_id),
        kind: "team".to_string(),
        name: definition.name.clone(),
        action: compare_json(current_team.as_deref(), &preset_team),
    }];
    for role in &preset.roles {
        let scope = AgentDefinitionScope::parse(&role.scope)
            .map_err(|error| TeamPresetError::Invalid(error.to_string()))?;
        let current = std::fs::read(definition_path(home, project_dir, scope, &role.id)).ok();
        items.push(TeamPresetItem {
            key: format!("role:{}:{}", role.scope, role.id),
            kind: "role".to_string(),
            name: role.name.clone(),
            action: compare(current.as_deref(), role.content.as_bytes()),
        });
    }
    for avatar in &preset.avatars {
        let current = crate::team::project_avatar_path(project_dir, &avatar.id)
            .and_then(|path| std::fs::read(path).ok());
        let incoming = BASE64.decode(&avatar.data).unwrap_or_default();
        items.push(TeamPresetItem {
            key: format!("avatar:{}", avatar.id),
            kind: "avatar".to_string(),
            name: avatar.id.clone(),
            action: compare(current.as_deref(), &incoming),
        });
    }
    Ok(TeamPresetPreview {
        schema_version: PRESET_SCHEMA_VERSION,
        team_id: definition.team_id,
        team_name: definition.name,
        member_count: definition.members.len(),
        role_count: preset.roles.len(),
        avatar_count: preset.avatars.len(),
        items,
        members: definition
            .members
            .iter()
            .map(|member| {
                let role = role_engines.get(&member.agent);
                let provider = member
                    .provider
                    .clone()
                    .or_else(|| role.and_then(|engine| engine.provider.clone()));
                let model = member
                    .model
                    .clone()
                    .or_else(|| role.and_then(|engine| engine.model.clone()));
                let thinking = member
                    .thinking
                    .clone()
                    .or_else(|| role.and_then(|engine| engine.thinking.clone()));
                TeamPresetMember {
                    member_id: member.member_id.clone(),
                    name: member.name.clone(),
                    needs_mapping: provider.as_deref().is_none_or(str::is_empty)
                        || model.as_deref().is_none_or(str::is_empty),
                    provider,
                    model,
                    thinking,
                }
            })
            .collect(),
    })
}

#[derive(Debug, Clone)]
struct PresetRoleEngine {
    provider: Option<String>,
    model: Option<String>,
    thinking: Option<String>,
}

fn preset_role_engines(
    preset: &TeamPreset,
) -> Result<HashMap<String, PresetRoleEngine>, TeamPresetError> {
    let mut roles = HashMap::new();
    for role in preset
        .roles
        .iter()
        .filter(|role| role.scope == "user")
        .chain(preset.roles.iter().filter(|role| role.scope == "project"))
    {
        let scope = AgentDefinitionScope::parse(&role.scope)
            .map_err(|error| TeamPresetError::Invalid(error.to_string()))?;
        let path = PathBuf::from(format!("{}.md", role.id));
        let document = document_from_raw(scope, &role.id, &path, role.content.as_bytes(), false)
            .map_err(|error| TeamPresetError::Invalid(error.to_string()))?;
        roles.insert(
            document.name,
            PresetRoleEngine {
                provider: document.provider,
                model: document.model,
                thinking: document.thinking,
            },
        );
    }
    Ok(roles)
}

fn apply_model_mappings(
    preset: &mut TeamPreset,
    mappings: &HashMap<String, TeamPresetModelMapping>,
) -> Result<(), TeamPresetError> {
    let members = preset
        .team
        .get_mut("members")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| TeamPresetError::Invalid("team members must be an array".to_string()))?;
    let known = members
        .iter()
        .filter_map(|member| member.get("memberId").and_then(serde_json::Value::as_str))
        .map(str::to_string)
        .collect::<HashSet<_>>();
    if let Some(unknown) = mappings
        .keys()
        .find(|member_id| !known.contains(*member_id))
    {
        return Err(TeamPresetError::Invalid(format!(
            "model mapping references unknown member {unknown}"
        )));
    }
    for member in members {
        let Some(member_id) = member
            .get("memberId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some(mapping) = mappings.get(&member_id) else {
            continue;
        };
        if mapping.provider.trim().is_empty() || mapping.model.trim().is_empty() {
            return Err(TeamPresetError::Invalid(format!(
                "provider and model mapping for {member_id} must not be empty"
            )));
        }
        let object = member.as_object_mut().ok_or_else(|| {
            TeamPresetError::Invalid(format!("team member {member_id} must be an object"))
        })?;
        object.insert(
            "provider".to_string(),
            serde_json::Value::String(mapping.provider.trim().to_string()),
        );
        object.insert(
            "model".to_string(),
            serde_json::Value::String(mapping.model.trim().to_string()),
        );
        if let Some(thinking) = mapping
            .thinking
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            object.insert(
                "thinking".to_string(),
                serde_json::Value::String(thinking.to_string()),
            );
        }
    }
    Ok(())
}

fn compare(current: Option<&[u8]>, incoming: &[u8]) -> String {
    match current {
        None => "add".to_string(),
        Some(current) if current == incoming => "keep".to_string(),
        Some(_) => "update".to_string(),
    }
}

fn compare_json(current: Option<&[u8]>, incoming: &[u8]) -> String {
    let Some(current) = current else {
        return "add".to_string();
    };
    match (
        serde_json::from_slice::<serde_json::Value>(current),
        serde_json::from_slice::<serde_json::Value>(incoming),
    ) {
        (Ok(current), Ok(incoming)) if current == incoming => "keep".to_string(),
        _ => "update".to_string(),
    }
}

fn contains_credential_key(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "apikey" | "api_key" | "token" | "secret" | "password" | "credential"
            ) || contains_credential_key(value)
        }),
        serde_json::Value::Array(values) => values.iter().any(contains_credential_key),
        _ => false,
    }
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<(), TeamPresetError> {
    crate::storage::write_atomic(path, content).map_err(TeamPresetError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("bingo-team-preset-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap_or_else(|error| panic!("{error}"));
        path
    }

    fn preset(role_provider: Option<&str>, role_model: Option<&str>) -> TeamPreset {
        let mut frontmatter = String::from("---\nname: lead\ndescription: Team lead\n");
        if let Some(provider) = role_provider {
            frontmatter.push_str(&format!("provider: {provider}\n"));
        }
        if let Some(model) = role_model {
            frontmatter.push_str(&format!("model: {model}\n"));
        }
        frontmatter.push_str("---\n\nLead the team.\n");
        TeamPreset {
            schema_version: PRESET_SCHEMA_VERSION,
            team: serde_json::json!({
                "schemaVersion": 2,
                "teamId": "team-dev",
                "name": "dev",
                "leader": "lead",
                "members": [{
                    "memberId": "member-lead",
                    "name": "lead",
                    "agent": "lead"
                }]
            }),
            roles: vec![PresetRole {
                id: "lead".to_string(),
                scope: "project".to_string(),
                name: "lead".to_string(),
                content: frontmatter,
            }],
            avatars: Vec::new(),
        }
    }

    fn encode(preset: &TeamPreset) -> Vec<u8> {
        serde_json::to_vec_pretty(preset).unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn inspect_resolves_role_engine_and_compares_team_json_semantically() {
        let root = temp("inspect");
        let home = root.join("home");
        let project = root.join("project");
        std::fs::create_dir_all(project.join(".bingo")).unwrap_or_else(|error| panic!("{error}"));
        let preset = preset(Some("deepseek"), Some("deepseek-chat"));
        std::fs::write(
            project.join(crate::team::TEAM_FILE),
            serde_json::to_vec_pretty(&preset.team).unwrap_or_else(|error| panic!("{error}")),
        )
        .unwrap_or_else(|error| panic!("{error}"));

        let preview =
            inspect(&home, &project, &encode(&preset)).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(preview.items[0].action, "keep");
        assert_eq!(preview.members[0].provider.as_deref(), Some("deepseek"));
        assert_eq!(preview.members[0].model.as_deref(), Some("deepseek-chat"));
        assert!(!preview.members[0].needs_mapping);
        std::fs::remove_dir_all(&root).unwrap_or_else(|error| panic!("{error}"));
    }

    #[test]
    fn model_mapping_is_explicit_and_rejects_unknown_members() {
        let root = temp("mapping");
        let home = root.join("home");
        let project = root.join("project");
        let mut bundle = preset(None, None);
        let before = preview(&home, &project, &bundle).unwrap_or_else(|error| panic!("{error}"));
        assert!(before.members[0].needs_mapping);

        apply_model_mappings(
            &mut bundle,
            &HashMap::from([(
                "member-lead".to_string(),
                TeamPresetModelMapping {
                    provider: "deepseek".to_string(),
                    model: "deepseek-chat".to_string(),
                    thinking: Some("high".to_string()),
                },
            )]),
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let after = preview(&home, &project, &bundle).unwrap_or_else(|error| panic!("{error}"));
        assert!(!after.members[0].needs_mapping);
        assert_eq!(after.members[0].provider.as_deref(), Some("deepseek"));
        assert_eq!(after.members[0].thinking.as_deref(), Some("high"));

        let error = apply_model_mappings(
            &mut bundle,
            &HashMap::from([(
                "unknown-member".to_string(),
                TeamPresetModelMapping {
                    provider: "default".to_string(),
                    model: "model".to_string(),
                    thinking: None,
                },
            )]),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown member"), "{error}");
        std::fs::remove_dir_all(&root).unwrap_or_else(|error| panic!("{error}"));
    }

    #[test]
    fn preset_parser_rejects_credentials_and_corrupt_input() {
        let mut with_secret = preset(Some("default"), Some("model"));
        with_secret.team["settings"] = serde_json::json!({ "apiKey": "do-not-export" });
        let error = parse(&encode(&with_secret)).unwrap_err().to_string();
        assert!(error.contains("credential-like"), "{error}");
        assert!(parse(b"{not-json").is_err());
    }

    #[test]
    fn import_plan_writes_only_additions_and_selected_updates() {
        let preview = TeamPresetPreview {
            schema_version: PRESET_SCHEMA_VERSION,
            team_id: "team-dev".to_string(),
            team_name: "dev".to_string(),
            member_count: 0,
            role_count: 0,
            avatar_count: 0,
            items: vec![
                TeamPresetItem {
                    key: "add".to_string(),
                    kind: "role".to_string(),
                    name: "add".to_string(),
                    action: "add".to_string(),
                },
                TeamPresetItem {
                    key: "keep".to_string(),
                    kind: "role".to_string(),
                    name: "keep".to_string(),
                    action: "keep".to_string(),
                },
                TeamPresetItem {
                    key: "update".to_string(),
                    kind: "avatar".to_string(),
                    name: "update".to_string(),
                    action: "update".to_string(),
                },
            ],
            members: Vec::new(),
        };

        assert!(should_write(&preview, "add", &HashMap::new()));
        assert!(!should_write(&preview, "keep", &HashMap::new()));
        assert!(!should_write(
            &preview,
            "update",
            &HashMap::from([("update".to_string(), "keep".to_string())])
        ));
        assert!(should_write(
            &preview,
            "update",
            &HashMap::from([("update".to_string(), "update".to_string())])
        ));
    }
}

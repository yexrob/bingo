use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};

use crate::api::contract::StreamEvent;
use crate::api::types::Message;
use crate::error::{ErrorCode, error_code_boxed, sanitize_msg};
use crate::query::{AskAnswer, Session, ToolCallStatus, UiHooks, run_query};
use crate::transcript::Transcript;

mod team_handlers;

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_COMMAND_LINE_BYTES: usize = 48 * 1024 * 1024;
pub const MAX_EVENT_LINE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PROMPT_CHARS: usize = 1_000_000;
pub const MAX_RESPONSE_CHARS: usize = 100_000;
pub const MAX_RENAME_CHARS: usize = 80;
pub const CAPABILITIES: &[&str] = &[
    "settings.inspect.v1",
    "session.context.v1",
    "session.workspace.v1",
    "team.workspace.v1",
    "team.tasks.v1",
    "team.blueprint.v2",
    "team.lobby.v1",
    "team.presets.v1",
    "team.member.profile.v1",
    "team.avatar.read.v1",
    "attachments.input.v1",
    "session.fork.v1",
];

#[derive(Debug, thiserror::Error)]
pub enum JsonEventsError {
    #[error("{0}")]
    BadArgument(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl ErrorCode for JsonEventsError {
    fn error_code(&self) -> &'static str {
        match self {
            Self::BadArgument(_) => "BAD_ARGUMENT",
            Self::Io(_) | Self::Json(_) => "STORAGE_ERROR",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ClientCommand {
    #[serde(rename = "attachment.add")]
    AttachmentAdd {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "attachmentId")]
        attachment_id: String,
        data: String,
    },
    #[serde(rename = "turn.start")]
    TurnStart {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "turnId")]
        turn_id: String,
        prompt: String,
    },
    #[serde(rename = "turn.cancel")]
    TurnCancel {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "turnId")]
        turn_id: String,
    },
    #[serde(rename = "prompt.respond")]
    PromptRespond {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "promptId")]
        prompt_id: String,
        response: PromptResponse,
    },
    #[serde(rename = "models.list")]
    ModelsList {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        provider: String,
    },
    #[serde(rename = "providers.list")]
    ProvidersList {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    #[serde(rename = "settings.get")]
    SettingsGet {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    #[serde(rename = "context.subscribe")]
    ContextSubscribe {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    #[serde(rename = "team.subscribe")]
    TeamSubscribe {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    #[serde(rename = "team.refresh")]
    TeamRefresh {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    #[serde(rename = "team.validate")]
    TeamValidate {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    #[serde(rename = "team.save")]
    TeamSave {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "baseRevision")]
        base_revision: String,
        definition: serde_json::Value,
    },
    #[serde(rename = "team.start")]
    TeamStart {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    #[serde(rename = "team.stop")]
    TeamStop {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    #[serde(rename = "team.lobby.get")]
    TeamLobbyGet {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(default, rename = "beforeSeq")]
        before_seq: Option<u64>,
        #[serde(default)]
        limit: Option<usize>,
    },
    #[serde(rename = "team.lobby.post")]
    TeamLobbyPost {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        text: String,
        #[serde(default)]
        targets: Vec<String>,
    },
    #[serde(rename = "team.avatar.import")]
    TeamAvatarImport {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "fileName")]
        file_name: String,
        data: String,
    },
    #[serde(rename = "team.avatar.get")]
    TeamAvatarGet {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        avatar: String,
    },
    #[serde(rename = "team.preset.inspect")]
    TeamPresetInspect {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        data: String,
    },
    #[serde(rename = "team.preset.import")]
    TeamPresetImport {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        data: String,
        #[serde(rename = "baseRevision")]
        base_revision: String,
        #[serde(default)]
        resolutions: std::collections::HashMap<String, String>,
        #[serde(default, rename = "modelMappings")]
        model_mappings:
            std::collections::HashMap<String, crate::team_presets::TeamPresetModelMapping>,
    },
    #[serde(rename = "team.preset.export")]
    TeamPresetExport {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    #[serde(rename = "team.member.restart")]
    TeamMemberRestart {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        member: String,
    },
    #[serde(rename = "team.member.useful")]
    TeamMemberUseful {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        member: String,
    },
    #[serde(rename = "team.member.promote")]
    TeamMemberPromote {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        member: String,
        #[serde(rename = "baseRevision")]
        base_revision: String,
    },
    #[serde(rename = "team.task.list")]
    TeamTaskList {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    #[serde(rename = "team.task.get")]
    TeamTaskGet {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(default, rename = "beforeSeq")]
        before_seq: Option<u64>,
        #[serde(default)]
        limit: Option<usize>,
    },
    #[serde(rename = "team.task.create")]
    TeamTaskCreate {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        title: String,
        description: String,
        #[serde(default)]
        participants: Option<Vec<String>>,
        #[serde(default)]
        leader: Option<String>,
        #[serde(
            default,
            rename = "contextMessageSeqs",
            skip_serializing_if = "Vec::is_empty"
        )]
        context_message_seqs: Vec<u64>,
        #[serde(
            default,
            rename = "additionalConstraints",
            skip_serializing_if = "Vec::is_empty"
        )]
        additional_constraints: Vec<crate::team::BehaviorConstraint>,
    },
    #[serde(rename = "team.task.post")]
    TeamTaskPost {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "taskId")]
        task_id: String,
        text: String,
    },
    #[serde(rename = "team.task.pause")]
    TeamTaskPause {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "taskId")]
        task_id: String,
    },
    #[serde(rename = "team.task.resume")]
    TeamTaskResume {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "taskId")]
        task_id: String,
        #[serde(default)]
        message: Option<String>,
    },
    #[serde(rename = "team.task.complete")]
    TeamTaskComplete {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "taskId")]
        task_id: String,
    },
    #[serde(rename = "team.task.cancel")]
    TeamTaskCancel {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "taskId")]
        task_id: String,
    },
    #[serde(rename = "agent.message")]
    AgentMessage {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        member: String,
        message: String,
    },
    #[serde(rename = "agent.stop")]
    AgentStop {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        member: String,
    },
    #[serde(rename = "agent.remove")]
    AgentRemove {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        member: String,
    },
    #[serde(rename = "agent.activity.get")]
    AgentActivityGet {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        member: String,
    },
    #[serde(rename = "agent.definition.list")]
    AgentDefinitionList {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    #[serde(rename = "agent.definition.get")]
    AgentDefinitionGet {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        scope: String,
        id: String,
    },
    #[serde(rename = "agent.definition.save")]
    AgentDefinitionSave {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        scope: String,
        id: String,
        #[serde(default, rename = "baseRevision")]
        base_revision: Option<String>,
        definition: Box<crate::agents::AgentDefinitionInput>,
    },
    #[serde(rename = "agent.definition.archive")]
    AgentDefinitionArchive {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        scope: String,
        id: String,
        #[serde(rename = "baseRevision")]
        base_revision: String,
    },
    #[serde(rename = "channel.post")]
    ChannelPost {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        channel: String,
        text: String,
    },
    #[serde(rename = "channel.history.get")]
    ChannelHistoryGet {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        channel: String,
    },
    #[serde(rename = "session.rename")]
    SessionRename {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        name: String,
    },
    #[serde(rename = "session.delete")]
    SessionDelete {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    #[serde(rename = "session.fork")]
    SessionFork {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
        reason: crate::transcript::ForkReason,
        #[serde(default, rename = "sourceTurnId")]
        source_turn_id: Option<String>,
        #[serde(default, rename = "sourceRevision")]
        source_revision: Option<String>,
    },
    #[serde(rename = "session.close")]
    SessionClose {
        #[serde(rename = "protocolVersion")]
        protocol_version: u8,
        #[serde(rename = "commandId")]
        command_id: String,
    },
}

impl ClientCommand {
    fn protocol_version(&self) -> u8 {
        match self {
            Self::AttachmentAdd {
                protocol_version, ..
            }
            | Self::TurnStart {
                protocol_version, ..
            }
            | Self::TurnCancel {
                protocol_version, ..
            }
            | Self::PromptRespond {
                protocol_version, ..
            }
            | Self::ModelsList {
                protocol_version, ..
            }
            | Self::ProvidersList {
                protocol_version, ..
            }
            | Self::SettingsGet {
                protocol_version, ..
            }
            | Self::ContextSubscribe {
                protocol_version, ..
            }
            | Self::TeamSubscribe {
                protocol_version, ..
            }
            | Self::TeamRefresh {
                protocol_version, ..
            }
            | Self::TeamValidate {
                protocol_version, ..
            }
            | Self::TeamSave {
                protocol_version, ..
            }
            | Self::TeamStart {
                protocol_version, ..
            }
            | Self::TeamStop {
                protocol_version, ..
            }
            | Self::TeamLobbyGet {
                protocol_version, ..
            }
            | Self::TeamLobbyPost {
                protocol_version, ..
            }
            | Self::TeamAvatarImport {
                protocol_version, ..
            }
            | Self::TeamAvatarGet {
                protocol_version, ..
            }
            | Self::TeamPresetInspect {
                protocol_version, ..
            }
            | Self::TeamPresetImport {
                protocol_version, ..
            }
            | Self::TeamPresetExport {
                protocol_version, ..
            }
            | Self::TeamMemberRestart {
                protocol_version, ..
            }
            | Self::TeamMemberUseful {
                protocol_version, ..
            }
            | Self::TeamMemberPromote {
                protocol_version, ..
            }
            | Self::TeamTaskList {
                protocol_version, ..
            }
            | Self::TeamTaskGet {
                protocol_version, ..
            }
            | Self::TeamTaskCreate {
                protocol_version, ..
            }
            | Self::TeamTaskPost {
                protocol_version, ..
            }
            | Self::TeamTaskPause {
                protocol_version, ..
            }
            | Self::TeamTaskResume {
                protocol_version, ..
            }
            | Self::TeamTaskComplete {
                protocol_version, ..
            }
            | Self::TeamTaskCancel {
                protocol_version, ..
            }
            | Self::AgentMessage {
                protocol_version, ..
            }
            | Self::AgentStop {
                protocol_version, ..
            }
            | Self::AgentRemove {
                protocol_version, ..
            }
            | Self::AgentActivityGet {
                protocol_version, ..
            }
            | Self::AgentDefinitionList {
                protocol_version, ..
            }
            | Self::AgentDefinitionGet {
                protocol_version, ..
            }
            | Self::AgentDefinitionSave {
                protocol_version, ..
            }
            | Self::AgentDefinitionArchive {
                protocol_version, ..
            }
            | Self::ChannelPost {
                protocol_version, ..
            }
            | Self::ChannelHistoryGet {
                protocol_version, ..
            }
            | Self::SessionRename {
                protocol_version, ..
            }
            | Self::SessionDelete {
                protocol_version, ..
            }
            | Self::SessionFork {
                protocol_version, ..
            }
            | Self::SessionClose {
                protocol_version, ..
            } => *protocol_version,
        }
    }

    fn command_id(&self) -> &str {
        match self {
            Self::AttachmentAdd { command_id, .. }
            | Self::TurnStart { command_id, .. }
            | Self::TurnCancel { command_id, .. }
            | Self::PromptRespond { command_id, .. }
            | Self::ModelsList { command_id, .. }
            | Self::ProvidersList { command_id, .. }
            | Self::SettingsGet { command_id, .. }
            | Self::ContextSubscribe { command_id, .. }
            | Self::TeamSubscribe { command_id, .. }
            | Self::TeamRefresh { command_id, .. }
            | Self::TeamValidate { command_id, .. }
            | Self::TeamSave { command_id, .. }
            | Self::TeamStart { command_id, .. }
            | Self::TeamStop { command_id, .. }
            | Self::TeamLobbyGet { command_id, .. }
            | Self::TeamLobbyPost { command_id, .. }
            | Self::TeamAvatarImport { command_id, .. }
            | Self::TeamAvatarGet { command_id, .. }
            | Self::TeamPresetInspect { command_id, .. }
            | Self::TeamPresetImport { command_id, .. }
            | Self::TeamPresetExport { command_id, .. }
            | Self::TeamMemberRestart { command_id, .. }
            | Self::TeamMemberUseful { command_id, .. }
            | Self::TeamMemberPromote { command_id, .. }
            | Self::TeamTaskList { command_id, .. }
            | Self::TeamTaskGet { command_id, .. }
            | Self::TeamTaskCreate { command_id, .. }
            | Self::TeamTaskPost { command_id, .. }
            | Self::TeamTaskPause { command_id, .. }
            | Self::TeamTaskResume { command_id, .. }
            | Self::TeamTaskComplete { command_id, .. }
            | Self::TeamTaskCancel { command_id, .. }
            | Self::AgentMessage { command_id, .. }
            | Self::AgentStop { command_id, .. }
            | Self::AgentRemove { command_id, .. }
            | Self::AgentActivityGet { command_id, .. }
            | Self::AgentDefinitionList { command_id, .. }
            | Self::AgentDefinitionGet { command_id, .. }
            | Self::AgentDefinitionSave { command_id, .. }
            | Self::AgentDefinitionArchive { command_id, .. }
            | Self::ChannelPost { command_id, .. }
            | Self::ChannelHistoryGet { command_id, .. }
            | Self::SessionRename { command_id, .. }
            | Self::SessionDelete { command_id, .. }
            | Self::SessionFork { command_id, .. }
            | Self::SessionClose { command_id, .. } => command_id,
        }
    }

    fn validate(&mut self) -> Result<(), JsonEventsError> {
        if self.protocol_version() != PROTOCOL_VERSION {
            return Err(JsonEventsError::BadArgument(format!(
                "unsupported protocolVersion {}; expected {PROTOCOL_VERSION}",
                self.protocol_version()
            )));
        }
        if self.command_id().is_empty() {
            return Err(JsonEventsError::BadArgument(
                "commandId must not be empty".to_string(),
            ));
        }
        match self {
            Self::AttachmentAdd {
                attachment_id,
                data,
                ..
            } => {
                if attachment_id.is_empty() || attachment_id.chars().count() > 128 {
                    return Err(JsonEventsError::BadArgument(
                        "attachmentId must contain 1-128 characters".to_string(),
                    ));
                }
                let max_base64_chars = crate::api::image::MAX_DECODE_BYTES.div_ceil(3) * 4;
                if data.is_empty() || data.len() > max_base64_chars {
                    return Err(JsonEventsError::BadArgument(
                        "attachment data must encode an image no larger than 32 MiB".to_string(),
                    ));
                }
            }
            Self::TurnStart {
                turn_id, prompt, ..
            } => {
                if turn_id.is_empty() {
                    return Err(JsonEventsError::BadArgument(
                        "turnId must not be empty".to_string(),
                    ));
                }
                let chars = prompt.chars().count();
                if prompt.trim().is_empty() || chars > MAX_PROMPT_CHARS {
                    return Err(JsonEventsError::BadArgument(format!(
                        "prompt must contain non-whitespace text and be at most {MAX_PROMPT_CHARS} characters"
                    )));
                }
            }
            Self::PromptRespond { response, .. } => {
                if let PromptResponse::Text { text } = response
                    && text.chars().count() > MAX_RESPONSE_CHARS
                {
                    return Err(JsonEventsError::BadArgument(format!(
                        "prompt response must be at most {MAX_RESPONSE_CHARS} characters"
                    )));
                }
            }
            Self::SessionRename { name, .. } => {
                let trimmed = name.trim();
                let chars = trimmed.chars().count();
                if chars == 0 || chars > MAX_RENAME_CHARS {
                    return Err(JsonEventsError::BadArgument(format!(
                        "session name must be 1 to {MAX_RENAME_CHARS} characters"
                    )));
                }
                *name = trimmed.to_string();
            }
            Self::SessionFork {
                reason,
                source_turn_id,
                source_revision,
                ..
            } => match reason {
                crate::transcript::ForkReason::EditLastPrompt => {
                    if source_turn_id.is_some() != source_revision.is_some() {
                        return Err(JsonEventsError::BadArgument(
                            "sourceTurnId and sourceRevision must be provided together".to_string(),
                        ));
                    }
                    if source_turn_id.as_ref().is_some_and(String::is_empty) {
                        return Err(JsonEventsError::BadArgument(
                            "sourceTurnId must not be empty".to_string(),
                        ));
                    }
                    if source_revision.as_ref().is_some_and(|revision| {
                        revision.len() != 64
                            || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
                    }) {
                        return Err(JsonEventsError::BadArgument(
                            "sourceRevision must be a 64-character SHA-256 hex string".to_string(),
                        ));
                    }
                }
                crate::transcript::ForkReason::RecoverInterrupted => {
                    if source_turn_id.is_some() || source_revision.is_some() {
                        return Err(JsonEventsError::BadArgument(
                            "recover-interrupted does not accept a source turn".to_string(),
                        ));
                    }
                }
            },
            Self::TeamSave {
                base_revision,
                definition,
                ..
            } => {
                if base_revision.len() != 64
                    || !base_revision.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(JsonEventsError::BadArgument(
                        "baseRevision must be a 64-character SHA-256 hex string".to_string(),
                    ));
                }
                let schema_version = definition
                    .get("schemaVersion")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(1);
                if schema_version != 1
                    && schema_version != u64::from(crate::team::TEAM_SCHEMA_VERSION)
                {
                    return Err(JsonEventsError::BadArgument(format!(
                        "team definition schemaVersion must be 1 or {}",
                        crate::team::TEAM_SCHEMA_VERSION
                    )));
                }
            }
            Self::TeamAvatarGet { avatar, .. } => {
                if crate::team::project_avatar_path(Path::new("."), avatar).is_none() {
                    return Err(JsonEventsError::BadArgument(
                        "avatar must be a project:<24 hex characters> id".to_string(),
                    ));
                }
            }
            Self::AgentMessage {
                member, message, ..
            } => validate_named_message("member", member, "message", message)?,
            Self::ChannelPost { channel, text, .. } => {
                validate_named_message("channel", channel, "text", text)?
            }
            Self::AgentStop { member, .. }
            | Self::AgentRemove { member, .. }
            | Self::AgentActivityGet { member, .. } => validate_name("member", member)?,
            Self::AgentDefinitionGet { scope, id, .. }
            | Self::AgentDefinitionSave { scope, id, .. }
            | Self::AgentDefinitionArchive { scope, id, .. } => {
                crate::agents::AgentDefinitionScope::parse(scope)
                    .map_err(|error| JsonEventsError::BadArgument(error.to_string()))?;
                validate_name("id", id)?;
            }
            Self::ChannelHistoryGet { channel, .. } => validate_name("channel", channel)?,
            Self::TeamTaskGet { task_id, limit, .. } => {
                validate_name("taskId", task_id)?;
                if limit.is_some_and(|limit| limit == 0 || limit > 200) {
                    return Err(JsonEventsError::BadArgument(
                        "limit must be between 1 and 200".to_string(),
                    ));
                }
            }
            Self::TeamTaskCreate {
                title,
                description,
                participants,
                leader,
                ..
            } => {
                validate_named_message("title", title, "description", description)?;
                if participants.as_ref().is_some_and(Vec::is_empty) {
                    return Err(JsonEventsError::BadArgument(
                        "participants must not be empty".to_string(),
                    ));
                }
                if let Some(leader) = leader {
                    validate_name("leader", leader)?;
                }
            }
            Self::TeamTaskPost { task_id, text, .. } => {
                validate_named_message("taskId", task_id, "text", text)?;
            }
            Self::TeamTaskResume {
                task_id, message, ..
            } => {
                validate_name("taskId", task_id)?;
                if message
                    .as_ref()
                    .is_some_and(|message| message.chars().count() > MAX_PROMPT_CHARS)
                {
                    return Err(JsonEventsError::BadArgument(format!(
                        "message must be at most {MAX_PROMPT_CHARS} characters"
                    )));
                }
            }
            Self::TeamTaskPause { task_id, .. }
            | Self::TeamTaskComplete { task_id, .. }
            | Self::TeamTaskCancel { task_id, .. } => validate_name("taskId", task_id)?,
            _ => {}
        }
        Ok(())
    }
}

fn validate_named_message(
    name_label: &str,
    name: &str,
    message_label: &str,
    message: &str,
) -> Result<(), JsonEventsError> {
    validate_name(name_label, name)?;
    let chars = message.chars().count();
    if message.trim().is_empty() || chars > MAX_PROMPT_CHARS {
        return Err(JsonEventsError::BadArgument(format!(
            "{message_label} must contain non-whitespace text and be at most {MAX_PROMPT_CHARS} characters"
        )));
    }
    Ok(())
}

fn validate_name(label: &str, value: &str) -> Result<(), JsonEventsError> {
    if value.trim().is_empty() {
        return Err(JsonEventsError::BadArgument(format!(
            "{label} must not be empty"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "kind")]
pub enum PromptResponse {
    #[serde(rename = "option")]
    Option {
        #[serde(rename = "optionId")]
        option_id: String,
    },
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "cancel")]
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliSessionMetadata {
    pub bingo_version: String,
    pub protocol_version: u8,
    pub session_id: String,
    pub transcript_path: String,
    pub resumed: bool,
    pub cwd: String,
    pub provider: String,
    pub model: String,
    pub thinking_level: String,
    pub permission_mode: String,
    pub theme: String,
    pub supports_images: bool,
    /// Resolved shell executable for the Bash tool (effective value, not the
    /// raw setting — an empty setting resolves to the platform default).
    pub shell: String,
    /// Syntax family of `shell`: `posix` / `powershell` / `cmd` / `unknown`.
    pub shell_dialect: String,
    pub capabilities: Vec<String>,
    pub transcript_revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_reason: Option<crate::transcript::ForkReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeMetadata {
    pub bingo_version: String,
    pub protocol_version: u8,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub name: String,
    pub protocol: String,
    pub api_base_url: String,
    pub supports_images: bool,
    pub credential_configured: bool,
    pub builtin: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsLayerInfo {
    pub name: String,
    pub path: String,
    pub exists: bool,
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SanitizedSettings {
    pub api_base_url: String,
    pub provider: String,
    pub model: String,
    pub thinking_level: String,
    pub permission_mode: String,
    pub theme: String,
    pub motion: String,
    pub send_images: bool,
    pub cache_control: bool,
    pub respond_to_bash_commands: bool,
    pub shell: String,
    pub credential_configured: bool,
    pub provider_count: usize,
    pub mcp_server_count: usize,
    pub disabled_mcp_servers: Vec<String>,
    pub permission_allow: Vec<String>,
    pub permission_ask: Vec<String>,
    pub permission_deny: Vec<String>,
    pub team_auto_start: bool,
    pub agent_channels: bool,
    pub channel_message_limit: u64,
    pub agent_message_limit: u64,
    pub share_base_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamMemberSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub member_id: Option<String>,
    pub name: String,
    pub agent: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_data_url: Option<String>,
    pub status: String,
    pub pending: usize,
    pub unacked: usize,
    pub model: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<crate::team::MemberProfile>,
    pub kind: String,
    pub recommended: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub restart_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamChannelSnapshot {
    pub name: String,
    pub mode: String,
    pub seq: u64,
    pub frozen: bool,
    pub members: Vec<String>,
    pub messages: Vec<crate::channels::ChannelMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinitionSnapshot {
    pub name: String,
    pub description: String,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    pub profile: crate::team::MemberProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamSnapshot {
    pub available: bool,
    pub path: String,
    pub revision: String,
    pub branch: String,
    pub validation: Option<String>,
    pub definition: Option<serde_json::Value>,
    pub agent_definitions: Vec<AgentDefinitionSnapshot>,
    pub avatars: Vec<String>,
    pub members: Vec<TeamMemberSnapshot>,
    pub channels: Vec<TeamChannelSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentActivityItem {
    pub id: String,
    pub kind: String,
    pub summary: String,
    pub status: String,
}

/// Provider inventory for `providers.result`, in the same order as the /provider
/// listing: default → built-in preset → user-defined (shared oracle for AC-F4-1).
pub fn provider_inventory(client: &crate::api::client::Client) -> Vec<ProviderInfo> {
    let mut names = vec!["default".to_string()];
    let mut user_names = Vec::new();
    for name in client.provider_names() {
        if client.is_preset(&name) {
            names.push(name);
        } else {
            user_names.push(name);
        }
    }
    names.extend(user_names);
    let image_capable = client.image_capable_providers();
    names
        .into_iter()
        .map(|name| {
            let (api_key, api_base_url) = client
                .provider_endpoint(&name)
                .unwrap_or_else(|| (None, String::new()));
            let protocol = client.provider_protocol(&name).unwrap_or_default();
            let supports_images = if name == "default" {
                client.supports_images()
            } else {
                image_capable.contains(&name)
            };
            let credential_configured = api_key.is_some_and(|key| !key.is_empty());
            let builtin = client.is_preset(&name);
            ProviderInfo {
                name,
                protocol,
                api_base_url,
                supports_images,
                credential_configured,
                builtin,
            }
        })
        .collect()
}

fn capabilities() -> Vec<String> {
    CAPABILITIES
        .iter()
        .map(|value| (*value).to_string())
        .collect()
}

fn sanitized_settings(
    settings: &crate::settings::Settings,
    client: &crate::api::client::Client,
) -> SanitizedSettings {
    let credential_configured = client
        .provider_endpoint("default")
        .and_then(|(key, _)| key)
        .is_some_and(|key| !key.is_empty());
    SanitizedSettings {
        api_base_url: settings.api_base_url.clone().unwrap_or_default(),
        provider: settings
            .provider
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        model: settings
            .model
            .clone()
            .unwrap_or_else(|| crate::api::types::DEFAULT_MODEL.to_string()),
        thinking_level: settings
            .thinking_level
            .clone()
            .unwrap_or_else(|| "off".to_string()),
        permission_mode: settings
            .permission_mode
            .clone()
            .unwrap_or_else(|| "default".to_string()),
        theme: settings.theme.clone().unwrap_or_else(|| "auto".to_string()),
        motion: settings
            .motion
            .clone()
            .unwrap_or_else(|| "auto".to_string()),
        send_images: settings.send_images.unwrap_or(true),
        cache_control: settings.cache_control.unwrap_or(false),
        respond_to_bash_commands: settings.respond_to_bash_commands.unwrap_or(true),
        shell: settings.shell.clone().unwrap_or_default(),
        credential_configured,
        provider_count: provider_inventory(client).len(),
        mcp_server_count: settings.mcp_servers.len(),
        disabled_mcp_servers: settings.disabled_mcp_servers.clone(),
        permission_allow: settings.permissions.allow.clone(),
        permission_ask: settings.permissions.ask.clone(),
        permission_deny: settings.permissions.deny.clone(),
        team_auto_start: settings.team.auto_start.unwrap_or(true),
        agent_channels: settings.experimental.agent_channels,
        channel_message_limit: settings.experimental.channel_message_limit.unwrap_or(500),
        agent_message_limit: settings.experimental.agent_message_limit.unwrap_or(50),
        share_base_url: settings.share.base_url.clone().unwrap_or_default(),
    }
}

fn settings_layers(user_dir: &Path, project_dir: &Path) -> Vec<SettingsLayerInfo> {
    let names = ["user", "project", "local"];
    crate::settings::layer_paths(user_dir, project_dir)
        .into_iter()
        .zip(names)
        .map(|(path, name)| {
            let raw = std::fs::read_to_string(&path).ok();
            let mut keys = raw
                .as_deref()
                .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
                .and_then(|value| {
                    value
                        .as_object()
                        .map(|object| object.keys().cloned().collect())
                })
                .unwrap_or_else(Vec::new);
            keys.sort();
            SettingsLayerInfo {
                name: name.to_string(),
                path: path.display().to_string(),
                exists: raw.is_some(),
                keys,
            }
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptOption {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
pub enum CliEvent {
    #[serde(rename = "attachment.ready")]
    AttachmentReady {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "attachmentId")]
        attachment_id: String,
        marker: String,
        #[serde(rename = "mediaType")]
        media_type: String,
    },
    #[serde(rename = "session.ready")]
    SessionReady {
        #[serde(flatten)]
        base: EventBase,
        metadata: CliSessionMetadata,
    },
    #[serde(rename = "context.usage")]
    ContextUsage {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId", skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
        #[serde(rename = "turnId", skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        #[serde(rename = "usedTokens")]
        used_tokens: u64,
        #[serde(rename = "contextWindow")]
        context_window: u64,
    },
    #[serde(rename = "protocol.ready")]
    ProtocolReady {
        #[serde(flatten)]
        base: EventBase,
        metadata: ProbeMetadata,
    },
    #[serde(rename = "turn.started")]
    TurnStarted {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "promptRevision")]
        prompt_revision: String,
    },
    #[serde(rename = "text.delta")]
    TextDelta {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "turnId")]
        turn_id: String,
        delta: String,
    },
    #[serde(rename = "tool.ready")]
    ToolReady {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        name: String,
        summary: String,
    },
    #[serde(rename = "tool.done")]
    ToolDone {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        name: String,
        summary: String,
        status: ToolEventStatus,
        output: String,
        #[serde(rename = "durationMs")]
        duration_ms: u64,
    },
    #[serde(rename = "prompt.request")]
    PromptRequest {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "promptId")]
        prompt_id: String,
        kind: PromptKind,
        title: String,
        question: String,
        options: Vec<PromptOption>,
        #[serde(rename = "allowFreeText")]
        allow_free_text: bool,
    },
    #[serde(rename = "prompt.resolved")]
    PromptResolved {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "promptId")]
        prompt_id: String,
        #[serde(rename = "commandId", skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
        reason: PromptResolvedReason,
    },
    #[serde(rename = "models.result")]
    ModelsResult {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        provider: String,
        models: Vec<String>,
    },
    #[serde(rename = "providers.result")]
    ProvidersResult {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        providers: Vec<ProviderInfo>,
    },
    #[serde(rename = "settings.result")]
    SettingsResult {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        settings: SanitizedSettings,
        layers: Vec<SettingsLayerInfo>,
    },
    #[serde(rename = "team.snapshot")]
    TeamSnapshot {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId", skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
        snapshot: TeamSnapshot,
    },
    #[serde(rename = "team.tasks.snapshot")]
    TeamTasksSnapshot {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId", skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
        branch: String,
        tasks: Vec<crate::team_tasks::TeamTaskSummary>,
    },
    #[serde(rename = "team.lobby.snapshot")]
    TeamLobbySnapshot {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId", skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
        lobby: crate::team_tasks::TeamLobby,
    },
    #[serde(rename = "team.lobby.message")]
    TeamLobbyMessage {
        #[serde(flatten)]
        base: EventBase,
        message: crate::team_tasks::TeamLobbyMessage,
    },
    #[serde(rename = "team.avatar.imported")]
    TeamAvatarImported {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        avatar: String,
        snapshot: TeamSnapshot,
    },
    #[serde(rename = "team.avatar.loaded")]
    TeamAvatarLoaded {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        avatar: String,
        #[serde(rename = "dataUrl")]
        data_url: String,
    },
    #[serde(rename = "team.preset.preview")]
    TeamPresetPreview {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        preview: crate::team_presets::TeamPresetPreview,
    },
    #[serde(rename = "team.preset.imported")]
    TeamPresetImported {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        preview: crate::team_presets::TeamPresetPreview,
        snapshot: TeamSnapshot,
    },
    #[serde(rename = "team.preset.exported")]
    TeamPresetExported {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "fileName")]
        file_name: String,
        data: String,
    },
    #[serde(rename = "team.member.configured")]
    TeamMemberConfigured {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        action: String,
        member: String,
        #[serde(rename = "memberId", skip_serializing_if = "Option::is_none")]
        member_id: Option<String>,
        snapshot: TeamSnapshot,
    },
    #[serde(rename = "team.task.updated")]
    TeamTaskUpdated {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId", skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
        action: String,
        task: crate::team_tasks::TeamTaskSummary,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<crate::team_tasks::TeamTask>,
    },
    #[serde(rename = "team.task.message")]
    TeamTaskMessage {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "taskId")]
        task_id: String,
        message: crate::team_tasks::TeamTaskMessage,
    },
    #[serde(rename = "team.member.updated")]
    TeamMemberUpdated {
        #[serde(flatten)]
        base: EventBase,
        member: String,
        status: String,
        #[serde(rename = "taskId", skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    #[serde(rename = "team.validation")]
    TeamValidation {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        valid: bool,
        msg: String,
    },
    #[serde(rename = "team.updated")]
    TeamUpdated {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        action: String,
        msg: String,
        snapshot: TeamSnapshot,
    },
    #[serde(rename = "agent.updated")]
    AgentUpdated {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        action: String,
        member: String,
        msg: String,
        snapshot: TeamSnapshot,
    },
    #[serde(rename = "agent.activity")]
    AgentActivity {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        member: String,
        activity: Vec<AgentActivityItem>,
    },
    #[serde(rename = "agent.definitions.snapshot")]
    AgentDefinitionsSnapshot {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        definitions: Vec<crate::agents::AgentDefinitionDocument>,
    },
    #[serde(rename = "agent.definition.updated")]
    AgentDefinitionUpdated {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        action: String,
        definition: crate::agents::AgentDefinitionDocument,
        #[serde(rename = "archivePath", skip_serializing_if = "Option::is_none")]
        archive_path: Option<String>,
    },
    #[serde(rename = "channel.updated")]
    ChannelUpdated {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        channel: String,
        msg: String,
        snapshot: TeamSnapshot,
    },
    #[serde(rename = "channel.message")]
    ChannelMessage {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId", skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
        channel: String,
        message: crate::channels::ChannelMessage,
    },
    #[serde(rename = "inspection.ready")]
    InspectionReady {
        #[serde(flatten)]
        base: EventBase,
        metadata: ProbeMetadata,
    },
    #[serde(rename = "warning")]
    Warning {
        #[serde(flatten)]
        base: EventBase,
        #[serde(skip_serializing_if = "Option::is_none", rename = "turnId")]
        turn_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        msg: String,
    },
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(skip_serializing_if = "Option::is_none", rename = "outputTokens")]
        output_tokens: Option<u64>,
    },
    #[serde(rename = "turn.cancelled")]
    TurnCancelled {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "commandId", skip_serializing_if = "Option::is_none")]
        command_id: Option<String>,
        reason: TurnCancelledReason,
    },
    #[serde(rename = "session.renamed")]
    SessionRenamed {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "previousSessionId")]
        previous_session_id: String,
        metadata: CliSessionMetadata,
    },
    #[serde(rename = "session.deleted")]
    SessionDeleted {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "deletedSessionId")]
        deleted_session_id: String,
    },
    #[serde(rename = "session.forked")]
    SessionForked {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "sourceSessionId")]
        source_session_id: String,
        reason: crate::transcript::ForkReason,
        metadata: CliSessionMetadata,
        warnings: Vec<String>,
    },
    #[serde(rename = "session.closed")]
    SessionClosed {
        #[serde(flatten)]
        base: EventBase,
        #[serde(rename = "commandId")]
        command_id: String,
    },
    #[serde(rename = "error")]
    Error {
        #[serde(flatten)]
        base: EventBase,
        scope: ErrorScope,
        #[serde(skip_serializing_if = "Option::is_none", rename = "commandId")]
        command_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none", rename = "turnId")]
        turn_id: Option<String>,
        code: String,
        msg: String,
        level: EventErrorLevel,
        recoverable: bool,
    },
}

impl CliEvent {
    fn set_base(&mut self, base: EventBase) {
        match self {
            Self::AttachmentReady { base: slot, .. }
            | Self::SessionReady { base: slot, .. }
            | Self::ContextUsage { base: slot, .. }
            | Self::ProtocolReady { base: slot, .. }
            | Self::InspectionReady { base: slot, .. }
            | Self::TurnStarted { base: slot, .. }
            | Self::TextDelta { base: slot, .. }
            | Self::ToolReady { base: slot, .. }
            | Self::ToolDone { base: slot, .. }
            | Self::PromptRequest { base: slot, .. }
            | Self::PromptResolved { base: slot, .. }
            | Self::ModelsResult { base: slot, .. }
            | Self::ProvidersResult { base: slot, .. }
            | Self::SettingsResult { base: slot, .. }
            | Self::TeamSnapshot { base: slot, .. }
            | Self::TeamTasksSnapshot { base: slot, .. }
            | Self::TeamLobbySnapshot { base: slot, .. }
            | Self::TeamLobbyMessage { base: slot, .. }
            | Self::TeamAvatarImported { base: slot, .. }
            | Self::TeamAvatarLoaded { base: slot, .. }
            | Self::TeamPresetPreview { base: slot, .. }
            | Self::TeamPresetImported { base: slot, .. }
            | Self::TeamPresetExported { base: slot, .. }
            | Self::TeamMemberConfigured { base: slot, .. }
            | Self::TeamTaskUpdated { base: slot, .. }
            | Self::TeamTaskMessage { base: slot, .. }
            | Self::TeamMemberUpdated { base: slot, .. }
            | Self::TeamValidation { base: slot, .. }
            | Self::TeamUpdated { base: slot, .. }
            | Self::AgentUpdated { base: slot, .. }
            | Self::AgentActivity { base: slot, .. }
            | Self::AgentDefinitionsSnapshot { base: slot, .. }
            | Self::AgentDefinitionUpdated { base: slot, .. }
            | Self::ChannelUpdated { base: slot, .. }
            | Self::ChannelMessage { base: slot, .. }
            | Self::Warning { base: slot, .. }
            | Self::TurnCompleted { base: slot, .. }
            | Self::TurnCancelled { base: slot, .. }
            | Self::SessionRenamed { base: slot, .. }
            | Self::SessionDeleted { base: slot, .. }
            | Self::SessionForked { base: slot, .. }
            | Self::SessionClosed { base: slot, .. }
            | Self::Error { base: slot, .. } => *slot = base,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventBase {
    protocol_version: u8,
    seq: u64,
    session_id: Option<String>,
}

impl Default for EventBase {
    fn default() -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            seq: 0,
            session_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolEventStatus {
    Done,
    Error,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptKind {
    Permission,
    Question,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorScope {
    Command,
    Turn,
    Session,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptResolvedReason {
    Responded,
    TurnCancelled,
    SessionClosing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum TurnCancelledReason {
    Requested,
    StdinEof,
    SessionClosing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EventErrorLevel {
    Field,
    Page,
    Flow,
}

pub struct EventWriter<W> {
    writer: W,
    seq: u64,
    session_id: Option<String>,
}

impl<W: Write> EventWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            seq: 0,
            session_id: None,
        }
    }

    pub fn set_session_id(&mut self, session_id: Option<String>) {
        self.session_id = session_id;
    }

    pub fn emit(&mut self, mut event: CliEvent) -> Result<(), JsonEventsError> {
        self.seq = self
            .seq
            .checked_add(1)
            .ok_or_else(|| JsonEventsError::BadArgument("event sequence exhausted".to_string()))?;
        event.set_base(EventBase {
            protocol_version: PROTOCOL_VERSION,
            seq: self.seq,
            session_id: self.session_id.clone(),
        });
        let line = serde_json::to_vec(&event)?;
        if line.len() > MAX_EVENT_LINE_BYTES {
            return Err(JsonEventsError::BadArgument(format!(
                "event exceeds the {MAX_EVENT_LINE_BYTES}-byte line limit"
            )));
        }
        self.writer.write_all(&line)?;
        self.writer.write_all(b"\n")?;
        self.writer.flush()?;
        Ok(())
    }
}

pub fn parse_command_line(line: &[u8]) -> Result<ClientCommand, JsonEventsError> {
    if line.len() > MAX_COMMAND_LINE_BYTES {
        return Err(JsonEventsError::BadArgument(format!(
            "command line exceeds the {MAX_COMMAND_LINE_BYTES}-byte limit"
        )));
    }
    let mut command: ClientCommand = serde_json::from_slice(line)
        .map_err(|e| JsonEventsError::BadArgument(format!("invalid command: {e}")))?;
    command.validate()?;
    Ok(command)
}

fn avatar_thumbnail_data_url(project_dir: &Path, id: Option<&str>) -> Option<String> {
    let path = crate::team::project_avatar_path(project_dir, id?)?;
    let bytes = std::fs::read(path).ok()?;
    let image = image::load_from_memory(&bytes).ok()?;
    let thumbnail = image.thumbnail(96, 96);
    let mut encoded = std::io::Cursor::new(Vec::new());
    thumbnail
        .write_to(&mut encoded, image::ImageFormat::Png)
        .ok()?;
    Some(format!(
        "data:image/png;base64,{}",
        BASE64.encode(encoded.into_inner())
    ))
}

pub fn resolve_session(home: &Path, stem: &str) -> Result<Transcript, JsonEventsError> {
    if stem.is_empty()
        || stem == "."
        || stem == ".."
        || stem.contains('/')
        || stem.contains('\\')
        || stem.contains("..")
    {
        return Err(JsonEventsError::BadArgument(
            "session must be an exact transcript stem, not a path".to_string(),
        ));
    }
    let mut matches = crate::transcript::list(home)?
        .into_iter()
        .filter(|transcript| transcript.name() == stem);
    let found = matches
        .next()
        .ok_or_else(|| JsonEventsError::BadArgument(format!("session {stem:?} was not found")))?;
    if matches.next().is_some() {
        return Err(JsonEventsError::BadArgument(format!(
            "session {stem:?} is ambiguous"
        )));
    }
    Ok(found)
}

impl From<crate::transcript::TranscriptError> for JsonEventsError {
    fn from(error: crate::transcript::TranscriptError) -> Self {
        match error {
            crate::transcript::TranscriptError::Io(error) => Self::Io(error),
            crate::transcript::TranscriptError::Parse(error) => Self::Json(error),
            crate::transcript::TranscriptError::ForkPointUnavailable(message)
            | crate::transcript::TranscriptError::SessionStale(message) => {
                Self::BadArgument(message)
            }
        }
    }
}

#[derive(Debug)]
enum AdapterEvent {
    Cli(Box<CliEvent>),
    ContextUsage {
        turn_id: String,
        used_tokens: u64,
        context_window: u64,
    },
    TeamTask(crate::team_tasks::TeamTaskEvent),
    Agent(crate::agents::AgentEvent),
    Prompt(PendingPrompt),
    TurnFinished {
        turn_id: String,
        result: Result<crate::query::QueryOutcome, crate::query::QueryError>,
    },
}

#[derive(Debug)]
struct PendingPrompt {
    turn_id: String,
    prompt_id: String,
    kind: PromptKind,
    title: String,
    question: String,
    options: Vec<PromptOption>,
    allow_free_text: bool,
    reply: PromptReply,
}

#[derive(Debug)]
enum PromptReply {
    Permission(oneshot::Sender<bool>),
    Question(oneshot::Sender<Option<AskAnswer>>),
}

fn prompt_response_matches(prompt: &PendingPrompt, response: &PromptResponse) -> bool {
    match (&prompt.reply, response) {
        (PromptReply::Permission(_), PromptResponse::Option { option_id }) => {
            matches!(option_id.as_str(), "allow" | "deny")
        }
        (PromptReply::Permission(_), PromptResponse::Cancel) => true,
        (PromptReply::Question(_), PromptResponse::Option { option_id }) => prompt
            .options
            .iter()
            .any(|option| option.id == option_id.as_str()),
        (PromptReply::Question(_), PromptResponse::Text { .. }) => prompt.allow_free_text,
        (PromptReply::Question(_), PromptResponse::Cancel) => true,
        (PromptReply::Permission(_), PromptResponse::Text { .. }) => false,
    }
}

struct ActiveTurn {
    id: String,
    cancel: watch::Sender<bool>,
}

pub struct JsonSession<W> {
    session: Arc<Session>,
    writer: EventWriter<W>,
    metadata: CliSessionMetadata,
    active: Option<ActiveTurn>,
    prompts: Vec<PendingPrompt>,
    seen_command_ids: std::collections::HashSet<String>,
    cancel_command_id: Option<String>,
    close_command_id: Option<String>,
    event_tx: mpsc::UnboundedSender<AdapterEvent>,
    event_rx: mpsc::UnboundedReceiver<AdapterEvent>,
    next_prompt_id: Arc<std::sync::atomic::AtomicU64>,
    context_subscribed: Arc<std::sync::atomic::AtomicBool>,
    closing: bool,
    exit_code: i32,
}

struct TeamTaskCreateRequest {
    command_id: String,
    title: String,
    description: String,
    participants: Option<Vec<String>>,
    leader: Option<String>,
    context_message_seqs: Vec<u64>,
    additional_constraints: Vec<crate::team::BehaviorConstraint>,
}

impl<W: Write> JsonSession<W> {
    pub fn new(session: Arc<Session>, metadata: CliSessionMetadata, writer: W) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let mut event_writer = EventWriter::new(writer);
        event_writer.set_session_id(Some(metadata.session_id.clone()));
        Self {
            session,
            writer: event_writer,
            metadata,
            active: None,
            prompts: Vec::new(),
            seen_command_ids: std::collections::HashSet::new(),
            cancel_command_id: None,
            close_command_id: None,
            event_tx,
            event_rx,
            next_prompt_id: Arc::new(std::sync::atomic::AtomicU64::new(1)),
            context_subscribed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            closing: false,
            exit_code: 0,
        }
    }

    pub async fn run<R: BufRead + Send + 'static>(
        mut self,
        reader: R,
    ) -> Result<i32, JsonEventsError>
    where
        W: Send + 'static,
    {
        self.emit(CliEvent::SessionReady {
            base: EventBase::default(),
            metadata: self.metadata.clone(),
        })?;
        let sender = self.event_tx.clone();
        let mut task_events = self.session.team_tasks.subscribe();
        tokio::spawn(async move {
            loop {
                match task_events.recv().await {
                    Ok(event) => {
                        if sender.send(AdapterEvent::TeamTask(event)).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let sender = self.event_tx.clone();
        let mut agent_events = self.session.agents.subscribe();
        tokio::spawn(async move {
            loop {
                match agent_events.recv().await {
                    Ok(event) => {
                        if sender.send(AdapterEvent::Agent(event)).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        tokio::task::spawn_blocking(move || read_commands(reader, command_tx));

        loop {
            tokio::select! {
                command = command_rx.recv() => {
                    match command {
                        Some(Ok(command)) => self.handle_command(command).await?,
                        Some(Err(error)) => {
                            self.emit_error(ErrorScope::Session, None, None, "BAD_ARGUMENT", &error.to_string(), EventErrorLevel::Flow, false)?;
                            return Ok(2);
                        }
                        None => {
                            self.handle_eof()?;
                            if self.active.is_none() {
                                return Ok(self.exit_code);
                            }
                        }
                    }
                }
                event = self.event_rx.recv() => {
                    if let Some(event) = event {
                        self.handle_adapter_event(event)?;
                    }
                }
            }
            if self.closing && self.active.is_none() {
                return Ok(self.exit_code);
            }
        }
    }

    fn emit(&mut self, event: CliEvent) -> Result<(), JsonEventsError> {
        self.writer.emit(event)
    }

    async fn handle_command(&mut self, command: ClientCommand) -> Result<(), JsonEventsError> {
        let command_id = command.command_id().to_string();
        if !self.seen_command_ids.insert(command_id.clone()) {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "BAD_ARGUMENT",
                "commandId must be unique",
                EventErrorLevel::Field,
                true,
            );
        }
        match command {
            ClientCommand::AttachmentAdd {
                command_id,
                attachment_id,
                data,
                ..
            } => self.add_attachment(command_id, attachment_id, data),
            ClientCommand::TurnStart {
                command_id,
                turn_id,
                prompt,
                ..
            } => self.start_turn(command_id, turn_id, prompt),
            ClientCommand::TurnCancel {
                command_id,
                turn_id,
                ..
            } => {
                self.request_cancel(&command_id, &turn_id)?;
                self.cancel_command_id = Some(command_id);
                Ok(())
            }
            ClientCommand::PromptRespond {
                command_id,
                prompt_id,
                response,
                ..
            } => self.resolve_prompt(command_id, &prompt_id, response),
            ClientCommand::ModelsList {
                command_id,
                provider,
                ..
            } => self.list_models(command_id, provider).await,
            ClientCommand::ProvidersList { command_id, .. } => self.list_providers(command_id),
            ClientCommand::SettingsGet { command_id, .. } => self.get_settings(command_id),
            ClientCommand::ContextSubscribe { command_id, .. } => {
                self.subscribe_context(command_id)
            }
            ClientCommand::TeamSubscribe { command_id, .. }
            | ClientCommand::TeamRefresh { command_id, .. } => self.refresh_team(command_id),
            ClientCommand::TeamValidate { command_id, .. } => self.validate_team(command_id),
            ClientCommand::TeamSave {
                command_id,
                base_revision,
                definition,
                ..
            } => self.save_team(command_id, &base_revision, definition),
            ClientCommand::TeamStart { command_id, .. } => self.start_team(command_id),
            ClientCommand::TeamStop { command_id, .. } => self.stop_team(command_id),
            ClientCommand::TeamLobbyGet {
                command_id,
                before_seq,
                limit,
                ..
            } => self.get_team_lobby(command_id, before_seq, limit),
            ClientCommand::TeamLobbyPost {
                command_id,
                text,
                targets,
                ..
            } => self.post_team_lobby(command_id, &text, &targets),
            ClientCommand::TeamAvatarImport {
                command_id,
                file_name,
                data,
                ..
            } => self.import_team_avatar(command_id, &file_name, &data),
            ClientCommand::TeamAvatarGet {
                command_id, avatar, ..
            } => self.get_team_avatar(command_id, &avatar),
            ClientCommand::TeamPresetInspect {
                command_id, data, ..
            } => self.inspect_team_preset(command_id, &data),
            ClientCommand::TeamPresetImport {
                command_id,
                data,
                base_revision,
                resolutions,
                model_mappings,
                ..
            } => self.import_team_preset(
                command_id,
                &data,
                &base_revision,
                &resolutions,
                &model_mappings,
            ),
            ClientCommand::TeamPresetExport { command_id, .. } => {
                self.export_team_preset(command_id)
            }
            ClientCommand::TeamMemberRestart {
                command_id, member, ..
            } => self.restart_team_member(command_id, &member),
            ClientCommand::TeamMemberUseful {
                command_id, member, ..
            } => self.mark_team_member_useful(command_id, &member),
            ClientCommand::TeamMemberPromote {
                command_id,
                member,
                base_revision,
                ..
            } => self.promote_team_member(command_id, &member, &base_revision),
            ClientCommand::TeamTaskList { command_id, .. } => self.list_team_tasks(command_id),
            ClientCommand::TeamTaskGet {
                command_id,
                task_id,
                before_seq,
                limit,
                ..
            } => self.get_team_task(command_id, &task_id, before_seq, limit),
            ClientCommand::TeamTaskCreate {
                command_id,
                title,
                description,
                participants,
                leader,
                context_message_seqs,
                additional_constraints,
                ..
            } => self.create_team_task(TeamTaskCreateRequest {
                command_id,
                title,
                description,
                participants,
                leader,
                context_message_seqs,
                additional_constraints,
            }),
            ClientCommand::TeamTaskPost {
                command_id,
                task_id,
                text,
                ..
            } => self.post_team_task(command_id, &task_id, &text),
            ClientCommand::TeamTaskPause {
                command_id,
                task_id,
                ..
            } => self.pause_team_task(command_id, &task_id),
            ClientCommand::TeamTaskResume {
                command_id,
                task_id,
                message,
                ..
            } => self.resume_team_task(command_id, &task_id, message.as_deref()),
            ClientCommand::TeamTaskComplete {
                command_id,
                task_id,
                ..
            } => self.complete_team_task(command_id, &task_id),
            ClientCommand::TeamTaskCancel {
                command_id,
                task_id,
                ..
            } => self.cancel_team_task(command_id, &task_id),
            ClientCommand::AgentMessage {
                command_id,
                member,
                message,
                ..
            } => self.message_agent(command_id, &member, &message),
            ClientCommand::AgentStop {
                command_id, member, ..
            } => self.stop_agent(command_id, &member),
            ClientCommand::AgentRemove {
                command_id, member, ..
            } => self.remove_agent(command_id, &member),
            ClientCommand::AgentActivityGet {
                command_id, member, ..
            } => self.agent_activity(command_id, &member),
            ClientCommand::AgentDefinitionList { command_id, .. } => {
                self.list_agent_definitions(command_id)
            }
            ClientCommand::AgentDefinitionGet {
                command_id,
                scope,
                id,
                ..
            } => self.get_agent_definition(command_id, &scope, &id),
            ClientCommand::AgentDefinitionSave {
                command_id,
                scope,
                id,
                base_revision,
                definition,
                ..
            } => self.save_agent_definition(
                command_id,
                &scope,
                &id,
                base_revision.as_deref(),
                *definition,
            ),
            ClientCommand::AgentDefinitionArchive {
                command_id,
                scope,
                id,
                base_revision,
                ..
            } => self.archive_agent_definition(command_id, &scope, &id, &base_revision),
            ClientCommand::ChannelPost {
                command_id,
                channel,
                text,
                ..
            } => self.post_channel(command_id, &channel, &text),
            ClientCommand::ChannelHistoryGet {
                command_id,
                channel,
                ..
            } => self.channel_history(command_id, &channel),
            ClientCommand::SessionRename {
                command_id, name, ..
            } => self.rename_session(command_id, &name),
            ClientCommand::SessionDelete { command_id, .. } => {
                self.delete_session(command_id)?;
                self.closing = true;
                Ok(())
            }
            ClientCommand::SessionFork {
                command_id,
                reason,
                source_turn_id,
                source_revision,
                ..
            } => {
                self.fork_session(
                    command_id,
                    reason,
                    source_turn_id.as_deref(),
                    source_revision.as_deref(),
                )
                .await
            }
            ClientCommand::SessionClose { command_id, .. } => {
                if let Some(active_turn_id) = self.active.as_ref().map(|active| active.id.clone()) {
                    self.request_cancel(&command_id, &active_turn_id)?;
                    self.close_command_id = Some(command_id);
                    self.closing = true;
                    Ok(())
                } else {
                    self.emit(CliEvent::SessionClosed {
                        base: EventBase::default(),
                        command_id,
                    })?;
                    self.closing = true;
                    Ok(())
                }
            }
        }
    }

    fn add_attachment(
        &mut self,
        command_id: String,
        attachment_id: String,
        data: String,
    ) -> Result<(), JsonEventsError> {
        if self.active.is_some() {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                self.active.as_ref().map(|active| active.id.clone()),
                "BAD_ARGUMENT",
                "attachments can only be added while the session is idle",
                EventErrorLevel::Page,
                true,
            );
        }
        let bytes = match BASE64.decode(data.as_bytes()) {
            Ok(bytes) if bytes.len() <= crate::api::image::MAX_DECODE_BYTES => bytes,
            _ => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "BAD_ARGUMENT",
                    "attachment data is invalid or exceeds the 32 MiB limit",
                    EventErrorLevel::Field,
                    true,
                );
            }
        };
        let Some(prepared) = crate::api::image::prepare_image(&bytes) else {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "BAD_ARGUMENT",
                "attachment is not a supported PNG, JPEG, or GIF image",
                EventErrorLevel::Field,
                true,
            );
        };
        let media_type = prepared.media_type.clone();
        let id = self.session.attachments.register_prepared(prepared);
        self.emit(CliEvent::AttachmentReady {
            base: EventBase::default(),
            command_id,
            attachment_id,
            marker: crate::api::image::marker(id),
            media_type,
        })
    }

    fn start_turn(
        &mut self,
        command_id: String,
        turn_id: String,
        prompt: String,
    ) -> Result<(), JsonEventsError> {
        if self.active.is_some() {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                Some(turn_id),
                "BAD_ARGUMENT",
                "a turn is already active",
                EventErrorLevel::Page,
                true,
            );
        }
        let Some(transcript) = self.session.runtime.transcript.borrow().clone() else {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                Some(turn_id),
                "STORAGE_ERROR",
                "this session has no transcript",
                EventErrorLevel::Page,
                true,
            );
        };
        let prompt_revision = match transcript.begin_turn(&turn_id, &prompt) {
            Ok(revision) => revision,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    Some(turn_id),
                    error.error_code(),
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        self.emit(CliEvent::TurnStarted {
            base: EventBase::default(),
            command_id,
            turn_id: turn_id.clone(),
            prompt_revision,
        })?;
        self.cancel_command_id = None;
        self.close_command_id = None;

        let (cancel, cancel_rx) = watch::channel(false);
        self.active = Some(ActiveTurn {
            id: turn_id.clone(),
            cancel,
        });
        let session = self.session.clone();
        let sender = self.event_tx.clone();
        let hooks = json_hooks(
            sender.clone(),
            turn_id.clone(),
            self.next_prompt_id.clone(),
            self.context_subscribed.clone(),
        );
        tokio::spawn(async move {
            let mut ui = hooks;
            let history = load_history(&session, &sender, &turn_id);
            let images = session.attachments.resolve(&prompt);
            let result = run_query(
                &session,
                history,
                &prompt,
                &images,
                &mut ui,
                Some(cancel_rx),
            )
            .await;
            let _ = sender.send(AdapterEvent::TurnFinished { turn_id, result });
        });
        Ok(())
    }

    fn request_cancel(&mut self, command_id: &str, turn_id: &str) -> Result<(), JsonEventsError> {
        let Some((active_id, cancel)) = self
            .active
            .as_ref()
            .map(|active| (active.id.clone(), active.cancel.clone()))
        else {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id.to_string()),
                Some(turn_id.to_string()),
                "BAD_ARGUMENT",
                "no turn is active",
                EventErrorLevel::Page,
                true,
            );
        };
        if active_id != turn_id {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id.to_string()),
                Some(turn_id.to_string()),
                "BAD_ARGUMENT",
                "turnId does not match the active turn",
                EventErrorLevel::Page,
                true,
            );
        }
        self.cancel_prompts(PromptResolvedReason::TurnCancelled);
        cancel.send_replace(true);
        Ok(())
    }

    fn resolve_prompt(
        &mut self,
        command_id: String,
        prompt_id: &str,
        response: PromptResponse,
    ) -> Result<(), JsonEventsError> {
        let Some(index) = self
            .prompts
            .iter()
            .position(|prompt| prompt.prompt_id == prompt_id)
        else {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                self.active.as_ref().map(|active| active.id.clone()),
                "BAD_ARGUMENT",
                "promptId is not live",
                EventErrorLevel::Field,
                true,
            );
        };
        if !prompt_response_matches(&self.prompts[index], &response) {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                Some(self.prompts[index].turn_id.clone()),
                "BAD_ARGUMENT",
                "response does not match the prompt",
                EventErrorLevel::Field,
                true,
            );
        }
        let prompt = self.prompts.remove(index);
        let PendingPrompt {
            turn_id,
            prompt_id,
            options,
            reply,
            ..
        } = prompt;
        let resolved = match (reply, response) {
            (PromptReply::Permission(reply), PromptResponse::Option { option_id }) => {
                if option_id == "allow" {
                    reply.send(true).is_ok()
                } else if option_id == "deny" {
                    reply.send(false).is_ok()
                } else {
                    false
                }
            }
            (PromptReply::Permission(reply), PromptResponse::Cancel) => reply.send(false).is_ok(),
            (PromptReply::Question(reply), PromptResponse::Option { option_id }) => {
                let index = option_id
                    .strip_prefix("option-")
                    .and_then(|value| value.parse().ok())
                    .filter(|index: &usize| *index < options.len());
                match index {
                    Some(index) => reply.send(Some(AskAnswer::Option(index))).is_ok(),
                    None => false,
                }
            }
            (PromptReply::Question(reply), PromptResponse::Text { text }) => {
                reply.send(Some(AskAnswer::Other(text))).is_ok()
            }
            (PromptReply::Question(reply), PromptResponse::Cancel) => reply.send(None).is_ok(),
            (PromptReply::Permission(_), PromptResponse::Text { .. }) => false,
        };
        if !resolved {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                Some(turn_id),
                "BAD_ARGUMENT",
                "response does not match the prompt",
                EventErrorLevel::Field,
                true,
            );
        }
        self.emit(CliEvent::PromptResolved {
            base: EventBase::default(),
            turn_id,
            prompt_id,
            command_id: Some(command_id),
            reason: PromptResolvedReason::Responded,
        })
    }

    async fn list_models(
        &mut self,
        command_id: String,
        provider: String,
    ) -> Result<(), JsonEventsError> {
        let client = match self.session.client.with_provider(&provider) {
            Ok(client) => client,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    "CONFIG_INVALID",
                    &error,
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        match client.list_models().await {
            Ok(models) => self.emit(CliEvent::ModelsResult {
                base: EventBase::default(),
                command_id,
                provider,
                models,
            }),
            Err(error) => self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                error.error_code(),
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            ),
        }
    }

    fn list_providers(&mut self, command_id: String) -> Result<(), JsonEventsError> {
        let providers = provider_inventory(&self.session.client);
        self.emit(CliEvent::ProvidersResult {
            base: EventBase::default(),
            command_id,
            providers,
        })
    }

    fn get_settings(&mut self, command_id: String) -> Result<(), JsonEventsError> {
        let project_dir = std::path::PathBuf::from(&self.metadata.cwd);
        self.emit(CliEvent::SettingsResult {
            base: EventBase::default(),
            command_id,
            settings: sanitized_settings(&self.session.settings, &self.session.client),
            layers: settings_layers(&self.session.user_config_dir, &project_dir),
        })
    }

    fn subscribe_context(&mut self, command_id: String) -> Result<(), JsonEventsError> {
        self.context_subscribed
            .store(true, std::sync::atomic::Ordering::Release);
        let messages = self
            .session
            .runtime
            .transcript
            .borrow()
            .clone()
            .and_then(|transcript| transcript.load_messages().ok())
            .unwrap_or_default();
        let used_tokens = crate::compact::estimate_tokens(&self.session.system, &messages, &[]);
        let model = self.session.runtime.model.borrow().clone();
        self.emit(CliEvent::ContextUsage {
            base: EventBase::default(),
            command_id: Some(command_id),
            turn_id: None,
            used_tokens,
            context_window: crate::budget::context_window_for(
                &self.session.client.models(),
                &model,
            ),
        })
    }

    fn require_idle(&mut self, command_id: &str, operation: &str) -> Result<bool, JsonEventsError> {
        if self.active.is_none() {
            return Ok(true);
        }
        self.emit_error(
            ErrorScope::Command,
            Some(command_id.to_string()),
            self.active.as_ref().map(|active| active.id.clone()),
            "BAD_ARGUMENT",
            &format!("{operation} is only available while the main session is idle"),
            EventErrorLevel::Page,
            true,
        )?;
        Ok(false)
    }

    fn rename_session(&mut self, command_id: String, name: &str) -> Result<(), JsonEventsError> {
        if self.active.is_some() {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                self.active.as_ref().map(|active| active.id.clone()),
                "BAD_ARGUMENT",
                "session.rename is only available while idle",
                EventErrorLevel::Page,
                true,
            );
        }
        let previous_session_id = self.metadata.session_id.clone();
        let Some(transcript) = self.session.runtime.transcript.borrow().clone() else {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "STORAGE_ERROR",
                "this session has no transcript",
                EventErrorLevel::Page,
                true,
            );
        };
        match transcript.rename(name) {
            Ok(transcript) => {
                let renamed_session_id = transcript.name();
                if let Err(error) = self
                    .session
                    .tasks
                    .rename_key(&previous_session_id, &renamed_session_id)
                {
                    return self.emit_error(
                        ErrorScope::Command,
                        Some(command_id),
                        None,
                        error.error_code(),
                        &format!(
                            "session was renamed, but its Task list could not follow it: {error}"
                        ),
                        EventErrorLevel::Page,
                        true,
                    );
                }
                let _ = self
                    .session
                    .runtime
                    .transcript_tx
                    .send(Some(transcript.clone()));
                self.metadata.session_id = renamed_session_id;
                self.metadata.transcript_path = transcript.path().display().to_string();
                self.writer
                    .set_session_id(Some(self.metadata.session_id.clone()));
                self.emit(CliEvent::SessionRenamed {
                    base: EventBase::default(),
                    command_id,
                    previous_session_id,
                    metadata: self.metadata.clone(),
                })
            }
            Err(error) => self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                error.error_code(),
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            ),
        }
    }

    fn delete_session(&mut self, command_id: String) -> Result<(), JsonEventsError> {
        if self.active.is_some() {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                self.active.as_ref().map(|active| active.id.clone()),
                "BAD_ARGUMENT",
                "session.delete is only available while idle",
                EventErrorLevel::Page,
                true,
            );
        }
        let deleted_session_id = self.metadata.session_id.clone();
        let Some(transcript) = self.session.runtime.transcript.borrow().clone() else {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "STORAGE_ERROR",
                "this session has no transcript",
                EventErrorLevel::Page,
                true,
            );
        };
        if let Err(error) = transcript.delete() {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                error.error_code(),
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            );
        }
        let _ = self.session.runtime.transcript_tx.send(None);
        self.emit(CliEvent::SessionDeleted {
            base: EventBase::default(),
            command_id,
            deleted_session_id,
        })
    }

    async fn fork_session(
        &mut self,
        command_id: String,
        reason: crate::transcript::ForkReason,
        source_turn_id: Option<&str>,
        source_revision: Option<&str>,
    ) -> Result<(), JsonEventsError> {
        if self.active.is_some() {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                self.active.as_ref().map(|active| active.id.clone()),
                "SESSION_BUSY",
                "session.fork is only available while the main session is idle",
                EventErrorLevel::Page,
                true,
            );
        }
        if reason == crate::transcript::ForkReason::EditLastPrompt
            && (self.session.agents.has_running_work()
                || self.session.team_tasks.has_running_work())
        {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "SESSION_BUSY",
                "editing is unavailable while background Agent or Team work is running",
                EventErrorLevel::Page,
                true,
            );
        }
        let source_session_id = self.metadata.session_id.clone();
        let Some(transcript) = self.session.runtime.transcript.borrow().clone() else {
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                "STORAGE_ERROR",
                "this session has no transcript",
                EventErrorLevel::Page,
                true,
            );
        };
        let result = match reason {
            crate::transcript::ForkReason::EditLastPrompt => transcript.fork_edit_last_prompt(
                &self.session.home,
                &self.session.cwd(),
                crate::transcript::EditForkPoint {
                    turn_id: source_turn_id,
                    content_revision: source_revision,
                },
            ),
            crate::transcript::ForkReason::RecoverInterrupted => {
                transcript.fork_recover_interrupted(&self.session.home, &self.session.cwd())
            }
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    error.error_code(),
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        if let Err(error) = self.session.tasks.fork_to(&result.transcript.name()).await {
            let _ = result.transcript.delete();
            return self.emit_error(
                ErrorScope::Command,
                Some(command_id),
                None,
                error.error_code(),
                &error.to_string(),
                EventErrorLevel::Page,
                true,
            );
        }
        let mut metadata = self.metadata.clone();
        metadata.session_id = result.transcript.name();
        metadata.transcript_path = result.transcript.path().display().to_string();
        metadata.resumed = true;
        metadata.transcript_revision = match result.transcript.transcript_revision() {
            Ok(revision) => revision,
            Err(error) => {
                return self.emit_error(
                    ErrorScope::Command,
                    Some(command_id),
                    None,
                    error.error_code(),
                    &error.to_string(),
                    EventErrorLevel::Page,
                    true,
                );
            }
        };
        metadata.parent_session_id = Some(source_session_id.clone());
        metadata.fork_reason = Some(reason);
        // session.forked is a cross-process handoff. On Windows the child cannot even be
        // read while these exclusive handles remain open, so release them before the event
        // becomes observable to the GUI.
        result.transcript.release_active_lock();
        self.emit(CliEvent::SessionForked {
            base: EventBase::default(),
            command_id,
            source_session_id,
            reason,
            metadata,
            warnings: result.warnings,
        })
    }

    fn handle_adapter_event(&mut self, event: AdapterEvent) -> Result<(), JsonEventsError> {
        match event {
            AdapterEvent::Cli(event) => self.emit(*event),
            AdapterEvent::ContextUsage {
                turn_id,
                used_tokens,
                context_window,
            } => self.emit(CliEvent::ContextUsage {
                base: EventBase::default(),
                command_id: None,
                turn_id: Some(turn_id),
                used_tokens,
                context_window,
            }),
            AdapterEvent::TeamTask(event) => match event {
                crate::team_tasks::TeamTaskEvent::Updated(task) => {
                    self.emit(CliEvent::TeamTaskUpdated {
                        base: EventBase::default(),
                        command_id: None,
                        action: "changed".to_string(),
                        task,
                        detail: None,
                    })
                }
                crate::team_tasks::TeamTaskEvent::Message { task_id, message } => {
                    self.emit(CliEvent::TeamTaskMessage {
                        base: EventBase::default(),
                        task_id,
                        message,
                    })
                }
                crate::team_tasks::TeamTaskEvent::LobbyMessage(message) => {
                    self.emit(CliEvent::TeamLobbyMessage {
                        base: EventBase::default(),
                        message,
                    })
                }
            },
            AdapterEvent::Agent(event) => {
                let _ = self
                    .session
                    .team_tasks
                    .settle_ready_tasks(&self.session.agents);
                let task_id = self
                    .session
                    .team_tasks
                    .active_task_for_member(&event.name)
                    .map(|task| task.id);
                let status = event
                    .state
                    .map(crate::agents::AgentState::label)
                    .unwrap_or("offline")
                    .to_string();
                self.emit(CliEvent::TeamMemberUpdated {
                    base: EventBase::default(),
                    member: event.name,
                    status,
                    task_id,
                })
            }
            AdapterEvent::Prompt(prompt) => {
                self.emit(CliEvent::PromptRequest {
                    base: EventBase::default(),
                    turn_id: prompt.turn_id.clone(),
                    prompt_id: prompt.prompt_id.clone(),
                    kind: prompt.kind,
                    title: prompt.title.clone(),
                    question: prompt.question.clone(),
                    options: prompt.options.clone(),
                    allow_free_text: prompt.allow_free_text,
                })?;
                self.prompts.push(prompt);
                Ok(())
            }
            AdapterEvent::TurnFinished { turn_id, result } => {
                let cancelled = self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.id == turn_id && *active.cancel.borrow());
                self.active = None;
                self.cancel_prompts(if self.close_command_id.is_some() {
                    PromptResolvedReason::SessionClosing
                } else {
                    PromptResolvedReason::TurnCancelled
                });
                if cancelled {
                    let (command_id, reason) = if self.cancel_command_id.is_some() {
                        (
                            self.cancel_command_id.take(),
                            TurnCancelledReason::Requested,
                        )
                    } else if self.close_command_id.is_some() {
                        (
                            self.close_command_id.clone(),
                            TurnCancelledReason::SessionClosing,
                        )
                    } else {
                        (None, TurnCancelledReason::StdinEof)
                    };
                    self.finish_turn_index(&turn_id, crate::transcript::TurnStatus::Cancelled)?;
                    self.emit(CliEvent::TurnCancelled {
                        base: EventBase::default(),
                        turn_id,
                        command_id,
                        reason,
                    })?;
                    if let Some(command_id) = self.close_command_id.take() {
                        self.emit(CliEvent::SessionClosed {
                            base: EventBase::default(),
                            command_id,
                        })?;
                    }
                    return Ok(());
                }
                match result {
                    Ok(outcome) if outcome.aborted => {
                        let reason = if self.cancel_command_id.is_some() {
                            TurnCancelledReason::Requested
                        } else {
                            TurnCancelledReason::StdinEof
                        };
                        let command_id = self.cancel_command_id.take();
                        self.finish_turn_index(&turn_id, crate::transcript::TurnStatus::Cancelled)?;
                        self.emit(CliEvent::TurnCancelled {
                            base: EventBase::default(),
                            turn_id,
                            command_id,
                            reason,
                        })
                    }
                    Ok(_) => {
                        self.finish_turn_index(&turn_id, crate::transcript::TurnStatus::Completed)?;
                        self.emit(CliEvent::TurnCompleted {
                            base: EventBase::default(),
                            turn_id,
                            output_tokens: None,
                        })
                    }
                    Err(error) => {
                        self.exit_code = 1;
                        self.finish_turn_index(&turn_id, crate::transcript::TurnStatus::Error)?;
                        self.emit_error(
                            ErrorScope::Turn,
                            None,
                            Some(turn_id),
                            error.error_code(),
                            &error.to_string(),
                            EventErrorLevel::Flow,
                            true,
                        )
                    }
                }
            }
        }
    }

    fn handle_eof(&mut self) -> Result<(), JsonEventsError> {
        if let Some(cancel) = self.active.as_ref().map(|active| active.cancel.clone()) {
            self.cancel_prompts(PromptResolvedReason::TurnCancelled);
            cancel.send_replace(true);
            self.closing = true;
        } else {
            self.closing = true;
        }
        Ok(())
    }

    fn finish_turn_index(
        &mut self,
        turn_id: &str,
        status: crate::transcript::TurnStatus,
    ) -> Result<(), JsonEventsError> {
        let Some(transcript) = self.session.runtime.transcript.borrow().clone() else {
            return Ok(());
        };
        if let Err(error) = transcript.finish_turn(turn_id, status) {
            self.emit(CliEvent::Warning {
                base: EventBase::default(),
                turn_id: Some(turn_id.to_string()),
                code: Some(error.error_code().to_string()),
                msg: sanitize_msg(&format!("failed to update turn index: {error}")),
            })?;
        }
        Ok(())
    }

    fn cancel_prompts(&mut self, reason: PromptResolvedReason) {
        let pending: Vec<PendingPrompt> = self.prompts.drain(..).collect();
        for prompt in pending {
            match prompt.reply {
                PromptReply::Permission(reply) => {
                    let _ = reply.send(false);
                }
                PromptReply::Question(reply) => {
                    let _ = reply.send(None);
                }
            }
            let _ = self.emit(CliEvent::PromptResolved {
                base: EventBase::default(),
                turn_id: prompt.turn_id,
                prompt_id: prompt.prompt_id,
                command_id: None,
                reason,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_error(
        &mut self,
        scope: ErrorScope,
        command_id: Option<String>,
        turn_id: Option<String>,
        code: &str,
        msg: &str,
        level: EventErrorLevel,
        recoverable: bool,
    ) -> Result<(), JsonEventsError> {
        self.emit(CliEvent::Error {
            base: EventBase::default(),
            scope,
            command_id,
            turn_id,
            code: code.to_string(),
            msg: sanitize_msg(msg),
            level,
            recoverable,
        })
    }
}

fn merge_team_value(existing: serde_json::Value, incoming: serde_json::Value) -> serde_json::Value {
    let mut incoming = match incoming {
        serde_json::Value::Object(incoming) => incoming,
        other => return other,
    };
    let existing = match existing {
        serde_json::Value::Object(existing) => existing,
        _ => return serde_json::Value::Object(incoming),
    };
    let known_root = [
        "schemaVersion",
        "name",
        "leader",
        "channel",
        "channels",
        "members",
        "teams",
    ];
    let mut merged: serde_json::Map<String, serde_json::Value> = existing
        .iter()
        .filter(|(key, _)| !known_root.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();

    if let Some(serde_json::Value::Object(channel)) = incoming.get_mut("channel")
        && let Some(serde_json::Value::Object(existing_channel)) = existing.get("channel")
    {
        for (key, value) in existing_channel {
            if !matches!(key.as_str(), "mode" | "messageLimit") && !channel.contains_key(key) {
                channel.insert(key.clone(), value.clone());
            }
        }
    }
    if let Some(serde_json::Value::Array(members)) = incoming.get_mut("members")
        && let Some(serde_json::Value::Array(existing_members)) = existing.get("members")
    {
        for member in members {
            let serde_json::Value::Object(member_object) = member else {
                continue;
            };
            let name = member_object
                .get("name")
                .and_then(serde_json::Value::as_str);
            let Some(serde_json::Value::Object(existing_member)) =
                existing_members.iter().find(|candidate| {
                    candidate.get("name").and_then(serde_json::Value::as_str) == name
                })
            else {
                continue;
            };
            for (key, value) in existing_member {
                if !matches!(
                    key.as_str(),
                    "name" | "agent" | "avatar" | "model" | "provider" | "thinking"
                ) && !member_object.contains_key(key)
                {
                    member_object.insert(key.clone(), value.clone());
                }
            }
        }
    }
    merged.extend(incoming);
    serde_json::Value::Object(merged)
}

fn read_commands<R: BufRead>(
    mut reader: R,
    sender: mpsc::UnboundedSender<Result<ClientCommand, JsonEventsError>>,
) {
    loop {
        let mut line = Vec::new();
        let read = match reader.read_until(b'\n', &mut line) {
            Ok(read) => read,
            Err(error) => {
                let _ = sender.send(Err(JsonEventsError::Io(error)));
                return;
            }
        };
        if read == 0 {
            return;
        }
        if line.ends_with(b"\n") {
            line.pop();
            if line.ends_with(b"\r") {
                line.pop();
            }
        }
        let command = parse_command_line(&line);
        let fatal = command.is_err();
        if sender.send(command).is_err() || fatal {
            return;
        }
    }
}

fn json_hooks(
    sender: mpsc::UnboundedSender<AdapterEvent>,
    turn_id: String,
    next_prompt_id: Arc<std::sync::atomic::AtomicU64>,
    context_subscribed: Arc<std::sync::atomic::AtomicBool>,
) -> UiHooks {
    let event_sender = sender.clone();
    let ready_sender = sender.clone();
    let done_sender = sender.clone();
    let warning_sender = sender.clone();
    let permission_sender = sender.clone();
    let permission_turn_id = turn_id.clone();
    let ready_turn_id = turn_id.clone();
    let permission_ids = next_prompt_id.clone();
    let question_sender = sender.clone();
    let question_turn_id = turn_id.clone();
    let done_turn_id = turn_id.clone();
    let context_sender = sender.clone();
    let context_turn_id = turn_id.clone();
    UiHooks {
        on_event: Box::new(move |event| {
            if let StreamEvent::TextDelta { text, .. } = event {
                let _ = event_sender.send(AdapterEvent::Cli(Box::new(CliEvent::TextDelta {
                    base: EventBase::default(),
                    turn_id: turn_id.clone(),
                    delta: text.clone(),
                })));
            }
        }),
        on_stream_retry: Box::new(|| {}),
        on_context_usage: Arc::new(move |used_tokens, context_window| {
            if context_subscribed.load(std::sync::atomic::Ordering::Acquire) {
                let _ = context_sender.send(AdapterEvent::ContextUsage {
                    turn_id: context_turn_id.clone(),
                    used_tokens,
                    context_window,
                });
            }
        }),
        on_tool_ready: Box::new(move |tool_call_id, name, input, _standalone| {
            let summary = crate::query::summarize_input(&name, &input);
            let _ = ready_sender.send(AdapterEvent::Cli(Box::new(CliEvent::ToolReady {
                base: EventBase::default(),
                turn_id: ready_turn_id.clone(),
                tool_call_id,
                name,
                summary,
            })));
        }),
        on_tool_done: Box::new(move |done| {
            // TODO(upstream JSON-events issue): tool.done currently flattens output to text. Preserve
            // image blocks through a bounded reference or chunked JSON-events extension.
            let status = match done.status {
                ToolCallStatus::Done => ToolEventStatus::Done,
                ToolCallStatus::Error => ToolEventStatus::Error,
                ToolCallStatus::Interrupted => ToolEventStatus::Interrupted,
            };
            let _ = done_sender.send(AdapterEvent::Cli(Box::new(CliEvent::ToolDone {
                base: EventBase::default(),
                turn_id: done_turn_id.clone(),
                tool_call_id: done.tool_call_id.clone(),
                name: done.name.clone(),
                summary: done.summary.clone(),
                status,
                output: done.output.clone(),
                duration_ms: done.duration_ms,
            })));
        }),
        on_round_end: Box::new(|| {}),
        on_warning: Box::new(move |message| {
            let _ = warning_sender.send(AdapterEvent::Cli(Box::new(CliEvent::Warning {
                base: EventBase::default(),
                turn_id: None,
                code: None,
                msg: sanitize_msg(&message),
            })));
        }),
        ask: Arc::new(move |tool_name, reason| {
            let prompt_id = format!(
                "prompt-{}",
                permission_ids.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );
            let (reply, receiver) = oneshot::channel();
            let prompt = PendingPrompt {
                turn_id: permission_turn_id.clone(),
                prompt_id,
                kind: PromptKind::Permission,
                title: format!("Allow running {tool_name}"),
                question: reason.to_string(),
                options: vec![
                    PromptOption {
                        id: "allow".to_string(),
                        label: "Allow".to_string(),
                        description: None,
                    },
                    PromptOption {
                        id: "deny".to_string(),
                        label: "Deny".to_string(),
                        description: None,
                    },
                ],
                allow_free_text: false,
                reply: PromptReply::Permission(reply),
            };
            let sent = permission_sender.send(AdapterEvent::Prompt(prompt)).is_ok();
            Box::pin(async move {
                if !sent {
                    return false;
                }
                receiver.await.unwrap_or(false)
            })
        }),
        ask_question: Arc::new(move |title, question, options| {
            let prompt_id = format!(
                "prompt-{}",
                next_prompt_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );
            let (reply, receiver) = oneshot::channel();
            let options = options
                .into_iter()
                .enumerate()
                .map(|(index, (label, description))| PromptOption {
                    id: format!("option-{index}"),
                    label,
                    description,
                })
                .collect();
            let prompt = PendingPrompt {
                turn_id: question_turn_id.clone(),
                prompt_id,
                kind: PromptKind::Question,
                title,
                question,
                options,
                allow_free_text: true,
                reply: PromptReply::Question(reply),
            };
            let sent = question_sender.send(AdapterEvent::Prompt(prompt)).is_ok();
            Box::pin(async move {
                if !sent {
                    return None;
                }
                receiver.await.unwrap_or(None)
            })
        }),
    }
}

fn load_history(
    session: &Session,
    sender: &mpsc::UnboundedSender<AdapterEvent>,
    turn_id: &str,
) -> Vec<Message> {
    let Some(transcript) = session.runtime.transcript.borrow().clone() else {
        return Vec::new();
    };
    match transcript.load_messages() {
        Ok(messages) => messages,
        Err(crate::transcript::TranscriptError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            Vec::new()
        }
        Err(error) => {
            let _ = sender.send(AdapterEvent::Cli(Box::new(CliEvent::Warning {
                base: EventBase::default(),
                turn_id: Some(turn_id.to_string()),
                code: Some(error.error_code().to_string()),
                msg: sanitize_msg(&error.to_string()),
            })));
            Vec::new()
        }
    }
}

pub fn fatal_event<W: Write>(
    writer: W,
    error: &(dyn std::error::Error + 'static),
) -> Result<(), JsonEventsError> {
    let mut writer = EventWriter::new(writer);
    writer.emit(CliEvent::Error {
        base: EventBase::default(),
        scope: ErrorScope::Session,
        command_id: None,
        turn_id: None,
        code: error_code_boxed(error).to_string(),
        msg: sanitize_msg(&error.to_string()),
        level: EventErrorLevel::Flow,
        recoverable: false,
    })
}

/// Side-effect-free capability probe: emits exactly one `protocol.ready`
/// record with the bingo and protocol versions, then returns so the process
/// exits 0. Never loads providers, hooks, teams, transcripts, or stdin.
pub fn probe_event<W: Write>(mut writer: W) -> Result<(), JsonEventsError> {
    let mut event_writer = EventWriter::new(&mut writer);
    event_writer.emit(CliEvent::ProtocolReady {
        base: EventBase::default(),
        metadata: ProbeMetadata {
            bingo_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: capabilities(),
        },
    })
}

/// Side-effect-free settings inspection transport: emits `inspection.ready`
/// (sessionId=null), then serves only `providers.list`, `models.list`, and
/// `session.close` over NDJSON. Never creates a transcript or runs hooks/teams.
pub async fn run_inspect<R: BufRead, W: Write>(
    client: crate::api::client::Client,
    settings: crate::settings::Settings,
    user_dir: &Path,
    project_dir: &Path,
    reader: R,
    mut writer: W,
) -> Result<i32, JsonEventsError> {
    let mut event_writer = EventWriter::new(&mut writer);
    event_writer.emit(CliEvent::InspectionReady {
        base: EventBase::default(),
        metadata: ProbeMetadata {
            bingo_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION,
            capabilities: capabilities(),
        },
    })?;
    let mut seen_command_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in reader.lines() {
        let line = line?;
        let command = parse_command_line(line.as_bytes())?;
        let command_id = command.command_id().to_string();
        if !seen_command_ids.insert(command_id.clone()) {
            event_writer.emit(CliEvent::Error {
                base: EventBase::default(),
                scope: ErrorScope::Command,
                command_id: Some(command_id),
                turn_id: None,
                code: "BAD_ARGUMENT".to_string(),
                msg: "commandId must be unique".to_string(),
                level: EventErrorLevel::Field,
                recoverable: true,
            })?;
            continue;
        }
        match command {
            ClientCommand::ProvidersList { .. } => {
                let providers = provider_inventory(&client);
                event_writer.emit(CliEvent::ProvidersResult {
                    base: EventBase::default(),
                    command_id,
                    providers,
                })?;
            }
            ClientCommand::ModelsList { provider, .. } => {
                let models = match client.with_provider(&provider) {
                    Ok(provider_client) => match provider_client.list_models().await {
                        Ok(models) => models,
                        Err(error) => {
                            event_writer.emit(CliEvent::Error {
                                base: EventBase::default(),
                                scope: ErrorScope::Command,
                                command_id: Some(command_id),
                                turn_id: None,
                                code: error.error_code().to_string(),
                                msg: sanitize_msg(&error.to_string()),
                                level: EventErrorLevel::Page,
                                recoverable: true,
                            })?;
                            continue;
                        }
                    },
                    Err(error) => {
                        event_writer.emit(CliEvent::Error {
                            base: EventBase::default(),
                            scope: ErrorScope::Command,
                            command_id: Some(command_id),
                            turn_id: None,
                            code: "CONFIG_INVALID".to_string(),
                            msg: sanitize_msg(&error),
                            level: EventErrorLevel::Page,
                            recoverable: true,
                        })?;
                        continue;
                    }
                };
                event_writer.emit(CliEvent::ModelsResult {
                    base: EventBase::default(),
                    command_id,
                    provider,
                    models,
                })?;
            }
            ClientCommand::SettingsGet { .. } => {
                event_writer.emit(CliEvent::SettingsResult {
                    base: EventBase::default(),
                    command_id,
                    settings: sanitized_settings(&settings, &client),
                    layers: settings_layers(user_dir, project_dir),
                })?;
            }
            ClientCommand::SessionClose { .. } => {
                event_writer.emit(CliEvent::SessionClosed {
                    base: EventBase::default(),
                    command_id,
                })?;
                return Ok(0);
            }
            _ => {
                return Err(JsonEventsError::BadArgument(
                    "command is not allowed in inspect mode (only providers.list, models.list, settings.get, session.close)"
                        .to_string(),
                ));
            }
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests;

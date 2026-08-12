use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};

use crate::api::contract::StreamEvent;
use crate::api::types::Message;
use crate::error::{ErrorCode, error_code_boxed, sanitize_msg};
use crate::query::{AskAnswer, Session, ToolCallStatus, UiHooks, run_query};
use crate::transcript::Transcript;

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_COMMAND_LINE_BYTES: usize = 1024 * 1024;
pub const MAX_EVENT_LINE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_PROMPT_CHARS: usize = 1_000_000;
pub const MAX_RESPONSE_CHARS: usize = 100_000;
pub const MAX_RENAME_CHARS: usize = 80;

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
            Self::TurnStart {
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
            | Self::SessionRename {
                protocol_version, ..
            }
            | Self::SessionDelete {
                protocol_version, ..
            }
            | Self::SessionClose {
                protocol_version, ..
            } => *protocol_version,
        }
    }

    fn command_id(&self) -> &str {
        match self {
            Self::TurnStart { command_id, .. }
            | Self::TurnCancel { command_id, .. }
            | Self::PromptRespond { command_id, .. }
            | Self::ModelsList { command_id, .. }
            | Self::ProvidersList { command_id, .. }
            | Self::SessionRename { command_id, .. }
            | Self::SessionDelete { command_id, .. }
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
            _ => {}
        }
        Ok(())
    }
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeMetadata {
    pub bingo_version: String,
    pub protocol_version: u8,
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
    #[serde(rename = "session.ready")]
    SessionReady {
        #[serde(flatten)]
        base: EventBase,
        metadata: CliSessionMetadata,
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
            Self::SessionReady { base: slot, .. }
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
            | Self::Warning { base: slot, .. }
            | Self::TurnCompleted { base: slot, .. }
            | Self::TurnCancelled { base: slot, .. }
            | Self::SessionRenamed { base: slot, .. }
            | Self::SessionDeleted { base: slot, .. }
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
        }
    }
}

#[derive(Debug)]
enum AdapterEvent {
    Cli(CliEvent),
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
    closing: bool,
    exit_code: i32,
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
            ClientCommand::SessionRename {
                command_id, name, ..
            } => self.rename_session(command_id, &name),
            ClientCommand::SessionDelete { command_id, .. } => {
                self.delete_session(command_id)?;
                self.closing = true;
                Ok(())
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
        self.emit(CliEvent::TurnStarted {
            base: EventBase::default(),
            command_id,
            turn_id: turn_id.clone(),
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
        let hooks = json_hooks(sender.clone(), turn_id.clone(), self.next_prompt_id.clone());
        tokio::spawn(async move {
            let mut ui = hooks;
            let history = load_history(&session, &sender, &turn_id);
            let result = run_query(&session, history, &prompt, &[], &mut ui, Some(cancel_rx)).await;
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
                let _ = self
                    .session
                    .runtime
                    .transcript_tx
                    .send(Some(transcript.clone()));
                self.metadata.session_id = transcript.name();
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

    fn handle_adapter_event(&mut self, event: AdapterEvent) -> Result<(), JsonEventsError> {
        match event {
            AdapterEvent::Cli(event) => self.emit(event),
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
                        self.emit(CliEvent::TurnCancelled {
                            base: EventBase::default(),
                            turn_id,
                            command_id,
                            reason,
                        })
                    }
                    Ok(_) => self.emit(CliEvent::TurnCompleted {
                        base: EventBase::default(),
                        turn_id,
                        output_tokens: None,
                    }),
                    Err(error) => {
                        self.exit_code = 1;
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
) -> UiHooks {
    let event_sender = sender.clone();
    let ready_sender = sender.clone();
    let done_sender = sender.clone();
    let warning_sender = sender.clone();
    let permission_sender = sender.clone();
    let permission_turn_id = turn_id.clone();
    let ready_turn_id = turn_id.clone();
    let permission_ids = next_prompt_id.clone();
    let question_sender = sender;
    let question_turn_id = turn_id.clone();
    let done_turn_id = turn_id.clone();
    UiHooks {
        on_event: Box::new(move |event| {
            if let StreamEvent::TextDelta { text, .. } = event {
                let _ = event_sender.send(AdapterEvent::Cli(CliEvent::TextDelta {
                    base: EventBase::default(),
                    turn_id: turn_id.clone(),
                    delta: text.clone(),
                }));
            }
        }),
        // Stream retries and context usage have no protocol events yet; a later
        // commit can surface them once the schema grows fields for them.
        on_stream_retry: Box::new(|| {}),
        on_context_usage: Arc::new(|_, _| {}),
        on_tool_ready: Box::new(move |tool_call_id, name, input, _standalone| {
            let summary = crate::query::summarize_input(&name, &input);
            let _ = ready_sender.send(AdapterEvent::Cli(CliEvent::ToolReady {
                base: EventBase::default(),
                turn_id: ready_turn_id.clone(),
                tool_call_id,
                name,
                summary,
            }));
        }),
        on_tool_done: Box::new(move |done| {
            let status = match done.status {
                ToolCallStatus::Done => ToolEventStatus::Done,
                ToolCallStatus::Error => ToolEventStatus::Error,
                ToolCallStatus::Interrupted => ToolEventStatus::Interrupted,
            };
            let _ = done_sender.send(AdapterEvent::Cli(CliEvent::ToolDone {
                base: EventBase::default(),
                turn_id: done_turn_id.clone(),
                tool_call_id: done.tool_call_id.clone(),
                name: done.name.clone(),
                summary: done.summary.clone(),
                status,
                output: done.output.clone(),
                duration_ms: done.duration_ms,
            }));
        }),
        on_round_end: Box::new(|| {}),
        on_warning: Box::new(move |message| {
            let _ = warning_sender.send(AdapterEvent::Cli(CliEvent::Warning {
                base: EventBase::default(),
                turn_id: None,
                code: None,
                msg: sanitize_msg(&message),
            }));
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
            let _ = sender.send(AdapterEvent::Cli(CliEvent::Warning {
                base: EventBase::default(),
                turn_id: Some(turn_id.to_string()),
                code: Some(error.error_code().to_string()),
                msg: sanitize_msg(&error.to_string()),
            }));
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
        },
    })
}

/// Side-effect-free settings inspection transport: emits `inspection.ready`
/// (sessionId=null), then serves only `providers.list`, `models.list`, and
/// `session.close` over NDJSON. Never creates a transcript or runs hooks/teams.
pub async fn run_inspect<R: BufRead, W: Write>(
    client: crate::api::client::Client,
    reader: R,
    mut writer: W,
) -> Result<i32, JsonEventsError> {
    let mut event_writer = EventWriter::new(&mut writer);
    event_writer.emit(CliEvent::InspectionReady {
        base: EventBase::default(),
        metadata: ProbeMetadata {
            bingo_version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION,
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
            ClientCommand::SessionClose { .. } => {
                event_writer.emit(CliEvent::SessionClosed {
                    base: EventBase::default(),
                    command_id,
                })?;
                return Ok(0);
            }
            _ => {
                return Err(JsonEventsError::BadArgument(
                    "command is not allowed in inspect mode (only providers.list, models.list, session.close)"
                        .to_string(),
                ));
            }
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn_start(prompt: String) -> String {
        serde_json::json!({
            "protocolVersion": 1,
            "type": "turn.start",
            "commandId": "command-1",
            "turnId": "turn-1",
            "prompt": prompt,
        })
        .to_string()
    }

    #[test]
    fn command_round_trip_uses_protocol_v1_shape() {
        let command = parse_command_line(turn_start("hello".to_string()).as_bytes())
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            command,
            ClientCommand::TurnStart {
                protocol_version: 1,
                command_id: "command-1".to_string(),
                turn_id: "turn-1".to_string(),
                prompt: "hello".to_string(),
            }
        );
        let value = serde_json::to_value(command).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(value["type"], "turn.start");
        assert_eq!(value["protocolVersion"], 1);
    }

    #[test]
    fn every_command_variant_round_trips() {
        let values = [
            serde_json::json!({
                "protocolVersion": 1,
                "type": "turn.cancel",
                "commandId": "command-1",
                "turnId": "turn-1"
            }),
            serde_json::json!({
                "protocolVersion": 1,
                "type": "prompt.respond",
                "commandId": "command-2",
                "promptId": "prompt-1",
                "response": {"kind": "option", "optionId": "allow"}
            }),
            serde_json::json!({
                "protocolVersion": 1,
                "type": "models.list",
                "commandId": "command-3",
                "provider": "default"
            }),
            serde_json::json!({
                "protocolVersion": 1,
                "type": "session.rename",
                "commandId": "command-4",
                "name": "Renamed"
            }),
            serde_json::json!({
                "protocolVersion": 1,
                "type": "session.delete",
                "commandId": "command-5"
            }),
            serde_json::json!({
                "protocolVersion": 1,
                "type": "session.close",
                "commandId": "command-6"
            }),
        ];
        for value in values {
            let line = value.to_string();
            let command = parse_command_line(line.as_bytes())
                .unwrap_or_else(|error| panic!("{value}: {error}"));
            assert_eq!(
                serde_json::to_value(command).unwrap_or_else(|error| panic!("{error}")),
                value
            );
        }
    }

    #[test]
    fn prompt_and_rename_bounds_count_unicode_scalars() {
        assert!(parse_command_line(turn_start("x".repeat(MAX_PROMPT_CHARS)).as_bytes()).is_ok());
        assert!(
            parse_command_line(turn_start("x".repeat(MAX_PROMPT_CHARS + 1)).as_bytes()).is_err()
        );
        assert!(parse_command_line(turn_start(" \n\t ".to_string()).as_bytes()).is_err());

        let response = |text: String| {
            serde_json::json!({
                "protocolVersion": 1,
                "type": "prompt.respond",
                "commandId": "command-1",
                "promptId": "prompt-1",
                "response": {"kind": "text", "text": text}
            })
            .to_string()
        };
        assert!(parse_command_line(response("界".repeat(MAX_RESPONSE_CHARS)).as_bytes()).is_ok());
        assert!(
            parse_command_line(response("界".repeat(MAX_RESPONSE_CHARS + 1)).as_bytes()).is_err()
        );

        let rename = |name: String| {
            serde_json::json!({
                "protocolVersion": 1,
                "type": "session.rename",
                "commandId": "command-1",
                "name": name,
            })
            .to_string()
        };
        assert!(parse_command_line(rename("界".repeat(MAX_RENAME_CHARS)).as_bytes()).is_ok());
        assert!(parse_command_line(rename("界".repeat(MAX_RENAME_CHARS + 1)).as_bytes()).is_err());
    }

    #[test]
    fn command_line_cap_is_enforced_before_parsing() {
        let line = vec![b'x'; MAX_COMMAND_LINE_BYTES + 1];
        let error = parse_command_line(&line).expect_err("oversized line must fail");
        assert!(error.to_string().contains("byte limit"));
    }

    #[test]
    fn version_and_unknown_commands_are_rejected() {
        let wrong_version = serde_json::json!({
            "protocolVersion": 2,
            "type": "session.close",
            "commandId": "command-1"
        });
        assert!(parse_command_line(wrong_version.to_string().as_bytes()).is_err());
        let unknown = serde_json::json!({
            "protocolVersion": 1,
            "type": "unknown",
            "commandId": "command-1"
        });
        assert!(parse_command_line(unknown.to_string().as_bytes()).is_err());
    }

    #[test]
    fn event_writer_assigns_gapless_sequence_and_ndjson_lines() {
        let mut output = Vec::new();
        {
            let mut writer = EventWriter::new(&mut output);
            writer.set_session_id(Some("session-1".to_string()));
            writer
                .emit(CliEvent::Warning {
                    base: EventBase::default(),
                    turn_id: None,
                    code: None,
                    msg: "one".to_string(),
                })
                .unwrap_or_else(|error| panic!("{error}"));
            writer
                .emit(CliEvent::Warning {
                    base: EventBase::default(),
                    turn_id: None,
                    code: None,
                    msg: "two".to_string(),
                })
                .unwrap_or_else(|error| panic!("{error}"));
        }
        let lines: Vec<serde_json::Value> = std::str::from_utf8(&output)
            .unwrap_or_else(|error| panic!("{error}"))
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|error| panic!("{error}")))
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["seq"], 1);
        assert_eq!(lines[1]["seq"], 2);
        assert_eq!(lines[0]["sessionId"], "session-1");
        assert!(output.ends_with(b"\n"));
    }

    #[test]
    fn exact_session_resolution_rejects_fragments_and_paths() {
        let root =
            std::env::temp_dir().join(format!("bingo-json-session-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let cwd = root.join("project");
        std::fs::create_dir_all(&cwd).unwrap_or_else(|error| panic!("{error}"));
        let transcript =
            crate::transcript::create(&home, &cwd).unwrap_or_else(|error| panic!("{error}"));
        transcript
            .append(&Message::user_text("hello"))
            .unwrap_or_else(|error| panic!("{error}"));
        let stem = transcript.name();
        assert_eq!(
            resolve_session(&home, &stem)
                .unwrap_or_else(|error| panic!("{error}"))
                .path(),
            transcript.path()
        );
        assert!(resolve_session(&home, &stem[..stem.len() - 1]).is_err());
        assert!(resolve_session(&home, "../session").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}

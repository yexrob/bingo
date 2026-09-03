//! The method table: which string names which type, in both directions.
//!
//! ACP's own crate keeps its method constants private, so the one place this
//! plugin writes a method string is here. A `Call` binds a request type to its
//! method and to the response that answers it, so a caller cannot pair the
//! wrong two; `Incoming` is the same table read backwards, for the lines the
//! agent starts.

use agent_client_protocol_schema::v1::{
    AuthenticateRequest, AuthenticateResponse, CancelNotification, CreateElicitationRequest,
    InitializeRequest, InitializeResponse, LoadSessionRequest, LoadSessionResponse,
    NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse, RequestPermissionRequest,
    ResumeSessionRequest, ResumeSessionResponse, SessionNotification,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

pub const INITIALIZE: &str = "initialize";
pub const AUTHENTICATE: &str = "authenticate";
pub const SESSION_NEW: &str = "session/new";
pub const SESSION_LOAD: &str = "session/load";
pub const SESSION_RESUME: &str = "session/resume";
pub const SESSION_PROMPT: &str = "session/prompt";
pub const SESSION_CANCEL: &str = "session/cancel";
pub const SESSION_UPDATE: &str = "session/update";
pub const SESSION_REQUEST_PERMISSION: &str = "session/request_permission";
pub const FS_READ_TEXT_FILE: &str = "fs/read_text_file";
pub const FS_WRITE_TEXT_FILE: &str = "fs/write_text_file";
pub const TERMINAL_CREATE: &str = "terminal/create";
pub const TERMINAL_OUTPUT: &str = "terminal/output";
pub const TERMINAL_RELEASE: &str = "terminal/release";
pub const TERMINAL_WAIT_FOR_EXIT: &str = "terminal/wait_for_exit";
pub const TERMINAL_KILL: &str = "terminal/kill";
pub const ELICITATION_CREATE: &str = "elicitation/create";

/// A request this plugin sends and the answer it expects back.
pub trait Call: Serialize + Send + Sync {
    const METHOD: &'static str;
    type Response: DeserializeOwned + Send;
}

/// A notification this plugin sends. Nothing answers it.
pub trait Notify: Serialize + Send + Sync {
    const METHOD: &'static str;
}

macro_rules! call {
    ($request:ty => $method:ident, $response:ty) => {
        impl Call for $request {
            const METHOD: &'static str = $method;
            type Response = $response;
        }
    };
}

call!(InitializeRequest => INITIALIZE, InitializeResponse);
call!(AuthenticateRequest => AUTHENTICATE, AuthenticateResponse);
call!(NewSessionRequest => SESSION_NEW, NewSessionResponse);
call!(LoadSessionRequest => SESSION_LOAD, LoadSessionResponse);
call!(ResumeSessionRequest => SESSION_RESUME, ResumeSessionResponse);
call!(PromptRequest => SESSION_PROMPT, PromptResponse);

impl Notify for CancelNotification {
    const METHOD: &'static str = SESSION_CANCEL;
}

/// A line the agent started. Everything ACP lets an agent ask a client that
/// this plugin does not answer is [`Incoming::Unsupported`] rather than a
/// silence: the agent is told `fs/*` and `terminal/*` are not here (ADR-0035
/// §6) instead of waiting for a reply that never comes.
#[derive(Debug)]
pub enum Incoming {
    /// The stream of a turn: chunks, tool calls, usage.
    Update(Box<SessionNotification>),
    /// The agent asking whether it may do something. Refused: it brings its
    /// own permission machinery (ADR-0035 §5).
    Permission(Box<RequestPermissionRequest>),
    /// The agent asking this client to collect something from a person.
    /// Declined at the same door, for the same reason.
    Elicitation(Box<CreateElicitationRequest>),
    /// A method this client declared it does not have.
    Unsupported,
}

/// Read a line the agent started. `Err` is a body that does not fit the method
/// it names — the agent's bug, reported as `invalid_params`.
pub fn incoming(method: &str, params: serde_json::Value) -> Result<Incoming, serde_json::Error> {
    match method {
        SESSION_UPDATE => Ok(Incoming::Update(Box::new(serde_json::from_value(params)?))),
        SESSION_REQUEST_PERMISSION => Ok(Incoming::Permission(Box::new(serde_json::from_value(
            params,
        )?))),
        ELICITATION_CREATE => Ok(Incoming::Elicitation(Box::new(serde_json::from_value(
            params,
        )?))),
        _ => Ok(Incoming::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;
    use serde_json::Value;

    /// What this plugin sends: the type must write the recorded body exactly,
    /// byte for byte, because those bytes are what a real adapter parses.
    fn writes<T: Serialize + DeserializeOwned>(recorded: Value) -> T {
        let parsed: T = reads(recorded.clone());
        assert_eq!(
            serde_json::to_value(&parsed).expect("it writes back"),
            recorded,
            "the value does not write the body it was read from"
        );
        parsed
    }

    /// What this plugin reads: an adapter omits what it has no opinion about,
    /// so an incoming body is not required to be what the type writes. The
    /// contract is that nothing is lost — parse, write, parse again, and the
    /// two values agree.
    fn reads<T: Serialize + DeserializeOwned>(recorded: Value) -> T {
        let parsed: T = serde_json::from_value(recorded.clone())
            .unwrap_or_else(|e| panic!("the recorded body parses: {e}\n{recorded:#}"));
        let written = serde_json::to_value(&parsed).expect("it writes back");
        let again: T = serde_json::from_value(written.clone())
            .unwrap_or_else(|e| panic!("what it wrote parses: {e}\n{written:#}"));
        assert_eq!(
            serde_json::to_value(&again).expect("and writes the same"),
            written,
            "reading the message loses something"
        );
        parsed
    }

    #[test]
    fn the_handshake_round_trips() {
        let request: InitializeRequest = writes(fixtures::initialize_request());
        assert_eq!(request.protocol_version.as_u16(), 1);
        assert!(!request.client_capabilities.terminal);
        let response: InitializeResponse = reads(fixtures::initialize_response());
        assert!(response.agent_capabilities.load_session);
        assert!(
            response
                .agent_capabilities
                .session_capabilities
                .resume
                .is_some()
        );
        let neither: InitializeResponse = reads(fixtures::initialize_response_without_restore());
        assert!(!neither.agent_capabilities.load_session);
        assert!(
            neither
                .agent_capabilities
                .session_capabilities
                .resume
                .is_none()
        );
        let unauthenticated: InitializeResponse =
            reads(fixtures::initialize_response_needing_auth());
        assert_eq!(unauthenticated.auth_methods.len(), 1);
    }

    #[test]
    fn every_session_door_round_trips() {
        let new: NewSessionRequest = writes(fixtures::new_session_request());
        // The recorded body carries no rows; since M39 a live one does, and
        // what goes in them is `servers.rs`'s question, not this fixture's.
        assert!(new.mcp_servers.is_empty(), "none in the recorded body");
        let opened: NewSessionResponse = reads(fixtures::new_session_response());
        assert_eq!(opened.session_id.0.as_ref(), "sess_abc123");
        writes::<LoadSessionRequest>(fixtures::load_session_request());
        reads::<LoadSessionResponse>(fixtures::load_session_response());
        writes::<ResumeSessionRequest>(fixtures::resume_session_request());
        reads::<ResumeSessionResponse>(fixtures::resume_session_response());
    }

    #[test]
    fn a_turn_round_trips() {
        writes::<PromptRequest>(fixtures::prompt_request());
        let ended: PromptResponse = reads(fixtures::prompt_response_with_usage());
        assert_eq!(
            ended.usage.map(|u| u.output_tokens),
            Some(64),
            "claude-agent-acp reports a turn's tokens; the field must survive"
        );
        let bare: PromptResponse = reads(fixtures::prompt_response_bare());
        assert!(
            bare.usage.is_none(),
            "codex-acp reports none, and none is not zero"
        );
        writes::<CancelNotification>(fixtures::cancel_notification());
    }

    #[test]
    fn a_method_the_table_does_not_know_is_unsupported_not_a_crash() {
        assert!(matches!(
            incoming(FS_READ_TEXT_FILE, serde_json::json!({ "path": "/tmp/x" }))
                .expect("an unknown method is not a parse failure"),
            Incoming::Unsupported
        ));
        assert!(matches!(
            incoming(TERMINAL_CREATE, Value::Null).expect("nor is a null body"),
            Incoming::Unsupported
        ));
    }

    #[test]
    fn a_body_that_does_not_fit_its_method_is_an_error() {
        assert!(incoming(SESSION_UPDATE, serde_json::json!({ "nope": true })).is_err());
    }

    #[test]
    fn the_lines_the_agent_starts_are_read_by_their_method() {
        assert!(matches!(
            incoming(SESSION_UPDATE, fixtures::update_agent_message_chunk()).expect("an update"),
            Incoming::Update(_)
        ));
        assert!(matches!(
            incoming(SESSION_REQUEST_PERMISSION, fixtures::request_permission())
                .expect("a permission request"),
            Incoming::Permission(_)
        ));
        assert!(matches!(
            incoming(ELICITATION_CREATE, fixtures::elicitation_create())
                .expect("an elicitation request"),
            Incoming::Elicitation(_)
        ));
    }

    /// The other door a person could be reached through, and the word this
    /// client answers it with (ADR-0035 §5).
    #[test]
    fn the_elicitation_door_round_trips() {
        reads::<CreateElicitationRequest>(fixtures::elicitation_create());
        writes::<agent_client_protocol_schema::v1::CreateElicitationResponse>(
            fixtures::elicitation_declined(),
        );
    }
}

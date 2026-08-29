//! The thirteen methods and the two notifications (ADR-0007): their names and
//! the shape of what goes in and comes back. Every params and result type is an
//! sdk type or a struct of sdk types.
//!
//! `METHODS` and `NOTIFICATIONS` are the one table; the dispatcher matches on
//! the names in [`name`], the schema walks the table, and `initialize` reports
//! it as the server's capabilities.

use bingo_sdk::{
    Activation, Answer, Catalog, CatalogKind, ClientIdentity, Frame, GatewayEvent, HistoryChunk,
    HistoryPage, Input, IntentId, InteractionId, InterruptScope, Seq, SessionFilter, SessionId,
    SessionSelector, SessionState, SessionSummary,
};
use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

/// The wire version a client checks against its own before it speaks.
pub const PROTOCOL: u32 = 1;

/// What `initialize` calls the far side. A client talks to bingo, not to a crate.
pub const SERVER_NAME: &str = "bingo";

/// Every name that travels on the wire, in one place.
pub mod name {
    pub const INITIALIZE: &str = "initialize";
    pub const SHUTDOWN: &str = "shutdown";
    pub const SESSION_LIST: &str = "session/list";
    pub const SESSION_OPEN: &str = "session/open";
    pub const SESSION_CLOSE: &str = "session/close";
    pub const SESSION_DELETE: &str = "session/delete";
    pub const SESSION_HISTORY: &str = "session/history";
    pub const SESSION_EVENTS: &str = "session/events";
    pub const SESSION_SUBMIT: &str = "session/submit";
    pub const SESSION_INTERRUPT: &str = "session/interrupt";
    pub const SESSION_ANSWER: &str = "session/answer";
    pub const CATALOG_READ: &str = "catalog/read";
    pub const GATEWAY_SUBSCRIBE: &str = "gateway/subscribe";

    /// One session frame, verbatim.
    pub const EVENT: &str = "event";
    /// One host-wide event, verbatim.
    pub const GATEWAY_EVENT: &str = "gateway/event";
}

/// A method that answers nothing, and a method that asks for nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Empty {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client: ClientIdentity,
    pub protocol: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol: u32,
    pub name: String,
    pub version: String,
    pub capabilities: Capabilities,
}

impl InitializeResult {
    /// What this build answers with.
    pub fn current() -> Self {
        Self {
            protocol: PROTOCOL,
            name: SERVER_NAME.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            capabilities: Capabilities {
                methods: METHODS.iter().map(|method| method.0.to_owned()).collect(),
                notifications: NOTIFICATIONS
                    .iter()
                    .map(|notification| notification.0.to_owned())
                    .collect(),
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub methods: Vec<String>,
    pub notifications: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListParams {
    #[serde(default)]
    pub filter: SessionFilter,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListResult {
    pub sessions: Vec<SessionSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpenParams {
    pub selector: SessionSelector,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpenResult {
    pub session: SessionId,
    pub snapshot: SessionState,
}

/// `session/close` and `session/delete`: a session and nothing else.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionParams {
    pub session: SessionId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryParams {
    pub session: SessionId,
    #[serde(default)]
    pub page: HistoryPage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EventsParams {
    pub session: SessionId,
    /// Frames after this one are re-sent, then the stream goes live.
    #[serde(default)]
    pub since: Seq,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SubmitParams {
    pub session: SessionId,
    /// Minted by the client; also the idempotency key.
    pub intent: IntentId,
    pub input: Input,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InterruptParams {
    pub session: SessionId,
    pub intent: IntentId,
    pub scope: InterruptScope,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnswerParams {
    pub session: SessionId,
    pub intent: IntentId,
    pub interaction: InteractionId,
    pub answer: Answer,
    pub activation: Activation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogParams {
    pub kind: CatalogKind,
}

/// Names a type in the schema: adds it to `$defs` and answers with its `$ref`.
pub type Ref = fn(&mut SchemaGenerator) -> Schema;

pub fn schema_of<T: JsonSchema>(generator: &mut SchemaGenerator) -> Schema {
    generator.subschema_for::<T>()
}

/// A method: its name, its params, its result.
pub type Method = (&'static str, Ref, Ref);

/// A notification: its name and its params.
pub type Notification = (&'static str, Ref);

pub static METHODS: &[Method] = &[
    (
        name::INITIALIZE,
        schema_of::<InitializeParams>,
        schema_of::<InitializeResult>,
    ),
    (name::SHUTDOWN, schema_of::<Empty>, schema_of::<Empty>),
    (
        name::SESSION_LIST,
        schema_of::<ListParams>,
        schema_of::<ListResult>,
    ),
    (
        name::SESSION_OPEN,
        schema_of::<OpenParams>,
        schema_of::<OpenResult>,
    ),
    (
        name::SESSION_CLOSE,
        schema_of::<SessionParams>,
        schema_of::<Empty>,
    ),
    (
        name::SESSION_DELETE,
        schema_of::<SessionParams>,
        schema_of::<Empty>,
    ),
    (
        name::SESSION_HISTORY,
        schema_of::<HistoryParams>,
        schema_of::<HistoryChunk>,
    ),
    (
        name::SESSION_EVENTS,
        schema_of::<EventsParams>,
        schema_of::<Empty>,
    ),
    (
        name::SESSION_SUBMIT,
        schema_of::<SubmitParams>,
        schema_of::<Empty>,
    ),
    (
        name::SESSION_INTERRUPT,
        schema_of::<InterruptParams>,
        schema_of::<Empty>,
    ),
    (
        name::SESSION_ANSWER,
        schema_of::<AnswerParams>,
        schema_of::<Empty>,
    ),
    (
        name::CATALOG_READ,
        schema_of::<CatalogParams>,
        schema_of::<Catalog>,
    ),
    (
        name::GATEWAY_SUBSCRIBE,
        schema_of::<Empty>,
        schema_of::<Empty>,
    ),
];

pub static NOTIFICATIONS: &[Notification] = &[
    (name::EVENT, schema_of::<Frame>),
    (name::GATEWAY_EVENT, schema_of::<GatewayEvent>),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_wire_has_thirteen_methods_and_two_notifications() {
        assert_eq!(METHODS.len(), 13, "ADR-0007 fixes the method count");
        assert_eq!(NOTIFICATIONS.len(), 2);
    }

    #[test]
    fn no_name_is_used_twice() {
        let mut names: Vec<&str> = METHODS
            .iter()
            .map(|method| method.0)
            .chain(NOTIFICATIONS.iter().map(|notification| notification.0))
            .collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
    }

    #[test]
    fn initialize_reports_the_table_it_dispatches_from() {
        let result = InitializeResult::current();
        assert_eq!(result.protocol, PROTOCOL);
        assert_eq!(result.capabilities.methods.len(), METHODS.len());
        assert!(
            result
                .capabilities
                .notifications
                .contains(&name::EVENT.to_owned())
        );
    }
}

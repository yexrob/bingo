//! The envelope: one JSON-RPC 2.0 message per line (ADR-0007).
//!
//! Nothing here knows what a method does; it knows what a message looks like.

use std::fmt;

use bingo_sdk::{ErrorCode, KernelError};
use serde::de::{self, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};
use tokio_util::codec::LinesCodec;

/// The only version this surface speaks.
const VERSION: &str = "2.0";

/// A frame can carry a base64 image, so the line limit is generous.
const MAX_LINE: usize = 16 * 1024 * 1024;

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
/// Every `KernelError`; its stable code travels in `data.code` (ADR-0007).
pub const KERNEL_ERROR: i64 = -32000;

/// The line framing both ends use.
pub fn lines() -> LinesCodec {
    LinesCodec::new_with_max_length(MAX_LINE)
}

/// `"2.0"`. A message with any other version cannot be built or parsed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Version;

impl Serialize for Version {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(VERSION)
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw == VERSION {
            Ok(Version)
        } else {
            Err(de::Error::invalid_value(Unexpected::Str(&raw), &VERSION))
        }
    }
}

/// A request id: a number or a string, echoed verbatim in the response.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    String(String),
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Id::Number(n) => write!(f, "{n}"),
            Id::String(s) => f.write_str(s),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: Version,
    pub id: Id,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn new(id: Id, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: Version,
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub jsonrpc: Version,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Notification {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: Version,
            method: method.into(),
            params,
        }
    }
}

/// A reply carries a result or an error, never both and never neither.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    Result(Value),
    Error(RpcError),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: Version,
    /// `null` when the line could not be parsed far enough to carry one.
    pub id: Option<Id>,
    #[serde(flatten)]
    pub outcome: Outcome,
}

impl Response {
    pub fn ok(id: Id, result: Value) -> Self {
        Self {
            jsonrpc: Version,
            id: Some(id),
            outcome: Outcome::Result(result),
        }
    }

    pub fn failed(id: Option<Id>, error: RpcError) -> Self {
        Self {
            jsonrpc: Version,
            id,
            outcome: Outcome::Error(error),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// The stable `ErrorCode` travels in `data.code`; the text stays for people.
impl From<KernelError> for RpcError {
    fn from(error: KernelError) -> Self {
        Self {
            code: KERNEL_ERROR,
            message: error.message,
            data: Some(json!({ "code": error.code })),
        }
    }
}

/// The inverse, so a `RemoteKernel` hands its caller the error the kernel raised.
impl From<RpcError> for KernelError {
    fn from(error: RpcError) -> Self {
        let code = error
            .data
            .as_ref()
            .and_then(|data| data.get("code"))
            .and_then(|code| serde_json::from_value(code.clone()).ok())
            .unwrap_or_else(|| protocol_code(error.code));
        KernelError::new(code, error.message)
    }
}

/// A protocol-level error carries no `data.code`; the client sent it, so it is input.
fn protocol_code(code: i64) -> ErrorCode {
    match code {
        INVALID_REQUEST | METHOD_NOT_FOUND | INVALID_PARAMS | PARSE_ERROR => {
            ErrorCode::InvalidInput
        }
        _ => ErrorCode::Internal,
    }
}

/// Any of the three, from one line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(message: &Message) -> String {
        serde_json::to_string(message).expect("a message serialises")
    }

    fn parse(raw: &str) -> Message {
        serde_json::from_str(raw).expect("a well formed message parses")
    }

    #[test]
    fn a_request_round_trips() {
        let request = Message::Request(Request::new(
            Id::Number(7),
            "session/open",
            json!({ "selector": {} }),
        ));
        assert_eq!(
            line(&request),
            r#"{"jsonrpc":"2.0","id":7,"method":"session/open","params":{"selector":{}}}"#
        );
        assert_eq!(parse(&line(&request)), request);
    }

    #[test]
    fn a_string_id_stays_a_string() {
        let request = Message::Request(Request::new(Id::String("a".into()), "shutdown", json!({})));
        assert_eq!(parse(&line(&request)), request);
    }

    #[test]
    fn a_result_and_an_error_are_never_both_present() {
        let ok = Message::Response(Response::ok(Id::Number(1), json!({})));
        assert_eq!(line(&ok), r#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
        let failed = Message::Response(Response::failed(
            None,
            RpcError::new(PARSE_ERROR, "not json"),
        ));
        assert_eq!(
            line(&failed),
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"not json"}}"#
        );
        assert_eq!(parse(&line(&failed)), failed);
    }

    #[test]
    fn a_notification_has_no_id() {
        let event = Message::Notification(Notification::new("event", json!({ "seq": 1 })));
        assert_eq!(
            line(&event),
            r#"{"jsonrpc":"2.0","method":"event","params":{"seq":1}}"#
        );
        assert_eq!(parse(&line(&event)), event);
    }

    #[test]
    fn another_version_is_not_a_message() {
        let wrong = r#"{"jsonrpc":"1.0","id":1,"method":"shutdown","params":{}}"#;
        assert!(serde_json::from_str::<Message>(wrong).is_err());
    }

    #[test]
    fn absent_params_parse_as_null() {
        let Message::Request(request) = parse(r#"{"jsonrpc":"2.0","id":1,"method":"shutdown"}"#)
        else {
            panic!("a request with an id and a method is a request");
        };
        assert_eq!(request.params, Value::Null);
    }

    #[test]
    fn a_kernel_error_keeps_its_code_across_the_wire() {
        let kernel = KernelError::new(ErrorCode::SessionNotFound, "no such session");
        let wire = RpcError::from(kernel.clone());
        assert_eq!(wire.code, KERNEL_ERROR);
        assert_eq!(wire.data, Some(json!({ "code": "SESSION_NOT_FOUND" })));
        assert_eq!(KernelError::from(wire), kernel);
    }

    #[test]
    fn a_protocol_error_comes_back_as_invalid_input() {
        let wire = RpcError::new(METHOD_NOT_FOUND, "no such method: nope");
        assert_eq!(KernelError::from(wire).code, ErrorCode::InvalidInput);
    }
}

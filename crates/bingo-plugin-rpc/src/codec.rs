//! The envelope: one JSON-RPC 2.0 message per line.
//!
//! Nothing here knows what a method does; it knows what a message looks like.
//! The RPC surface has the same loop, deliberately: sharing it would make a
//! plugin import a plugin (ADR-0015 §Consequences), and two copies of a codec
//! are mechanism, not a second representation of a fact.

use serde::de::{self, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

/// The only version this bridge speaks.
const VERSION: &str = "2.0";

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
/// This bridge's own: the process ended, or never answered.
pub const TRANSPORT_ERROR: i64 = -32010;

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

/// A request id. The host mints every one of them and they are numbers;
/// JSON-RPC has the answer echo the value, so anything else is not this id.
pub type Id = i64;

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

/// Any of the three, from one line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Request(Request),
    Response(Response),
    Notification(Notification),
}

impl Message {
    /// The line as it goes on the wire, newline excluded.
    pub fn line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(message: &Message) -> String {
        message.line().expect("a message serialises")
    }

    fn parse(raw: &str) -> Message {
        serde_json::from_str(raw).expect("a well formed message parses")
    }

    #[test]
    fn a_request_round_trips() {
        let request = Message::Request(Request::new(
            1,
            "tool/call",
            json!({ "callId": "call_1", "name": "count" }),
        ));
        assert_eq!(
            line(&request),
            r#"{"jsonrpc":"2.0","id":1,"method":"tool/call","params":{"callId":"call_1","name":"count"}}"#
        );
        assert_eq!(parse(&line(&request)), request);
    }

    #[test]
    fn a_result_and_an_error_are_never_both_present() {
        let ok = Message::Response(Response::ok(1, json!({ "output": {} })));
        assert_eq!(
            line(&ok),
            r#"{"jsonrpc":"2.0","id":1,"result":{"output":{}}}"#
        );
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
        let progress = Message::Notification(Notification::new(
            "tool/progress",
            json!({ "callId": "call_1", "tail": "counting" }),
        ));
        assert_eq!(
            line(&progress),
            r#"{"jsonrpc":"2.0","method":"tool/progress","params":{"callId":"call_1","tail":"counting"}}"#
        );
        assert_eq!(parse(&line(&progress)), progress);
    }

    #[test]
    fn another_version_is_not_a_message() {
        let wrong = r#"{"jsonrpc":"1.0","id":1,"method":"initialize","params":{}}"#;
        assert!(serde_json::from_str::<Message>(wrong).is_err());
    }

    #[test]
    fn absent_params_parse_as_null() {
        let Message::Request(request) = parse(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#)
        else {
            panic!("a request with an id and a method is a request");
        };
        assert_eq!(request.params, Value::Null);
    }
}

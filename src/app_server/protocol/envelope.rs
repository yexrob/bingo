//! JSON-RPC 2.0 envelopes, framed as one JSON object per line.
//!
//! Stdout carries protocol frames only; diagnostics go to stderr. A malformed
//! but bounded line is a parse error and mutates nothing; an unframeable one
//! closes the transport.

use schemars::JsonSchema;
use schemars::r#gen::SchemaGenerator;
use schemars::schema::{InstanceType, Schema, SchemaObject};
use serde::de::{Error as DeError, Unexpected};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::app_server::protocol::error::RpcError;
use crate::app_server::protocol::notifications::ServerNotification;
use crate::app_server::protocol::requests::{ClientNotification, ClientRequest, ResponseResult};

pub const JSONRPC_VERSION: &str = "2.0";

/// The negotiated major version. Breaking semantics require a new one.
pub const PROTOCOL_MAJOR: u32 = 1;
/// The highest minor this build speaks. New methods, notifications, required
/// fields, and union variants arrive by minor; unknown object fields are always
/// additive and ignored.
pub const PROTOCOL_MINOR: u32 = 0;

/// One client line may not exceed this. Retained from the previous protocol
/// rather than re-derived.
pub const MAX_CLIENT_FRAME_BYTES: usize = 1024 * 1024;
/// One server line may not exceed this. Images, full command output, and other
/// large artifacts are referenced by ID and read in chunks below it.
pub const MAX_SERVER_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// The `"jsonrpc": "2.0"` literal, typed so a frame cannot quietly claim another
/// version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JsonRpcVersion;

impl Serialize for JsonRpcVersion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(JSONRPC_VERSION)
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        if raw == JSONRPC_VERSION {
            Ok(Self)
        } else {
            Err(D::Error::invalid_value(
                Unexpected::Str(&raw),
                &JSONRPC_VERSION,
            ))
        }
    }
}

impl JsonSchema for JsonRpcVersion {
    fn schema_name() -> String {
        "JsonRpcVersion".to_string()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        SchemaObject {
            instance_type: Some(InstanceType::String.into()),
            const_value: Some(serde_json::Value::String(JSONRPC_VERSION.to_string())),
            ..Default::default()
        }
        .into()
    }
}

/// Correlates one wire request with its reply. It correlates transport calls
/// only: application resources are named by their own server-minted IDs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    Text(String),
}

impl From<i64> for RequestId {
    fn from(value: i64) -> Self {
        Self::Number(value)
    }
}

impl From<&str> for RequestId {
    fn from(value: &str) -> Self {
        Self::Text(value.to_string())
    }
}

/// A client request frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RequestFrame {
    pub jsonrpc: JsonRpcVersion,
    pub id: RequestId,
    #[serde(flatten)]
    pub call: ClientRequest,
}

impl RequestFrame {
    pub fn new(id: impl Into<RequestId>, call: ClientRequest) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id: id.into(),
            call,
        }
    }
}

/// A client notification frame. `initialized` is the only one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClientNotificationFrame {
    pub jsonrpc: JsonRpcVersion,
    #[serde(flatten)]
    pub notification: ClientNotification,
}

impl ClientNotificationFrame {
    pub fn new(notification: ClientNotification) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            notification,
        }
    }
}

/// A server notification frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NotificationFrame {
    pub jsonrpc: JsonRpcVersion,
    #[serde(flatten)]
    pub notification: ServerNotification,
}

impl NotificationFrame {
    pub fn new(notification: ServerNotification) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            notification,
        }
    }
}

/// A server response frame: exactly one of `result` or `error`, which the two
/// constructors are the only way to build.
///
/// It serializes; it does not deserialize. A result's shape is decided by the
/// method that was called, which the frame does not repeat — a client decodes it
/// with [`ResponseResult::from_value`] against the request it sent, and so does
/// the fixture suite.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct ResponseFrame {
    pub jsonrpc: JsonRpcVersion,
    pub id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<ResponseResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl ResponseFrame {
    pub fn result(id: impl Into<RequestId>, result: ResponseResult) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id: id.into(),
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: impl Into<RequestId>, error: RpcError) -> Self {
        Self {
            jsonrpc: JsonRpcVersion,
            id: id.into(),
            result: None,
            error: Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_literal_is_the_only_accepted_one() {
        let value = serde_json::to_value(JsonRpcVersion).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(value, serde_json::json!("2.0"));
        assert!(serde_json::from_value::<JsonRpcVersion>(serde_json::json!("2.0")).is_ok());
        assert!(serde_json::from_value::<JsonRpcVersion>(serde_json::json!("1.0")).is_err());
        assert!(serde_json::from_value::<JsonRpcVersion>(serde_json::json!(2.0)).is_err());
    }

    #[test]
    fn a_request_id_is_a_number_or_a_string() {
        let number: RequestId =
            serde_json::from_value(serde_json::json!(7)).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(number, RequestId::Number(7));
        let text: RequestId = serde_json::from_value(serde_json::json!("a"))
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(text, RequestId::Text("a".to_string()));
        assert_eq!(
            serde_json::to_value(&text).unwrap_or_else(|error| panic!("{error}")),
            serde_json::json!("a")
        );
    }

    #[test]
    fn frame_ceilings_are_the_ones_the_previous_protocol_used() {
        assert_eq!(MAX_CLIENT_FRAME_BYTES, 1_048_576);
        assert_eq!(MAX_SERVER_FRAME_BYTES, 8_388_608);
    }
}

//! The envelope: one JSON-RPC 2.0 message per line.
//!
//! The types are the schema crate's own (ADR-0035 §2), so nothing here
//! re-spells a field ACP already names. What this module adds is the one thing
//! the schema does not carry: the untagged sum a reader needs, because a line
//! arriving from a child may be any of the three and the reader learns which
//! only by looking.
//!
//! `bingo-plugin-rpc`'s codec is the same animal, host-side. Sharing it would
//! make a plugin import a plugin (ADR-0001), and two codecs are mechanism, not
//! a second representation of a fact.

use agent_client_protocol_schema::rpc::{JsonRpcMessage, Notification, Request, RequestId};
use agent_client_protocol_schema::v1::Error;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A reply carries a result or an error, never both and never neither.
pub type Reply = agent_client_protocol_schema::rpc::Response<Value, Error>;

/// One line, before the method string has been read. `params` stays `Value`:
/// the method table decides what it is, and a line for a method we do not
/// answer must still parse far enough to be refused by id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Body {
    /// Ordered first: only a request carries both an id and a method.
    Request(Request<Value>),
    Reply(Reply),
    Notification(Notification<Value>),
}

/// A `Body` with the `"jsonrpc": "2.0"` the specification requires. A line
/// without it is not a message, in either direction.
pub type Envelope = JsonRpcMessage<Body>;

/// The line as it goes on the wire, newline excluded.
pub fn line(body: Body) -> Result<String, serde_json::Error> {
    serde_json::to_string(&JsonRpcMessage::wrap(body))
}

pub fn request(id: RequestId, method: &str, params: Value) -> Body {
    Body::Request(Request {
        id,
        method: method.into(),
        params: Some(params),
    })
}

pub fn notification(method: &str, params: Value) -> Body {
    Body::Notification(Notification {
        method: method.into(),
        params: Some(params),
    })
}

pub fn result(id: RequestId, result: Value) -> Body {
    Body::Reply(Reply::Result { id, result })
}

pub fn failed(id: RequestId, error: Error) -> Body {
    Body::Reply(Reply::Error { id, error })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(raw: &str) -> Body {
        serde_json::from_str::<Envelope>(raw)
            .expect("a well formed message parses")
            .into_inner()
    }

    fn wire(body: Body) -> String {
        line(body).expect("a message serialises")
    }

    #[test]
    fn a_request_round_trips() {
        let body = request(RequestId::Number(1), "session/prompt", json!({ "a": 1 }));
        assert_eq!(
            wire(body.clone()),
            r#"{"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{"a":1}}"#
        );
        assert_eq!(parse(&wire(body.clone())), body);
    }

    /// The agent mints the ids of the requests it sends us, and JSON-RPC lets
    /// those be strings. Echoing back a number we invented would answer
    /// nobody.
    #[test]
    fn an_agents_string_id_survives_the_round_trip() {
        let body = parse(
            r#"{"jsonrpc":"2.0","id":"perm-1","method":"session/request_permission","params":{}}"#,
        );
        let Body::Request(asked) = &body else {
            panic!("a line with an id and a method is a request");
        };
        assert_eq!(asked.id, RequestId::Str("perm-1".into()));
        assert_eq!(
            wire(result(asked.id.clone(), json!({ "outcome": {} }))),
            r#"{"jsonrpc":"2.0","id":"perm-1","result":{"outcome":{}}}"#
        );
    }

    #[test]
    fn a_notification_has_no_id() {
        let body = notification("session/cancel", json!({ "sessionId": "s1" }));
        assert_eq!(
            wire(body.clone()),
            r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s1"}}"#
        );
        assert_eq!(
            parse(&wire(body)),
            notification("session/cancel", json!({ "sessionId": "s1" }))
        );
    }

    #[test]
    fn a_result_and_an_error_are_never_both_present() {
        let ok = result(RequestId::Number(2), json!({ "stopReason": "end_turn" }));
        assert_eq!(
            wire(ok.clone()),
            r#"{"jsonrpc":"2.0","id":2,"result":{"stopReason":"end_turn"}}"#
        );
        assert_eq!(
            parse(&wire(ok)),
            result(RequestId::Number(2), json!({ "stopReason": "end_turn" }))
        );
        let bad = failed(RequestId::Number(3), Error::method_not_found());
        let Body::Reply(Reply::Error { error, .. }) = parse(&wire(bad)) else {
            panic!("an error reply parses as one");
        };
        assert_eq!(error.code, Error::method_not_found().code);
    }

    #[test]
    fn another_version_is_not_a_message() {
        let wrong = r#"{"jsonrpc":"1.0","id":1,"method":"initialize","params":{}}"#;
        assert!(serde_json::from_str::<Envelope>(wrong).is_err());
    }
}

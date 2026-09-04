//! What the page posted back.
//!
//! Two shapes, because the script writes two: a value, or nothing. Anything
//! else is refused rather than guessed at — a page that posts an envelope this
//! does not know is a page whose author should hear about it.

use serde_json::Value;

use crate::error::LoopbackError;

/// The field `window.bingo.submit` fills in.
const VALUE: &str = "value";
/// The field `window.bingo.cancel` sets.
const CANCELLED: &str = "cancelled";

#[derive(Clone, Debug, PartialEq)]
pub enum Answer {
    /// `window.bingo.submit(value)` — the value, whatever JSON it is.
    Submitted(Value),
    /// `window.bingo.cancel()` — the person read the page and chose nothing.
    Cancelled,
}

/// `{"value": …}` or `{"cancelled": true}`, and nothing else.
pub fn parse(body: &[u8]) -> Result<Answer, LoopbackError> {
    let posted: Value = serde_json::from_slice(body)
        .map_err(|e| LoopbackError::Answer(format!("it is not JSON: {e}")))?;
    if posted.get(CANCELLED).and_then(Value::as_bool) == Some(true) {
        return Ok(Answer::Cancelled);
    }
    posted
        .get(VALUE)
        .cloned()
        .map(Answer::Submitted)
        .ok_or_else(|| {
            LoopbackError::Answer(format!("it carries neither {VALUE:?} nor {CANCELLED:?}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn answered(body: &str) -> Result<Answer, LoopbackError> {
        parse(body.as_bytes())
    }

    #[test]
    fn a_submitted_value_comes_back_as_the_json_it_was() {
        assert_eq!(
            answered(r#"{"value":{"layout":"three columns","rows":2}}"#).expect("an answer"),
            Answer::Submitted(json!({ "layout": "three columns", "rows": 2 }))
        );
    }

    /// A page may submit any JSON, `null` and a bare string included: what the
    /// model asked for is the model's business.
    #[test]
    fn any_json_is_a_value() {
        for (body, value) in [
            (r#"{"value":null}"#, json!(null)),
            (r#"{"value":"b"}"#, json!("b")),
            (r#"{"value":[1,2]}"#, json!([1, 2])),
            (r#"{"value":false}"#, json!(false)),
        ] {
            assert_eq!(
                answered(body).expect("an answer"),
                Answer::Submitted(value),
                "{body}"
            );
        }
    }

    #[test]
    fn a_cancel_is_a_cancel_whatever_else_it_carries() {
        assert_eq!(
            answered(r#"{"cancelled":true}"#).expect("a cancel"),
            Answer::Cancelled
        );
        assert_eq!(
            answered(r#"{"cancelled":true,"value":1}"#).expect("a cancel"),
            Answer::Cancelled,
            "a page that cancels has not submitted"
        );
    }

    #[test]
    fn an_envelope_this_does_not_know_is_refused_by_name() {
        for body in [
            "",
            "not json",
            "{}",
            "[]",
            r#"{"cancelled":false}"#,
            r#"{"answer":1}"#,
            r#""value""#,
        ] {
            assert!(
                matches!(answered(body), Err(LoopbackError::Answer(_))),
                "{body:?} is not an answer"
            );
        }
    }
}

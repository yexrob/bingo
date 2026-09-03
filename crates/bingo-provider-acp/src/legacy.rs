//! The model door codex-acp had before config options (ADR-0037 §2).
//!
//! `session/set_model` is nobody's spec — it is one adapter's extension — so
//! its two fields are written here rather than imported, and its ids pair a
//! model with an effort in one string (`gpt-5[high]`). It is the fallback and
//! never the first choice: an agent that declares a model option is turned
//! through that, and an agent that has neither is left where it is.
//!
//! The list rides the same answer, under `models`, beside the fields the
//! schema knows. Nothing is invented for it: what is not there is nothing.

use agent_client_protocol_schema::v1::SessionId;
use bingo_sdk::ModelInfo;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetModelRequest {
    pub session_id: SessionId,
    pub model_id: String,
}

impl SetModelRequest {
    pub fn new(session: &str, model: &str) -> Self {
        Self {
            session_id: SessionId::new(session),
            model_id: model.to_string(),
        }
    }
}

/// codex-acp answers `{}`, and there is nothing in it to read.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SetModelResponse {}

/// The models a session-opening answer carries beside the fields the schema
/// knows. An answer with none of this is an agent that never had the door.
pub fn models(body: &Value) -> Vec<ModelInfo> {
    let Some(listed) = body["models"]["availableModels"].as_array() else {
        return Vec::new();
    };
    listed.iter().filter_map(model).collect()
}

fn model(row: &Value) -> Option<ModelInfo> {
    let id = row["modelId"].as_str()?;
    Some(ModelInfo {
        id: id.to_string(),
        display: row["name"]
            .as_str()
            .filter(|name| *name != id)
            .map(str::to_string),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The shape codex-acp answers `session/new` with when it is serving the
    /// legacy state: the ids are `model[effort]`, which is what the door takes.
    fn opened() -> Value {
        json!({
            "sessionId": "sess-1",
            "models": {
                "currentModelId": "gpt-5.4-codex[medium]",
                "availableModels": [
                    { "modelId": "gpt-5.4-codex[medium]", "name": "GPT-5.4 Codex (medium)",
                      "description": "Balanced" },
                    { "modelId": "gpt-5.4-codex[high]", "name": "GPT-5.4 Codex (high)" },
                    { "modelId": "plain" }
                ]
            }
        })
    }

    #[test]
    fn the_legacy_list_is_read_off_the_answer_that_carried_it() {
        assert_eq!(
            models(&opened()),
            vec![
                ModelInfo {
                    id: "gpt-5.4-codex[medium]".into(),
                    display: Some("GPT-5.4 Codex (medium)".into()),
                },
                ModelInfo {
                    id: "gpt-5.4-codex[high]".into(),
                    display: Some("GPT-5.4 Codex (high)".into()),
                },
                ModelInfo {
                    id: "plain".into(),
                    display: None
                },
            ]
        );
    }

    /// An agent that never had the door said nothing about it, and a row
    /// without an id is not a model.
    #[test]
    fn an_answer_without_the_legacy_state_lists_nothing() {
        assert!(models(&json!({ "sessionId": "s" })).is_empty());
        assert!(models(&json!({ "models": { "availableModels": [{ "name": "x" }] } })).is_empty());
    }

    /// The body a real adapter parses, byte for byte.
    #[test]
    fn the_request_writes_the_two_fields_the_adapter_reads() {
        assert_eq!(
            serde_json::to_value(SetModelRequest::new("sess-1", "gpt-5[high]"))
                .expect("it serialises"),
            json!({ "sessionId": "sess-1", "modelId": "gpt-5[high]" })
        );
    }
}

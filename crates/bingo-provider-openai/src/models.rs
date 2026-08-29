//! The catalogue the endpoint advertises. What each model can do is the
//! kernel's to resolve (ADR-0004); this only reads the list.

use bingo_sdk::ModelInfo;
use serde_json::Value;

/// `GET /v1/models` → the catalogue, sorted by id. `data[]` is the documented
/// envelope; `models[]` and a bare array — of objects or of plain id strings —
/// are what OpenAI-shaped proxies answer instead (old
/// `providers/openai.rs:372`).
pub fn parse(body: &Value) -> Vec<ModelInfo> {
    let Some(entries) = entries(body) else {
        return Vec::new();
    };
    let mut models: Vec<ModelInfo> = entries.iter().filter_map(entry).collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

fn entries(body: &Value) -> Option<&Vec<Value>> {
    body.get("data")
        .or_else(|| body.get("models"))
        .unwrap_or(body)
        .as_array()
}

/// `display` is always `None`: the Responses catalogue carries no human name,
/// and inventing one would be a second representation of the id.
fn entry(entry: &Value) -> Option<ModelInfo> {
    let id = entry
        .get("id")
        .and_then(Value::as_str)
        .or_else(|| entry.as_str())?;
    Some(ModelInfo {
        id: id.to_string(),
        display: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ids(body: &Value) -> Vec<String> {
        parse(body).into_iter().map(|m| m.id).collect()
    }

    #[test]
    fn the_catalogue_is_read_from_data_and_sorted() {
        assert_eq!(
            ids(&json!({
                "object": "list",
                "data": [
                    { "id": "gpt-5.4", "object": "model" },
                    { "id": "gpt-5.1" },
                    { "not_a_model": true },
                ]
            })),
            ["gpt-5.1", "gpt-5.4"]
        );
    }

    #[test]
    fn a_models_envelope_a_bare_array_and_plain_strings_are_all_read() {
        assert_eq!(
            ids(&json!({ "models": [{ "id": "gpt-5.6" }] })),
            ["gpt-5.6"]
        );
        assert_eq!(ids(&json!([{ "id": "o4-mini" }])), ["o4-mini"]);
        assert_eq!(ids(&json!(["gpt-5.5", "gpt-5.4"])), ["gpt-5.4", "gpt-5.5"]);
        assert_eq!(
            ids(&json!({ "data": ["gpt-5.6-sol"] })),
            ["gpt-5.6-sol"],
            "the codex list is a string array inside the envelope"
        );
    }

    #[test]
    fn an_unreadable_body_is_an_empty_catalogue_rather_than_a_failure() {
        assert!(parse(&json!({ "error": "nope" })).is_empty());
        assert!(parse(&json!("gpt-5")).is_empty());
    }

    #[test]
    fn a_listed_model_carries_no_display_name() {
        assert_eq!(
            parse(&json!({ "data": [{ "id": "gpt-5.4" }] }))[0].display,
            None
        );
    }
}

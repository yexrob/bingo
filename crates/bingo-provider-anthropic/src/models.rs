//! The catalogue the endpoint advertises. What each model can do is the
//! kernel's to resolve (ADR-0004); this only reads the list.

use bingo_sdk::ModelInfo;
use serde_json::Value;

/// `GET /v1/models` → the catalogue, sorted by id. `data[]` is the documented
/// envelope; a body that is already an array is taken as it came, because
/// Anthropic-shaped endpoints differ here.
pub fn parse(body: &Value) -> Vec<ModelInfo> {
    let Some(entries) = body.get("data").unwrap_or(body).as_array() else {
        return Vec::new();
    };
    let mut models: Vec<ModelInfo> = entries.iter().filter_map(entry).collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    models
}

fn entry(entry: &Value) -> Option<ModelInfo> {
    let id = entry.get("id").and_then(Value::as_str)?.to_string();
    Some(ModelInfo {
        display: entry
            .get("display_name")
            .and_then(Value::as_str)
            .map(str::to_string),
        id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_catalogue_is_read_from_data_and_sorted() {
        let models = parse(&json!({
            "data": [
                { "id": "claude-sonnet-4-5-20250929", "display_name": "Claude Sonnet 4.5" },
                { "id": "claude-3-5-haiku-20241022" },
                { "not_a_model": true },
            ]
        }));
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["claude-3-5-haiku-20241022", "claude-sonnet-4-5-20250929"]
        );
        assert_eq!(models[0].display, None);
        assert_eq!(models[1].display.as_deref(), Some("Claude Sonnet 4.5"));
    }

    #[test]
    fn a_bare_array_and_an_unreadable_body_are_both_tolerated() {
        assert_eq!(parse(&json!([{ "id": "claude-opus-4-1" }])).len(), 1);
        assert!(parse(&json!({ "error": "nope" })).is_empty());
    }
}

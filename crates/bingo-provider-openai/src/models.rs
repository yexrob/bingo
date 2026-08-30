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

/// The nine models the old project live-tested as usable on a ChatGPT
/// subscription (`providers/openai.rs:115-125`). The dynamic list is
/// authoritative; this is what the `/model` menu falls back to, because a
/// catalogue that cannot be read must not take the menu down with it.
pub const CODEX_FALLBACK: [&str; 9] = [
    "gpt-5.6-sol",
    "gpt-5.6-sol-wm",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    "gpt-5.4",
    "gpt-5.4-mini",
    "gpt-5.3-codex-spark",
    "codex-auto-review",
];

/// `GET /codex/models` → the subscription's own catalogue (ADR-0012 §6).
/// A different envelope from `/v1/models`: a slug rather than an id, a human
/// name, and the endpoint's own ordering, which the menu keeps. What the
/// endpoint hides stays hidden.
pub fn codex(body: &Value) -> Vec<ModelInfo> {
    let Some(entries) = body.get("models").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut listed: Vec<(i64, ModelInfo)> = entries.iter().filter_map(codex_entry).collect();
    // The slug breaks a tie, so two models of equal priority keep one order.
    listed.sort_by(|(left, a), (right, b)| left.cmp(right).then_with(|| a.id.cmp(&b.id)));
    listed.into_iter().map(|(_, model)| model).collect()
}

pub fn codex_fallback() -> Vec<ModelInfo> {
    CODEX_FALLBACK
        .iter()
        .map(|id| ModelInfo {
            id: (*id).to_string(),
            display: None,
        })
        .collect()
}

/// An entry with no priority sorts last rather than first: an endpoint that
/// forgot to rank a model did not mean to promote it.
fn codex_entry(entry: &Value) -> Option<(i64, ModelInfo)> {
    if entry.get("visibility").and_then(Value::as_str) == Some("hide") {
        return None;
    }
    let slug = entry.get("slug").and_then(Value::as_str)?;
    Some((
        entry
            .get("priority")
            .and_then(Value::as_i64)
            .unwrap_or(i64::MAX),
        ModelInfo {
            id: slug.to_string(),
            display: entry
                .get("display_name")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
    ))
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

    /// `fixtures/codex_models.json` is written to the shape codex-rs
    /// documents, not recorded from a live subscription: no ChatGPT account
    /// exists in this workspace, so the field names are the contract under
    /// test and the values are invented.
    #[test]
    fn the_codex_catalogue_is_ordered_by_priority_and_hides_what_it_is_told_to() {
        let body: Value = serde_json::from_str(
            &std::fs::read_to_string(crate::tests::fixture("codex_models.json"))
                .expect("the fixture"),
        )
        .expect("json");
        let models = codex(&body);
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            [
                "gpt-5.6-sol",
                "gpt-5.5",
                "gpt-5.4",
                "gpt-5.4-mini",
                "gpt-5.3-codex-spark",
            ],
            "priority ascending, the slug breaking a tie, and no hidden model"
        );
        assert_eq!(models[0].display.as_deref(), Some("GPT-5.6 Sol"));
    }

    #[test]
    fn a_codex_body_that_is_not_a_catalogue_is_empty_rather_than_a_failure() {
        assert!(codex(&json!({ "error": "nope" })).is_empty());
        assert!(codex(&json!({ "models": "soon" })).is_empty());
        assert!(codex(&json!({ "models": [{ "display_name": "no slug" }] })).is_empty());
    }

    #[test]
    fn a_model_with_no_priority_sorts_last() {
        assert_eq!(
            codex(&json!({ "models": [
                { "slug": "unranked" },
                { "slug": "first", "priority": 1 },
            ]}))
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
            ["first", "unranked"]
        );
    }

    #[test]
    fn the_fallback_is_the_nine_models_the_old_project_proved() {
        let fallback = codex_fallback();
        assert_eq!(fallback.len(), 9);
        assert_eq!(fallback[0].id, "gpt-5.6-sol");
        assert_eq!(fallback[8].id, "codex-auto-review");
        assert!(fallback.iter().all(|model| model.display.is_none()));
    }
}

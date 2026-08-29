//! What a Claude model can do, and the catalogue the endpoint advertises.
//!
//! The table is a stopgap. M2 reads capabilities from the models.dev
//! catalogue (`docs/plans/M1-provider-tools-gate.md`, "Non-goals"), and this
//! table is deleted then rather than maintained alongside it.

use bingo_sdk::{ModelCapabilities, ModelInfo};
use serde_json::Value;

/// Every Claude family published so far shares one window.
const CONTEXT_WINDOW: u64 = 200_000;

/// The Claude 4 families take a 64k output budget.
const MAX_OUTPUT_4: u64 = 64_000;

/// Everything older, and anything unrecognised, takes the conservative 8k.
const MAX_OUTPUT_LEGACY: u64 = 8_192;

/// Id fragments of the families on the larger budget. Matched as substrings,
/// so a dated id (`claude-sonnet-4-5-20250929`) resolves like its family.
const GENERATION_4: &[&str] = &["opus-4", "sonnet-4", "haiku-4-5"];

/// The last legacy family that thinks.
const LEGACY_REASONING: &str = "claude-3-7";

/// A model unknown to the table gets the smaller budget and no reasoning:
/// asking for more than a model has is a 400, so the guess fails closed.
pub fn capabilities(model: &str) -> ModelCapabilities {
    let generation_4 = GENERATION_4.iter().any(|family| model.contains(family));
    ModelCapabilities {
        context_window: CONTEXT_WINDOW,
        max_output: if generation_4 {
            MAX_OUTPUT_4
        } else {
            MAX_OUTPUT_LEGACY
        },
        images: true,
        reasoning: generation_4 || model.contains(LEGACY_REASONING),
        count_tokens: true,
        caching: true,
    }
}

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
        capabilities: Some(capabilities(&id)),
        id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_four_families_take_the_larger_output_budget_and_think() {
        for model in [
            "claude-opus-4-1-20250805",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5-20251001",
        ] {
            let caps = capabilities(model);
            assert_eq!(caps.context_window, CONTEXT_WINDOW, "{model}");
            assert_eq!(caps.max_output, MAX_OUTPUT_4, "{model}");
            assert!(
                caps.reasoning && caps.images && caps.count_tokens && caps.caching,
                "{model}"
            );
        }
    }

    #[test]
    fn the_legacy_families_keep_the_window_and_lose_the_budget() {
        for (model, reasoning) in [
            ("claude-3-7-sonnet-20250219", true),
            ("claude-3-5-sonnet-20241022", false),
            ("claude-3-5-haiku-20241022", false),
        ] {
            let caps = capabilities(model);
            assert_eq!(caps.context_window, CONTEXT_WINDOW, "{model}");
            assert_eq!(caps.max_output, MAX_OUTPUT_LEGACY, "{model}");
            assert_eq!(caps.reasoning, reasoning, "{model}");
        }
    }

    #[test]
    fn an_unknown_model_fails_closed_on_the_budget_and_on_reasoning() {
        let caps = capabilities("claude-something-new");
        assert_eq!(caps.max_output, MAX_OUTPUT_LEGACY);
        assert!(!caps.reasoning);
        assert_eq!(
            capabilities("not-a-claude-at-all").max_output,
            MAX_OUTPUT_LEGACY
        );
    }

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
        assert_eq!(
            models[1].capabilities.as_ref().map(|c| c.max_output),
            Some(MAX_OUTPUT_4)
        );
    }

    #[test]
    fn a_bare_array_and_an_unreadable_body_are_both_tolerated() {
        assert_eq!(parse(&json!([{ "id": "claude-opus-4-1" }])).len(), 1);
        assert!(parse(&json!({ "error": "nope" })).is_empty());
    }
}

//! What the user says about a model, in settings: the `models` kernel key,
//! `"<provider>/<model>": { … }`, every field optional so declaring one never
//! resets another.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields, default)]
pub struct Declared {
    pub context_window: Option<u64>,
    pub max_output: Option<u64>,
    pub reasoning: Option<bool>,
    pub images: Option<bool>,
}

/// The settings key for one model of one provider.
pub fn key(provider: &str, model: &str) -> String {
    format!("{provider}/{model}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_partial_declaration_leaves_the_other_fields_open() {
        let declared: Declared =
            serde_json::from_value(json!({ "contextWindow": 128000 })).expect("readable");
        assert_eq!(declared.context_window, Some(128_000));
        assert_eq!(declared.max_output, None);
        assert_eq!(declared.reasoning, None);
    }

    #[test]
    fn a_misspelled_field_is_refused_rather_than_ignored() {
        assert!(serde_json::from_value::<Declared>(json!({ "contextWindwo": 1 })).is_err());
    }

    #[test]
    fn the_key_joins_provider_and_model() {
        assert_eq!(key("openai", "gpt-5.4"), "openai/gpt-5.4");
    }
}

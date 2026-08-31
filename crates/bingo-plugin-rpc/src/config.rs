//! The one settings key this plugin claims.
//!
//! A bridge plugin's own configuration lives under this host's claim
//! (ADR-0015 §Consequences): `plugins.<name>` is the slice that reaches that
//! process as `initialize.config`, typed by the schema its manifest declares.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// Each plugin's own slice, by plugin name. What is in one is the
    /// plugin's business, not this host's.
    pub plugins: BTreeMap<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_person_with_no_plugins_key_has_no_slices() {
        let settings: Settings = serde_json::from_value(json!({})).expect("settings");
        assert!(settings.plugins.is_empty());
    }

    #[test]
    fn a_slice_reaches_the_plugin_it_is_named_for() {
        let settings: Settings =
            serde_json::from_value(json!({ "plugins": { "wordcount": { "minimum": 3 } } }))
                .expect("settings");
        assert_eq!(settings.plugins["wordcount"], json!({ "minimum": 3 }));
    }
}

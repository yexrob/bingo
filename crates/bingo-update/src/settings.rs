//! `update.check`: the one setting this crate owns.
//!
//! It is claimed by the surface that says what the check found, so a person
//! who turns it off is not told about an unknown key; the binary reads the
//! answer out of the settings layers, as it does for every other plugin whose
//! key decides something before a host exists.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// The top-level key.
pub const SETTING: &str = "update";

/// The claimed slice, as the kernel hands it over.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub update: Update,
}

/// A typo here would leave a check running that a person asked to stop, so an
/// unknown key is a startup failure rather than a silence.
#[derive(Clone, Copy, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Update {
    /// Whether a start asks whether a newer release is out. On unless it is
    /// turned off: the check is once a day, off the start's own thread, and
    /// silent whenever it cannot answer.
    #[serde(default = "on")]
    pub check: bool,
}

impl Default for Update {
    fn default() -> Self {
        Self { check: on() }
    }
}

fn on() -> bool {
    true
}

pub fn schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

/// Whether a run may ask. Absent is yes; only `update.check: false` is a no.
pub fn wanted(settings: &Value) -> bool {
    serde_json::from_value::<Settings>(settings.clone())
        .map(|settings| settings.update.check)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_check_is_on_until_it_is_turned_off() {
        assert!(wanted(&json!({})));
        assert!(wanted(&json!({ "update": {} })));
        assert!(wanted(&json!({ "update": { "check": true } })));
        assert!(!wanted(&json!({ "update": { "check": false } })));
    }

    #[test]
    fn a_slice_that_is_not_this_shape_leaves_the_check_where_it_was() {
        assert!(wanted(&json!({ "update": { "check": "no" } })));
        assert!(wanted(&json!({ "update": 3 })));
    }

    #[test]
    fn a_neighbouring_key_is_not_this_ones_business() {
        assert!(!wanted(
            &json!({ "model": "gpt-5", "update": { "check": false } })
        ));
    }

    /// The kernel validates a claimed slice against this schema, so a typo
    /// inside `update` is a startup failure and never a silent default.
    #[test]
    fn an_unknown_key_inside_the_slice_is_refused() {
        let refused = serde_json::from_value::<Settings>(json!({"update": {"chek": false}}));
        assert!(refused.is_err(), "a typo is not a setting");
        assert!(
            schema().as_value().is_object(),
            "the claim carries a schema"
        );
    }
}

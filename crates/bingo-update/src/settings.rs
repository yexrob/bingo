//! `update.check`: the one setting this crate owns.
//!
//! It is claimed by the surface that says what the check found, so a person
//! who turns it off is not told about an unknown key; the binary reads the
//! answer out of the settings layers and hands it to that surface with the
//! rest of its arguments, as it does for `demoUi` — every key that decides
//! something before a host exists is read there.

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

/// `update` is claimed by the surface that says what the check found, so a
/// misspelling of *it* is reported as an unknown setting like any other. A
/// misspelling of what is inside it leaves the check where it was — on —
/// which is the safe way for this one setting to be wrong.
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

    /// A misspelling inside the slice is not a setting, and leaves the check
    /// on rather than half-reading the object it is in.
    #[test]
    fn an_unknown_key_inside_the_slice_is_not_a_setting() {
        let typo = json!({"update": {"chek": false}});
        assert!(
            serde_json::from_value::<Settings>(typo.clone()).is_err(),
            "a typo is not a setting"
        );
        assert!(wanted(&typo), "and the check stays on");
        assert!(
            schema().as_value().is_object(),
            "the claim carries a schema"
        );
    }
}

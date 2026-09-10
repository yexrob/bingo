//! `tui.measure`: the one setting this surface owns, beside the
//! `update.check` it claims for the box that says what the check found
//! (ADR-0043 §4).
//!
//! The transcript fills the terminal. A person who reads a narrower line says
//! so here and gets `min(width, that)` — the number is theirs, and this crate
//! names none of its own (design §7, 2026-09-10).
//!
//! One claim carries one schema (`ConfigClaim`), so both keys are described by
//! one type: the `update` half is `bingo_update`'s own, spelled here only for
//! the field the schema hangs it on. The binary reads both off the settings
//! layers — every key that decides something before a host exists is read
//! there (ADR-0003 §2) — and hands the answers to this surface with the rest
//! of its arguments.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// The top-level key. Spelled like the surface it belongs to and kept apart
/// from [`crate::SURFACE_ID`]: what a person writes in a settings file does
/// not move when an id does.
pub const SETTING: &str = "tui";

/// The claimed slice, as the kernel hands it over: both keys the manifest
/// names.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub update: bingo_update::Update,
    #[serde(default)]
    pub tui: Tui,
}

/// `tui` is claimed by the surface it is named after, so a misspelling of *it*
/// is reported as an unknown setting like any other. A misspelling of what is
/// inside it leaves the transcript where it was — filling the terminal —
/// which is the safe way for this one setting to be wrong.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tui {
    /// The widest a line of prose is drawn, however wide the terminal is.
    /// Absent is the terminal's own width, and so is `0`: a person who wants
    /// their measure back is writing a number, not a `null`.
    #[serde(default)]
    pub measure: Option<u32>,
}

pub fn schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

/// The measure a person set, out of one layer of the settings. Nothing at all
/// is the region's own width.
pub fn measure(settings: &Value) -> Option<usize> {
    serde_json::from_value::<Settings>(settings.clone())
        .ok()
        .and_then(|settings| settings.tui.measure)
        .filter(|measure| *measure > 0)
        .and_then(|measure| usize::try_from(measure).ok())
}

/// The measure the bin gave this run, out of its own arguments — beside
/// `updateCheck`, and read once at the start as that is. A harness that builds
/// its own options says nothing and draws the width it has.
pub(crate) fn given(args: &Value) -> Option<usize> {
    args.get("measure")
        .and_then(Value::as_u64)
        .and_then(|measure| usize::try_from(measure).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prose_has_the_width_it_is_drawn_in_until_a_person_names_one() {
        assert_eq!(measure(&json!({})), None);
        assert_eq!(measure(&json!({ "tui": {} })), None);
        assert_eq!(measure(&json!({ "tui": { "measure": null } })), None);
        assert_eq!(
            measure(&json!({ "tui": { "measure": 0 } })),
            None,
            "no measure at all, said as a number"
        );
        assert_eq!(measure(&json!({ "tui": { "measure": 100 } })), Some(100));
    }

    #[test]
    fn a_slice_that_is_not_this_shape_leaves_the_transcript_where_it_was() {
        assert_eq!(measure(&json!({ "tui": { "measure": "wide" } })), None);
        assert_eq!(measure(&json!({ "tui": 3 })), None);
    }

    #[test]
    fn a_neighbouring_key_is_not_this_ones_business() {
        assert_eq!(
            measure(&json!({ "model": "gpt-5", "tui": { "measure": 72 } })),
            Some(72)
        );
    }

    /// A misspelling inside either slice is not a setting, and leaves the
    /// surface as it was rather than half-reading the object it is in.
    #[test]
    fn an_unknown_key_inside_a_claimed_slice_is_not_a_setting() {
        let typo = json!({ "tui": { "mesure": 100 } });
        assert!(
            serde_json::from_value::<Settings>(typo.clone()).is_err(),
            "a typo is not a setting"
        );
        assert_eq!(measure(&typo), None, "and the terminal keeps its width");
        assert!(
            serde_json::from_value::<Settings>(json!({ "update": { "chek": false } })).is_err(),
            "the update half is read as strictly through this type as its own"
        );
    }

    #[test]
    fn what_the_bin_handed_over_is_what_the_run_draws_at() {
        assert_eq!(given(&json!({ "measure": 100 })), Some(100));
        assert_eq!(given(&json!({ "measure": null })), None);
        assert_eq!(given(&json!({})), None, "a harness fills its terminal");
        assert_eq!(given(&Value::Null), None);
    }

    /// One claim, one schema: it describes both keys the manifest names, or a
    /// person who sets one of them is told it is unknown.
    #[test]
    fn the_claim_carries_a_schema_for_both_keys() {
        let schema = schema();
        let properties = schema
            .as_value()
            .get("properties")
            .expect("a schema of properties");
        assert!(properties.get("tui").is_some(), "{properties}");
        assert!(properties.get("update").is_some(), "{properties}");
    }
}

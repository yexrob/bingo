//! What this plugin says about itself: the page the model reads as the
//! `guide-persona` skill (ADR-0054 §1).
//!
//! Its test is next to the words it describes, so a stance rewritten in
//! [`crate::judgement::TEXT`] without a sentence here fails in this crate —
//! and so does a field added under `persona` that the page never names.

use std::sync::Arc;

use bingo_sdk::{Contribution, Page, Pages, Registrar};

pub static PAGES: Pages = Pages(&[Page {
    name: "persona",
    description: "The stance bingo takes when it disagrees: what the shipped \
                  block says, where it sits among the system blocks, how \
                  `persona.text` replaces it or silences it, and the switch \
                  that turns the plugin off.",
    body: include_str!("guide.md"),
}]);

/// The page under the key every plugin registers its pages by.
pub fn contribution(registrar: &Registrar) -> Contribution {
    Contribution::Service {
        key: Pages::key(registrar.plugin_id()),
        value: Arc::new(PAGES),
        // A page is data in this binary; the gathering that reads it is in
        // process (ADR-0031 §3).
        wire: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MANIFEST, SETTING, TEXT};

    fn page() -> &'static Page {
        let [page] = PAGES.0 else {
            panic!("one page");
        };
        page
    }

    #[test]
    fn the_page_names_the_settings_key_this_plugin_claims() {
        let claim = MANIFEST.config.expect("the plugin claims one setting");
        for (key, _) in claim.keys {
            assert!(page().body.contains(key), "the page never says {key}");
        }
    }

    /// Every field a person may write under the key, read off the schema so a
    /// field added without a sentence fails here rather than going unsaid.
    #[test]
    fn the_page_names_every_field_under_the_key() {
        let schema =
            serde_json::to_value(schemars::schema_for!(crate::Persona)).expect("a schema is json");
        let fields = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
            .expect("`Persona` has fields");
        assert!(!fields.is_empty(), "a field for the page to name");
        for field in fields.keys() {
            assert!(
                page().body.contains(&format!("`{SETTING}.{field}`"))
                    || page().body.contains(&format!("`{field}`")),
                "the page never says the {field} field"
            );
        }
    }

    /// The phrase the binary's black-box run matches the request on, so the
    /// page and the block cannot drift apart without a failure here.
    #[test]
    fn the_page_quotes_the_line_the_stance_is_pinned_by() {
        let line = "Never deviate silently";
        assert!(TEXT.contains(line), "the stance dropped {line:?}");
        assert!(page().body.contains(line), "the page never says {line:?}");
    }

    /// The three readings of the one key and the one switch, each shown the
    /// way a person writes it into a layer.
    #[test]
    fn the_page_shows_the_key_the_silence_and_the_off_switch() {
        for shown in [
            "[persona]",
            "text = \"\"",
            "[enabledPlugins]",
            "\"bingo.persona\" = false",
        ] {
            assert!(page().body.contains(shown), "the page never shows {shown}");
        }
        assert!(
            page().body.contains(MANIFEST.id),
            "the page never names the plugin the switch takes"
        );
    }

    /// The settings layers are TOML (ADR-0058 §1), so every example a person
    /// is invited to paste is written in the format the file is.
    #[test]
    fn every_settings_example_is_toml_and_none_is_json() {
        for fence in ["```json", "```jsonc"] {
            assert!(!page().body.contains(fence), "the page still shows {fence}");
        }
        assert_eq!(
            page().body.matches("```toml").count(),
            3,
            "the three examples the page promises"
        );
    }

    #[test]
    fn the_page_is_one_line_of_description_and_a_body_worth_reading() {
        assert_eq!(page().name, "persona");
        assert!(!page().description.contains('\n'));
        assert!(page().description.chars().count() < 250);
        assert!(page().body.lines().count() < 200);
    }
}

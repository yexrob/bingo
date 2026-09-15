//! What this plugin says about itself: the page the model reads as the
//! `guide-hooks` skill (ADR-0054 §1).
//!
//! Its test is next to the code it describes, so an event served without a
//! sentence written for it fails in this crate.

use std::sync::Arc;

use bingo_sdk::{Contribution, Page, Pages, Registrar};

pub static PAGES: Pages = Pages(&[Page {
    name: "hooks",
    description: "The shell commands a person's settings run at bingo's \
                  lifecycle points: the ten events and what each may answer, \
                  the matcher, the JSON on stdin, the exit codes, the \
                  timeouts, and the variables a session exports.",
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
    use crate::MANIFEST;
    use crate::config::CLAIMED;

    fn page() -> &'static Page {
        let [page] = PAGES.0 else {
            panic!("one page");
        };
        page
    }

    #[test]
    fn the_page_names_every_event_this_plugin_serves() {
        for (key, _) in CLAIMED {
            let event = key.strip_prefix("hooks.").unwrap_or(key);
            assert!(
                page().body.contains(event),
                "the page never says {event}, which is an event a person may \
                 configure"
            );
        }
    }

    #[test]
    fn the_page_names_the_settings_key_this_plugin_claims() {
        let claim = MANIFEST.config.expect("the plugin claims settings");
        assert!(!claim.keys.is_empty());
        assert!(
            page().body.contains("[[hooks."),
            "the page never shows the block the way a settings layer spells it"
        );
    }

    /// The settings layers are TOML (ADR-0058 §1), so the example a person
    /// pastes is written in the format the file is. The page still says that
    /// Claude Code's JSON block drops in unchanged, because a `settings.json`
    /// layer is read as it always was (ADR-0058 §1) — but it is not what the
    /// example shows.
    #[test]
    fn the_settings_example_is_toml_and_no_json_is_left() {
        for fence in ["```json", "```jsonc"] {
            assert!(!page().body.contains(fence), "the page still shows {fence}");
        }
        assert!(
            page().body.contains("```toml"),
            "the example is still shown"
        );
        assert!(
            page().body.contains(".claude/settings.json"),
            "the page stopped saying where a pasted block comes from"
        );
    }

    #[test]
    fn the_page_registers_nothing_else_to_describe() {
        assert_eq!(
            MANIFEST.provides,
            ["hook:shell", "service:bingo.hooks.shell.pages"]
        );
    }

    #[test]
    fn the_page_is_one_line_of_description_and_a_body_worth_reading() {
        assert_eq!(page().name, "hooks");
        assert!(!page().description.contains('\n'));
        assert!(page().description.chars().count() < 250);
        assert!(page().body.lines().count() < 200);
    }
}

//! What this plugin says about itself: the page the model reads as the
//! `guide-permissions` skill (ADR-0054 §1).
//!
//! It is registered as a service under the one key the sdk spells, and its
//! test is next to the code it describes: a mode, a rule form or a settings
//! key that grows here without a sentence there fails in this crate.

use std::sync::Arc;

use bingo_sdk::{Contribution, Page, Pages, Registrar};

pub static PAGES: Pages = Pages(&[Page {
    name: "permissions",
    description: "How a tool call is decided here: the ladder every call \
                  walks, the five permission modes, the allow/deny/ask rule \
                  grammar, what a prompt may offer for the session, and why \
                  an undescribed tool's traits fail closed.",
    body: include_str!("guide.md"),
}]);

/// The page under the key every plugin registers its pages by.
pub fn contribution(registrar: &Registrar) -> Contribution {
    Contribution::Service {
        key: Pages::key(registrar.plugin_id()),
        value: Arc::new(PAGES),
        // A page is data in this binary; nothing across a process line reads
        // it, and the gathering that does is in process (ADR-0031 §3).
        wire: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MANIFEST;
    use crate::mode::Mode;

    fn page() -> &'static Page {
        let [page] = PAGES.0 else {
            panic!("one page");
        };
        page
    }

    #[test]
    fn the_page_names_every_mode_this_plugin_has() {
        for mode in Mode::ALL {
            assert!(
                page().body.contains(mode.as_str()),
                "the page never says {mode}"
            );
            assert!(
                page().body.contains(mode.meaning()),
                "the page says {mode} without saying what it does"
            );
        }
    }

    #[test]
    fn the_page_names_every_settings_key_this_plugin_claims() {
        let claim = MANIFEST.config.expect("the plugin claims settings");
        for (key, _) in claim.keys {
            assert!(page().body.contains(key), "the page never says {key}");
        }
    }

    #[test]
    fn the_page_names_every_command_this_plugin_registers() {
        for provided in MANIFEST.provides {
            let Some(name) = provided.strip_prefix("command:") else {
                continue;
            };
            assert!(
                page().body.contains(&format!("/{name}")),
                "the page never says /{name}"
            );
        }
    }

    #[test]
    fn the_page_is_one_line_of_description_and_a_body_worth_reading() {
        assert_eq!(page().name, "permissions");
        assert!(!page().description.contains('\n'));
        assert!(page().description.chars().count() < 250);
        assert!(page().body.lines().count() < 200);
    }
}

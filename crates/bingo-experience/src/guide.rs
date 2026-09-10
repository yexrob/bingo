//! What this plugin says about itself: the page the model reads as the
//! `guide-experience` skill (ADR-0054 §1).
//!
//! Its test is next to the code it describes, so a tool, a status or an
//! outcome that grows here without a sentence there fails in this crate.

use std::sync::Arc;

use bingo_sdk::{Contribution, Page, Pages, Registrar};

pub static PAGES: Pages = Pages(&[Page {
    name: "experience",
    description: "The playbooks a project teaches the agent: what belongs in \
                  one and what belongs in memory instead, the four \
                  Experience tools, how an entry is ranked back into a turn, \
                  and `/experience`.",
    body: include_str!("guide.md"),
}]);

/// The page under the key every plugin registers its pages by. Registered
/// only where the library is, because a project that keeps no playbooks is
/// offered nothing at all — a page included.
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
    use crate::entry::{Outcome, Status};

    fn page() -> &'static Page {
        let [page] = PAGES.0 else {
            panic!("one page");
        };
        page
    }

    #[test]
    fn the_page_names_every_tool_and_command_this_plugin_registers() {
        for provided in MANIFEST.provides {
            if let Some(name) = provided.strip_prefix("tool:") {
                assert!(
                    page().body.contains(&format!("`{name}`")),
                    "the page never says the {name} tool"
                );
            }
            if let Some(name) = provided.strip_prefix("command:") {
                assert!(
                    page().body.contains(&format!("/{name}")),
                    "the page never says /{name}"
                );
            }
        }
    }

    #[test]
    fn the_page_names_the_setting_that_turns_the_library_off() {
        let claim = MANIFEST.config.expect("the plugin claims settings");
        for (key, _) in claim.keys {
            assert!(page().body.contains(key), "the page never says {key}");
        }
        assert!(
            page().body.contains("\"enabled\""),
            "the page never says the field inside the block"
        );
    }

    /// The two words a model writes into a call, and the two it reads back.
    #[test]
    fn the_page_names_every_status_an_entry_may_have_and_every_outcome() {
        for word in [Status::Active.as_str(), Status::Retired.as_str()] {
            assert!(page().body.contains(word), "the page never says {word}");
        }
        for word in [Outcome::Helpful.as_str(), Outcome::Harmful.as_str()] {
            assert!(
                page().body.contains(&format!("`{word}`")),
                "the page never says {word}, which is an outcome to record"
            );
        }
    }

    #[test]
    fn the_page_is_one_line_of_description_and_a_body_worth_reading() {
        assert_eq!(page().name, "experience");
        assert!(!page().description.contains('\n'));
        assert!(page().description.chars().count() < 250);
        assert!(page().body.lines().count() < 200);
    }
}

//! What this plugin says about itself: the page the model reads as the
//! `guide-memory` skill (ADR-0054 §1).
//!
//! Its test is next to the code it describes, so a memory type, an instruction
//! file name or a command that grows here without a sentence there fails in
//! this crate.

use std::sync::Arc;

use bingo_sdk::{Contribution, Page, Pages, Registrar};

pub static PAGES: Pages = Pages(&[Page {
    name: "memory",
    description: "What the agent knows before anybody types and keeps for \
                  next time: the two memory directories and the one file per \
                  fact in them, `/memory`, the AGENTS.md files a project \
                  leaves, and what compaction does when the window fills.",
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
    use crate::instructions::NAMES;
    use crate::memory::INDEX_LINES;
    use crate::memory::file::Kind;

    fn page() -> &'static Page {
        let [page] = PAGES.0 else {
            panic!("one page");
        };
        page
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

    /// A fifth type would be a fifth directory of files nobody is told to
    /// write, so the four are derived from the names the parser answers to.
    #[test]
    fn the_page_names_every_type_a_memory_may_be() {
        for word in Kind::NAMES.split(" | ") {
            assert!(
                page().body.contains(word),
                "the page never says the {word} type"
            );
        }
    }

    #[test]
    fn the_page_names_both_files_a_directory_may_speak_through() {
        for name in NAMES {
            assert!(page().body.contains(name), "the page never says {name}");
        }
    }

    #[test]
    fn the_page_says_what_an_index_is_called_and_what_it_costs() {
        assert!(page().body.contains("MEMORY.md"), "the index has a name");
        assert!(
            page().body.contains(&INDEX_LINES.to_string()),
            "the page never says how many lines of an index reach the prompt"
        );
    }

    /// ADR-0049: the extractor and its switch are gone, so there is nothing
    /// to configure and the page says so. A key added here without a sentence
    /// there fails on the first line of this test.
    #[test]
    fn this_plugin_claims_no_settings_for_the_page_to_name() {
        assert!(MANIFEST.config.is_none(), "the model is the one writer");
        assert!(page().body.contains("claims no settings key"));
    }

    #[test]
    fn the_page_is_one_line_of_description_and_a_body_worth_reading() {
        assert_eq!(page().name, "memory");
        assert!(!page().description.contains('\n'));
        assert!(page().description.chars().count() < 250);
        assert!(page().body.lines().count() < 200);
    }
}

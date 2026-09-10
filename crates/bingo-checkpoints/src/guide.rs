//! What this plugin says about itself: the page the model reads as the
//! `guide-rewind` skill (ADR-0054 §1).
//!
//! Its test is next to the code it describes, so a tool taught to write a
//! file — a row in [`crate::hook::WRITERS`] — without a sentence saying a
//! rewind now covers it fails in this crate.

use std::sync::Arc;

use bingo_sdk::{Contribution, Page, Pages, Registrar};

pub static PAGES: Pages = Pages(&[Page {
    name: "rewind",
    description: "Going back to an earlier turn: what a checkpoint keeps \
                  before a file is written, what `/rewind` puts back and in \
                  which order, and what — a shell line's work above all — it \
                  never claims to undo.",
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
    use crate::hook::WRITERS;
    use crate::store::MOST;

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

    #[test]
    fn the_page_names_every_tool_whose_writes_are_kept() {
        for (tool, field) in WRITERS {
            assert!(
                page().body.contains(&format!("`{tool}`")),
                "the page never says the {tool} tool, whose writes are undone"
            );
            assert!(
                page().body.contains(&format!("`{field}`")),
                "the page never says the {field} a snapshot is taken from"
            );
        }
    }

    /// The one number a person is told, said the way the reply says it.
    #[test]
    fn the_page_says_how_big_a_file_may_be_and_still_be_kept() {
        assert!(
            page().body.contains(&format!("{} MiB", MOST / 1024 / 1024)),
            "the page never says the size past which nothing is kept"
        );
    }

    #[test]
    fn this_plugin_claims_no_settings_for_the_page_to_name() {
        assert!(
            MANIFEST.config.is_none(),
            "a checkpoint follows the data directory and the turn"
        );
    }

    #[test]
    fn the_page_is_one_line_of_description_and_a_body_worth_reading() {
        assert_eq!(page().name, "rewind");
        assert!(!page().description.contains('\n'));
        assert!(page().description.chars().count() < 250);
        assert!(page().body.lines().count() < 200);
    }
}

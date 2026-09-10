//! What this plugin says about itself: the page the model reads as the
//! `guide-agents` skill (ADR-0054 §1).
//!
//! Its test is next to the code it describes, so a tool, a command or a name
//! a child may not have that grows here without a sentence there fails in
//! this crate.

use std::sync::Arc;

use bingo_sdk::{Contribution, Page, Pages, Registrar};

pub static PAGES: Pages = Pages(&[Page {
    name: "agents",
    description: "Sub-agents as child sessions: how `SpawnAgent` staffs one \
                  and what becomes of its answer, how `SendMessage` reaches \
                  an agent or a room, agent definitions, and the team a \
                  project seats.",
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
    use crate::names::{PARENT, ROOM};
    use crate::spawn::NOT_A_CHILDS;

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

    /// A tool a child is not offered is a tool a parent must not plan around.
    #[test]
    fn the_page_names_every_tool_a_child_never_has() {
        for name in NOT_A_CHILDS {
            assert!(
                page().body.contains(&format!("`{name}`")),
                "the page never says a child has no {name}"
            );
        }
    }

    /// The two addresses that are not a name, and the word no agent may take.
    #[test]
    fn the_page_names_the_addresses_a_message_may_carry() {
        assert!(
            page().body.contains(&format!("`{PARENT}`")),
            "the page never says how a child reaches whoever started it"
        );
        assert!(
            page().body.contains(&format!("`{ROOM}name`")),
            "the page never says how a room is addressed"
        );
    }

    #[test]
    fn the_page_names_every_column_a_roster_shows() {
        for header in crate::list::HEADERS {
            assert!(
                page().body.contains(&format!("`{header}`")),
                "the page never says the {header} column"
            );
        }
    }

    #[test]
    fn this_plugin_claims_no_settings_for_the_page_to_name() {
        assert!(
            MANIFEST.config.is_none(),
            "definitions are files, and a tree's limits are the kernel's"
        );
    }

    #[test]
    fn the_page_is_one_line_of_description_and_a_body_worth_reading() {
        assert_eq!(page().name, "agents");
        assert!(!page().description.contains('\n'));
        assert!(page().description.chars().count() < 250);
        assert!(page().body.lines().count() < 200);
    }
}

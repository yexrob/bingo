//! What this plugin says about itself: the page the model reads as the
//! `guide-tasks` skill (ADR-0054 §1).
//!
//! Its test is next to the code it describes, so a tool, a status or a column
//! that grows here without a sentence there fails in this crate.

use std::sync::Arc;

use bingo_sdk::{Contribution, Page, Pages, Registrar};

pub static PAGES: Pages = Pages(&[Page {
    name: "tasks",
    description: "The list of what is to be done: the four Task tools and \
                  their fields, the three statuses, what reaches the prompt, \
                  `/tasks`, and the shared board a room holds.",
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
    use crate::task::Status;

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

    /// The words a model writes into `TaskUpdate` and reads back in a line.
    #[test]
    fn the_page_names_every_status_a_task_may_have() {
        for status in [Status::Pending, Status::InProgress, Status::Completed] {
            assert!(
                page().body.contains(&format!("`{}`", status.as_str())),
                "the page never says {}",
                status.as_str()
            );
        }
    }

    #[test]
    fn the_page_names_every_column_the_table_shows() {
        for header in crate::render::HEADERS {
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
            "a task list is a session's own state, not a setting"
        );
    }

    #[test]
    fn the_page_is_one_line_of_description_and_a_body_worth_reading() {
        assert_eq!(page().name, "tasks");
        assert!(!page().description.contains('\n'));
        assert!(page().description.chars().count() < 250);
        assert!(page().body.lines().count() < 200);
    }
}

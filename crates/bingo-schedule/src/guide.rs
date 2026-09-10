//! What this plugin says about itself: the page the model reads as the
//! `guide-schedule` skill (ADR-0054 §1).
//!
//! Its test is next to the code it describes, so a tool, a command word or a
//! column that grows here without a sentence there fails in this crate.

use std::sync::Arc;

use bingo_sdk::{Contribution, Page, Pages, Registrar};

pub static PAGES: Pages = Pages(&[Page {
    name: "schedule",
    description: "Work that happens later: schedules that fire a turn on a \
                  session of their own, the wake a turn sets on itself, what \
                  each grammar accepts, and why nothing fires while no bingo \
                  process runs.",
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
    use crate::schedules::Schedules;
    use crate::{MANIFEST, ScheduleCommand, WakeCommand};
    use bingo_sdk::{ArgSpec, Command, CommandSpec};

    fn page() -> &'static Page {
        let [page] = PAGES.0 else {
            panic!("one page");
        };
        page
    }

    /// The two commands, as they describe themselves. Building one reads
    /// nothing: the store is a path until something writes to it.
    fn commands() -> Vec<CommandSpec> {
        let schedules = Arc::new(Schedules::new(std::path::Path::new("/nowhere")));
        vec![
            ScheduleCommand::new(schedules.clone()).spec(),
            WakeCommand::new(schedules).spec(),
        ]
    }

    #[test]
    fn the_page_names_every_tool_this_plugin_registers() {
        for provided in MANIFEST.provides {
            let Some(name) = provided.strip_prefix("tool:") else {
                continue;
            };
            assert!(
                page().body.contains(&format!("`{name}`")),
                "the page never says the {name} tool"
            );
        }
    }

    /// Every command, every alias a person may type instead, and every word
    /// a command offers to complete after it.
    #[test]
    fn the_page_names_every_command_word_a_person_may_type() {
        for spec in commands() {
            for name in std::iter::once(&spec.name).chain(spec.aliases.iter()) {
                assert!(
                    page().body.contains(&format!("/{name}")),
                    "the page never says /{name}"
                );
            }
            if let ArgSpec::Words { values } = &spec.args {
                for word in values {
                    assert!(
                        page().body.contains(&format!("/{} {word}", spec.name)),
                        "the page never says /{} {word}",
                        spec.name
                    );
                }
            }
        }
    }

    #[test]
    fn the_page_names_every_column_a_listing_shows() {
        for header in crate::render::HEADERS {
            assert!(
                page().body.contains(&format!("`{header}`")),
                "the page never says the {header} column"
            );
        }
    }

    #[test]
    fn the_page_names_the_setting_that_turns_wakes_off() {
        let claim = MANIFEST.config.expect("the plugin claims settings");
        for (key, _) in claim.keys {
            assert!(page().body.contains(key), "the page never says {key}");
        }
        assert!(
            page().body.contains("\"wakes\""),
            "the page never says the field inside the block"
        );
    }

    #[test]
    fn the_page_is_one_line_of_description_and_a_body_worth_reading() {
        assert_eq!(page().name, "schedule");
        assert!(!page().description.contains('\n'));
        assert!(page().description.chars().count() < 250);
        assert!(page().body.lines().count() < 200);
    }
}

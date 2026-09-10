//! What this plugin says about itself: the page the model reads as the
//! `guide-rooms` skill (ADR-0054 §1).
//!
//! Its test is next to the code it describes, so a verb, a column or a number
//! that grows here without a sentence there fails in this crate.

use std::sync::Arc;

use bingo_sdk::{Contribution, Page, Pages, Registrar};

pub static PAGES: Pages = Pages(&[Page {
    name: "rooms",
    description: "A conversation every member reads: opening one for a \
                  purpose, who may move its roster, the ear a seat wears, \
                  what an `@name` owes, why every room is serial, and \
                  `/room`.",
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
    use crate::chase::PATIENCE;
    use crate::ear::{FLOOR, PATIENCE_S};
    use crate::name::{CLOSE, PARENT};

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

    /// The two words a person's door spends, and the one no room may take.
    #[test]
    fn the_page_names_the_word_that_ends_a_room_and_the_one_that_is_the_holder() {
        assert!(
            page().body.contains(&format!("/room {CLOSE}")),
            "the page never says how a person ends a room"
        );
        assert!(
            page().body.contains(&format!("`{PARENT}`")),
            "the page never says the name a room calls its holder"
        );
    }

    #[test]
    fn the_page_names_every_column_the_listing_shows() {
        for header in crate::command::HEADERS {
            assert!(
                page().body.contains(&format!("`{header}`")),
                "the page never says the {header} column"
            );
        }
    }

    /// The three numbers a seat is held to, said as the code holds them.
    #[test]
    fn the_page_says_the_patience_a_seat_has_and_the_band_that_is_refused() {
        assert!(page().body.contains(PATIENCE_S), "the key an ear is set by");
        assert!(
            page()
                .body
                .contains(&format!("{} seconds", PATIENCE.as_secs())),
            "the page never says the default patience"
        );
        assert!(
            page()
                .body
                .contains(&format!("{} seconds is refused", FLOOR.as_secs() - 1)),
            "the page never says the band under the floor is refused"
        );
    }

    #[test]
    fn this_plugin_claims_no_settings_for_the_page_to_name() {
        assert!(
            MANIFEST.config.is_none(),
            "who sits where is a project's file, not a person's settings"
        );
    }

    #[test]
    fn the_page_is_one_line_of_description_and_a_body_worth_reading() {
        assert_eq!(page().name, "rooms");
        assert!(!page().description.contains('\n'));
        assert!(page().description.chars().count() < 250);
        assert!(page().body.lines().count() < 200);
    }
}

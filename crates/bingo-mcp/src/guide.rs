//! What this plugin says about itself: the page the model reads as the
//! `guide-mcp` skill (ADR-0054 §1).
//!
//! Its test is next to the code it describes, so a `/mcp` verb or a settings
//! key that grows here without a sentence there fails in this crate.

use std::sync::Arc;

use bingo_sdk::{Contribution, Page, Pages, Registrar};

pub static PAGES: Pages = Pages(&[Page {
    name: "mcp",
    description: "The Model Context Protocol servers a session dials: the \
                  `mcpServers` shapes, how their tools are named and why they \
                  are never trusted, the `/mcp` table and its verbs, and how a \
                  server that wants a sign-in gets one.",
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
    use crate::command::Verb;
    use crate::tool::tool_name;

    fn page() -> &'static Page {
        let [page] = PAGES.0 else {
            panic!("one page");
        };
        page
    }

    #[test]
    fn the_page_names_every_verb_of_the_command() {
        for verb in Verb::ALL {
            assert!(
                page().body.contains(&format!("/mcp {}", verb.as_str())),
                "the page never says /mcp {}",
                verb.as_str()
            );
        }
    }

    #[test]
    fn the_page_names_every_command_and_settings_key_this_plugin_registers() {
        for provided in MANIFEST.provides {
            if let Some(name) = provided.strip_prefix("command:") {
                assert!(
                    page().body.contains(&format!("/{name}")),
                    "the page never says /{name}"
                );
            }
        }
        let claim = MANIFEST.config.expect("the plugin claims settings");
        for (key, _) in claim.keys {
            assert!(page().body.contains(key), "the page never says {key}");
        }
    }

    #[test]
    fn the_page_spells_a_tool_name_the_way_this_crate_mints_one() {
        assert!(
            page().body.contains(&tool_name("<server>", "<tool>")),
            "the page never says how an mcp tool is named"
        );
    }

    #[test]
    fn the_page_is_one_line_of_description_and_a_body_worth_reading() {
        assert_eq!(page().name, "mcp");
        assert!(!page().description.contains('\n'));
        assert!(page().description.chars().count() < 250);
        assert!(page().body.lines().count() < 200);
    }
}

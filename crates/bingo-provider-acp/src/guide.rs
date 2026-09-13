//! What this plugin says about itself: the page the model reads as the
//! `guide-acp` skill (ADR-0054 §1).
//!
//! The page is registered whether or not an adapter is configured: a person
//! asking how to run an agent through here has not configured one yet. Its
//! test reads the row's own fields out of the schema the claim carries, and
//! the tools that never cross out of the list that keeps them back.

use std::sync::Arc;

use bingo_sdk::{Contribution, Page, Pages, Registrar};

pub static PAGES: Pages = Pages(&[Page {
    name: "acp",
    description: "Another coding agent driving as a model: the `acp.adapters` \
                  rows that spawn one, the `agent` model label and how /model \
                  and /think reach it, the tool bridge, and what a session on \
                  one cannot do — `/compact` included.",
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
    use crate::bridge::offer::NOT_THE_AGENTS;
    use crate::config::AGENT;
    use crate::{MANIFEST, schema};
    use serde_json::Value;

    fn page() -> &'static Page {
        let [page] = PAGES.0 else {
            panic!("one page");
        };
        page
    }

    /// A word the page must spell exactly, in backticks: a settings field read
    /// as prose is a field nobody can copy. A dotted key spells both of its
    /// halves, so `acp.adapters` answers for `acp` and for `adapters`.
    fn quoted(word: &str) -> bool {
        [
            format!("`{word}`"),
            format!("`{word}."),
            format!(".{word}`"),
            format!(".{word}."),
        ]
        .iter()
        .any(|spelling| page().body.contains(spelling))
    }

    /// Every field of every settings type this plugin claims, out of the
    /// schema the claim carries: a row that grows a field grows a sentence
    /// here or fails.
    fn claimed() -> Vec<String> {
        let mut found = Vec::new();
        walk(schema().as_value(), &mut found);
        found.sort();
        found.dedup();
        found
    }

    fn walk(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                if let Some(Value::Object(properties)) = map.get("properties") {
                    found.extend(properties.keys().cloned());
                }
                map.values().for_each(|value| walk(value, found));
            }
            Value::Array(items) => items.iter().for_each(|value| walk(value, found)),
            _ => {}
        }
    }

    #[test]
    fn the_page_names_every_settings_key_and_field_this_plugin_claims() {
        let claim = MANIFEST.config.expect("the plugin claims settings");
        for (key, _) in claim.keys {
            assert!(page().body.contains(key), "the page never says {key}");
        }
        let claimed = claimed();
        assert!(claimed.len() > 5, "the schema was read: {claimed:?}");
        let unsaid: Vec<String> = claimed.into_iter().filter(|word| !quoted(word)).collect();
        assert!(unsaid.is_empty(), "the page never says {unsaid:?}");
    }

    /// The one model name bingo mints itself, and so the one a person can
    /// always type.
    #[test]
    fn the_page_names_the_label_that_never_crosses_the_wire() {
        assert!(quoted(AGENT), "the page never says the {AGENT} model");
    }

    /// The one command that answers differently at an ACP session than
    /// anywhere else (ADR-0055 §1): a person who types it gets a refusal, so
    /// the page owes it a sentence before they do.
    #[test]
    fn the_page_names_the_command_a_held_context_refuses() {
        assert!(quoted("/compact"), "the page never says /compact");
    }

    /// The tools an agent is never handed are the tools it brought itself. A
    /// name added to that list without a sentence here is a person wondering
    /// why their tool went missing.
    #[test]
    fn the_page_names_every_tool_that_does_not_cross_the_bridge() {
        let unsaid: Vec<&str> = NOT_THE_AGENTS
            .into_iter()
            .filter(|tool| !quoted(tool))
            .collect();
        assert!(unsaid.is_empty(), "the page never says {unsaid:?}");
    }

    /// This plugin registers no command of its own, so the page promises none.
    #[test]
    fn the_page_promises_no_command_this_plugin_does_not_register() {
        assert!(
            !MANIFEST
                .provides
                .iter()
                .any(|provided| provided.starts_with("command:")),
            "a command was added: the page owes it a sentence"
        );
    }

    #[test]
    fn the_page_is_one_line_of_description_and_a_body_worth_reading() {
        assert_eq!(page().name, "acp");
        assert!(!page().description.contains('\n'));
        assert!(page().description.chars().count() < 250);
        assert!(page().body.lines().count() < 200);
    }
}

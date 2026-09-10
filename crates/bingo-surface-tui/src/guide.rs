//! What this surface says about itself: the page the model reads as the
//! `guide-tui` skill (ADR-0054 §1).
//!
//! Its test reads [`crate::keys::BINDINGS`], which is the one binding table
//! the `?` panel and the footer already read: a key bound without a sentence
//! written for it fails here, beside the row that bound it.

use std::sync::Arc;

use bingo_sdk::{Contribution, Page, Pages, Registrar};

pub static PAGES: Pages = Pages(&[Page {
    name: "tui",
    description: "The terminal surface a person sits in front of: every key it \
                  binds, the four commands it answers itself, `@` mentions and \
                  pasted pictures in the composer, which terminals draw \
                  pictures, and the `tui.measure` and `update.check` settings.",
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
    use crate::commands::local_specs;
    use crate::keys::BINDINGS;

    fn page() -> &'static Page {
        let [page] = PAGES.0 else {
            panic!("one page");
        };
        page
    }

    /// Every chord this surface binds is a chord a person can be told about.
    /// The table is the one the `?` panel prints, so a key can never be bound
    /// in one place and described in another.
    #[test]
    fn the_page_names_every_key_this_surface_binds() {
        let unsaid: Vec<&str> = BINDINGS
            .iter()
            .map(|binding| binding.keys)
            .filter(|keys| !page().body.contains(&format!("`{keys}`")))
            .collect();
        assert!(unsaid.is_empty(), "the page never says {unsaid:?}");
    }

    #[test]
    fn the_page_names_every_command_this_surface_answers_itself() {
        for spec in local_specs() {
            assert!(
                page().body.contains(&format!("/{}", spec.name)),
                "the page never says /{}",
                spec.name
            );
        }
    }

    #[test]
    fn the_page_names_every_settings_key_this_surface_claims() {
        let claim = MANIFEST.config.expect("the surface claims settings");
        for (key, _) in claim.keys {
            assert!(
                page().body.contains(&format!("`{key}.")),
                "the page never says what is under {key}"
            );
        }
    }

    /// What a person meets of the libraries this surface draws with is said
    /// here, because they write no page of their own (ADR-0054 §5).
    #[test]
    fn the_page_says_what_becomes_of_a_picture() {
        for subject in ["[image N]", "pictures.cacheDays", "kitty"] {
            assert!(
                page().body.contains(subject),
                "the page never says {subject}"
            );
        }
    }

    #[test]
    fn the_page_is_one_line_of_description_and_a_body_worth_reading() {
        assert_eq!(page().name, "tui");
        assert!(!page().description.contains('\n'));
        assert!(page().description.chars().count() < 250);
        assert!(page().body.lines().count() < 200);
    }
}

//! The pages the loaded plugins wrote, read as skills (ADR-0054 §2).
//!
//! Nothing here knows a plugin: the catalogue names the ones that loaded, and
//! each id's pages are a typed service under the one key the sdk spells
//! ([`Pages::key`]). A plugin that wrote none has none, and a host that
//! answers no catalogue has none either — a page is what the model may read,
//! never what a turn depends on.

use bingo_sdk::{CatalogKind, HostHandle, Page, Pages};

use crate::skill::Skill;

/// What every page is called: the map is `guide`, and a page of it is
/// `guide-<name>`, so a person completing `/guide` sees the whole set.
const PREFIX: &str = "guide-";

/// Every page every loaded plugin wrote, by plugin id and then in the order
/// the plugin listed them.
pub async fn gather(host: &HostHandle) -> Vec<Skill> {
    ids(host)
        .await
        .iter()
        .flat_map(|id| of_plugin(host, id))
        .collect()
}

/// The loaded plugins, sorted: a listing must not read differently because a
/// plugin was registered earlier or later in the binary.
async fn ids(host: &HostHandle) -> Vec<String> {
    let Ok(catalog) = host.catalog(CatalogKind::Plugins).await else {
        return Vec::new();
    };
    let mut ids: Vec<String> = catalog.entries.into_iter().map(|entry| entry.id).collect();
    ids.sort();
    ids
}

fn of_plugin(host: &HostHandle, id: &str) -> Vec<Skill> {
    let Some(pages) = host.service::<Pages>(&Pages::key(id)) else {
        return Vec::new();
    };
    let written: &'static [Page] = pages.0;
    written.iter().map(skill).collect()
}

fn skill(page: &Page) -> Skill {
    Skill::page(
        &format!("{PREFIX}{}", page.name),
        page.description,
        page.body,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{PAGES, host_with_pages};

    #[tokio::test]
    async fn a_plugin_s_page_arrives_as_a_guide_skill() {
        let host = host_with_pages(&[("bingo.mcp", PAGES)]);
        let gathered = gather(&host).await;
        assert_eq!(names(&gathered), ["guide-first", "guide-second"]);
        assert_eq!(gathered[0].description, "The first page.");
        assert_eq!(gathered[0].body, "# First\n");
        assert!(gathered[0].dir.as_os_str().is_empty(), "no directory");
    }

    #[tokio::test]
    async fn the_pages_read_in_one_order_whatever_order_the_plugins_loaded_in() {
        let host = host_with_pages(&[("bingo.zebra", PAGES), ("bingo.alpha", PAGES)]);
        let gathered = gather(&host).await;
        assert_eq!(
            names(&gathered),
            ["guide-first", "guide-second", "guide-first", "guide-second"],
            "bingo.alpha's pages come first, each plugin's in its own order"
        );
    }

    #[tokio::test]
    async fn a_plugin_that_wrote_nothing_and_a_host_with_no_catalogue_are_both_empty() {
        assert!(gather(&host_with_pages(&[])).await.is_empty());
        assert!(
            gather(&bingo_sdk::testing::NoHost::handle())
                .await
                .is_empty(),
            "a page is never what a turn depends on"
        );
    }

    /// The skills plugin asks a page for its shape and nothing else: what a
    /// page *says* is asserted by the plugin that owns it (ADR-0054 §4).
    #[tokio::test]
    async fn every_page_gathered_is_shaped_like_a_page() {
        let host = host_with_pages(&[("bingo.mcp", PAGES), ("bingo.rooms", PAGES)]);
        let gathered = gather(&host).await;
        assert!(!gathered.is_empty(), "there is something to check");
        for page in gathered {
            let noun = page.name.strip_prefix(PREFIX).expect("guide-<name>");
            assert!(
                !noun.is_empty() && noun.chars().all(|c| c.is_ascii_lowercase()),
                "a page is named after one noun, in one word: {}",
                page.name
            );
            assert!(
                !page.description.contains('\n') && page.description.chars().count() < 250,
                "a description is one line in the prompt: {}",
                page.description
            );
            assert!(
                page.body.lines().count() < 200,
                "{} is a page, not a manual",
                page.name
            );
        }
    }

    fn names(skills: &[Skill]) -> Vec<&str> {
        skills.iter().map(|s| s.name.as_str()).collect()
    }
}

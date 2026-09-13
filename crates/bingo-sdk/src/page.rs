//! What a plugin has to say about itself (ADR-0054 §1).
//!
//! A page is plain data: three static strings a plugin `include_str!`s from a
//! file beside its own code and hands over under one key. The kernel never
//! reads a page — it carries the type so that the plugin that owns a noun and
//! the plugin that gathers the pages need no import of one another, which is
//! the whole contract: this struct, and the spelling of [`Pages::key`].

/// One page: the name it is filed under, the line that stands for it in a
/// listing, and the text somebody reads when they ask for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Page {
    /// One word, lower case: the noun this page is about.
    pub name: &'static str,
    /// One sentence, for the listing that indexes the pages.
    pub description: &'static str,
    /// The page itself, markdown, with no frontmatter: a page is read, not run.
    pub body: &'static str,
}

/// Every page one plugin writes, in the order it wants them read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pages(pub &'static [Page]);

impl Pages {
    /// Where a plugin's pages are found: `"<plugin id>.pages"`, the key it
    /// registers a `Contribution::Service` under and lists in `provides` as
    /// `service:<key>`. One spelling, in one place, so neither side of the
    /// contract writes it out by hand.
    pub fn key(plugin_id: &str) -> String {
        format!("{plugin_id}.pages")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plugin_s_pages_are_found_under_its_own_id() {
        assert_eq!(Pages::key("bingo.rooms"), "bingo.rooms.pages");
        assert_eq!(Pages::key("bingo.mcp"), "bingo.mcp.pages");
    }

    #[test]
    fn a_page_is_three_strings_and_nothing_else() {
        static PAGES: Pages = Pages(&[Page {
            name: "example",
            description: "What this plugin does.",
            body: "# Example\n",
        }]);
        let [page] = PAGES.0 else {
            panic!("one page");
        };
        assert_eq!(page.name, "example");
        assert_eq!(PAGES, PAGES, "a page is data: two readings are one fact");
    }
}

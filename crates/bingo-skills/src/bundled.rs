//! The skills the binary ships with. They have no directory on disk, so
//! `${BINGO_SKILL_DIR}` in one of them stands for nothing.

use std::path::PathBuf;

use crate::listing;
use crate::skill::Skill;

/// What bingo is, for a model that has been asked about bingo itself. The map,
/// not the manual: what belongs to a plugin is on that plugin's page.
const GUIDE: &str = include_str!("bundled/guide.md");

const PAGES_HEADING: &str = "## Pages";

const PAGES_PREAMBLE: &str = "\
One page per plugin, about the nouns that plugin owns. Read one by calling the \
`Skill` tool with `guide-<name>`, or by typing `/guide-<name>`.";

/// Every bundled skill. A skill of the same name in any layer overrides one.
pub fn skills(pages: &[Skill]) -> Vec<Skill> {
    vec![Skill::parse("guide", PathBuf::new(), &guide(pages))]
}

/// The map, and the index of the pages this build found (ADR-0054 §3). A build
/// whose plugins wrote nothing ends at the map: a heading over no lines says
/// there are pages somewhere, and there are not.
fn guide(pages: &[Skill]) -> String {
    if pages.is_empty() {
        return GUIDE.to_string();
    }
    format!(
        "{GUIDE}\n{PAGES_HEADING}\n\n{PAGES_PREAMBLE}\n\n{}\n",
        listing::lines(pages)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guide_parses_and_says_what_it_is_for() {
        let skills = skills(&[]);
        assert_eq!(skills.len(), 1);
        let guide = &skills[0];
        assert_eq!(guide.name, "guide");
        assert!(
            guide.description.contains("Read it before answering"),
            "the frontmatter parsed and the description tells the model when \
             to read the guide: {}",
            guide.description
        );
        assert!(
            !guide.description.contains('\n'),
            "a folded description is one line: {}",
            guide.description
        );
        assert!(guide.dir.as_os_str().is_empty(), "it is in the binary");
        assert!(
            !guide.body.starts_with("---"),
            "the frontmatter was split off"
        );
    }

    #[test]
    fn the_guide_describes_this_product() {
        let guide = &skills(&[])[0];
        for subject in [
            "--print",
            "serve --stdio",
            "--continue",
            "/model",
            "/compact",
            "/permission",
            "acceptEdits",
            "hooks",
            "skills",
            "MCP",
            "~/.bingo",
        ] {
            assert!(
                guide.body.contains(subject),
                "the guide never says {subject}"
            );
        }
    }

    #[test]
    fn the_guide_ends_in_the_pages_this_build_found() {
        let pages = [
            Skill::page("guide-mcp", "What MCP is here.", "# MCP\n"),
            Skill::page("guide-hooks", "What a hook is here.", "# Hooks\n"),
        ];
        let body = skills(&pages)[0].body.clone();
        let listed: Vec<&str> = body
            .lines()
            .skip_while(|line| *line != PAGES_HEADING)
            .filter(|line| line.starts_with("- "))
            .collect();
        assert_eq!(
            listed,
            [
                "- guide-mcp — What MCP is here.",
                "- guide-hooks — What a hook is here."
            ],
            "one line per page, in the order they were gathered"
        );
        assert!(
            body.contains("`/guide-<name>`"),
            "the map says how a page is read: {body}"
        );
    }

    #[test]
    fn a_build_whose_plugins_wrote_nothing_ends_at_the_map() {
        assert!(!skills(&[])[0].body.contains(PAGES_HEADING));
    }

    #[test]
    fn the_guide_stays_short_enough_to_read() {
        assert!(
            skills(&[])[0].body.lines().count() <= 200,
            "the guide is a page, not a manual"
        );
    }
}

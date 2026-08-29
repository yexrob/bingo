//! The skills the binary ships with. They have no directory on disk, so
//! `${BINGO_SKILL_DIR}` in one of them stands for nothing.

use std::path::PathBuf;

use crate::skill::Skill;

/// What bingo is, for a model that has been asked about bingo itself.
const GUIDE: &str = include_str!("bundled/guide.md");

/// Every bundled skill. A skill of the same name in any layer overrides one.
pub fn skills() -> Vec<Skill> {
    vec![Skill::parse("guide", PathBuf::new(), GUIDE)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_guide_parses_and_says_what_it_is_for() {
        let skills = skills();
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
        let guide = &skills()[0];
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
    fn the_guide_stays_short_enough_to_read() {
        assert!(
            skills()[0].body.lines().count() <= 200,
            "the guide is a page, not a manual"
        );
    }
}

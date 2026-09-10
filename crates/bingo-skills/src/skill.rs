//! One skill: what a `SKILL.md` becomes once it has been read.

use std::path::PathBuf;

use crate::frontmatter;

/// A procedure written down: the name it answers to, what it says it is for,
/// and the body that becomes a prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// What to show while completing `/name`, from `argument-hint`.
    pub argument_hint: Option<String>,
    /// Names for the positional arguments, in order.
    pub argument_names: Vec<String>,
    /// Read and recorded, never enforced (M7 non-goal).
    pub allowed_tools: Vec<String>,
    /// Read and recorded, never enforced (M7 non-goal).
    pub model: Option<String>,
    /// The directory holding the `SKILL.md`, and what `${BINGO_SKILL_DIR}`
    /// stands for. Empty for a bundled skill, which is in the binary.
    pub dir: PathBuf,
    pub body: String,
}

impl Skill {
    /// A skill as its file spells it. The directory name is the fallback the
    /// frontmatter's `name` overrides, and the body's first line is the
    /// fallback its `description` overrides.
    pub fn parse(dir_name: &str, dir: PathBuf, source: &str) -> Self {
        let (front, body) = frontmatter::parse(source);
        Self {
            name: front.name.clone().unwrap_or_else(|| dir_name.to_string()),
            description: front
                .description
                .clone()
                .unwrap_or_else(|| frontmatter::first_line(body)),
            argument_hint: front.argument_hint.clone(),
            argument_names: front.argument_names(),
            allowed_tools: front.allowed_tools.0.clone(),
            model: front.model.clone(),
            dir,
            body: body.to_string(),
        }
    }

    /// A page a plugin wrote about itself (ADR-0054 §2). There is no
    /// frontmatter to read and no directory to be relative to, so neither is
    /// invented: the name, the line and the body are all a page has.
    pub fn page(name: &str, description: &str, body: &str) -> Self {
        Self {
            name: name.to_string(),
            description: description.to_string(),
            argument_hint: None,
            argument_names: Vec::new(),
            allowed_tools: Vec::new(),
            model: None,
            dir: PathBuf::new(),
            body: body.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_with_no_frontmatter_is_named_after_its_directory() {
        let skill = Skill::parse(
            "summarise",
            PathBuf::from("/skills/summarise"),
            "Summarise the diff.\n",
        );
        assert_eq!(skill.name, "summarise");
        assert_eq!(skill.description, "Summarise the diff.");
        assert_eq!(skill.body, "Summarise the diff.\n");
        assert_eq!(skill.dir, PathBuf::from("/skills/summarise"));
    }

    #[test]
    fn the_frontmatter_name_wins_over_the_directory() {
        let skill = Skill::parse("dir", PathBuf::new(), "---\nname: other\n---\nbody\n");
        assert_eq!(skill.name, "other");
    }

    #[test]
    fn a_skill_with_neither_description_nor_body_still_loads() {
        let skill = Skill::parse("empty", PathBuf::new(), "---\nname: empty\n---\n");
        assert_eq!(skill.description, "");
        assert_eq!(skill.body, "");
    }

    #[test]
    fn a_page_carries_its_three_strings_and_invents_nothing() {
        let page = Skill::page("guide-mcp", "What MCP is here.", "# MCP\n");
        assert_eq!(page.name, "guide-mcp");
        assert_eq!(page.description, "What MCP is here.");
        assert_eq!(page.body, "# MCP\n");
        assert!(page.dir.as_os_str().is_empty(), "a page is in the binary");
        assert!(page.argument_hint.is_none());
        assert!(page.argument_names.is_empty());
    }

    #[test]
    fn what_is_recorded_but_not_enforced_is_still_recorded() {
        let skill = Skill::parse(
            "deploy",
            PathBuf::new(),
            "---\nallowed-tools: [Read, Grep]\nmodel: fake/fake-2\n---\nbody\n",
        );
        assert_eq!(skill.allowed_tools, ["Read", "Grep"]);
        assert_eq!(skill.model.as_deref(), Some("fake/fake-2"));
    }
}

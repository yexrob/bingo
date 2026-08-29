//! How a set of skills reads as a list: one line each, name and description.
//! Pure, and shared by the prompt, the `/` dropdown and the tool's error.

use crate::skill::Skill;

/// What one description may spend. Past this it is not a description any more,
/// and the detail belongs in the body the skill hands over when it is invoked.
const MAX_CHARS: usize = 250;

/// A description as one line: a folded or literal YAML scalar may hold
/// newlines, and a list is only a list while each entry is one row.
pub fn one_line(description: &str) -> String {
    let folded = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if folded.chars().count() <= MAX_CHARS {
        return folded;
    }
    let kept: String = folded.chars().take(MAX_CHARS - 1).collect();
    format!("{kept}…")
}

/// `- name — description` per skill, in order, one per line. A skill with no
/// description is still named: a model that cannot see a name concludes the
/// skill does not exist.
pub fn lines(skills: &[Skill]) -> String {
    skills.iter().map(entry).collect::<Vec<_>>().join("\n")
}

fn entry(skill: &Skill) -> String {
    let description = one_line(&skill.description);
    if description.is_empty() {
        return format!("- {}", skill.name);
    }
    format!("- {} — {description}", skill.name)
}

/// The names alone, for a message that must say what was on offer.
pub fn names(skills: &[Skill]) -> String {
    skills
        .iter()
        .map(|skill| skill.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn skill(name: &str, description: &str) -> Skill {
        Skill::parse(
            name,
            PathBuf::new(),
            &format!("---\ndescription: |\n  {description}\n---\nbody\n"),
        )
    }

    #[test]
    fn a_description_written_over_several_lines_reads_as_one() {
        assert_eq!(one_line("first\nsecond\n  third"), "first second third");
    }

    #[test]
    fn a_description_that_grew_into_a_document_is_cut() {
        let long = "word ".repeat(200);
        let line = one_line(&long);
        assert_eq!(line.chars().count(), MAX_CHARS);
        assert!(line.ends_with('…'));
    }

    #[test]
    fn one_line_per_skill_name_and_description() {
        let skills = [
            skill("deploy", "Ship the build"),
            skill("review", "Read a diff"),
        ];
        assert_eq!(
            lines(&skills),
            "- deploy — Ship the build\n- review — Read a diff"
        );
    }

    #[test]
    fn a_skill_with_nothing_to_say_is_still_named() {
        let skills = [Skill::parse("quiet", PathBuf::new(), "")];
        assert_eq!(lines(&skills), "- quiet");
    }

    #[test]
    fn the_names_alone_are_a_sentence_a_model_can_read() {
        let skills = [skill("a", "x"), skill("b", "y")];
        assert_eq!(names(&skills), "a, b");
    }
}

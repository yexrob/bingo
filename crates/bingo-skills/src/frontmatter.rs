//! The `---` block at the top of a `SKILL.md`, and the fields this product
//! reads from it. Pure: text in, values out, no file ever touched.

use serde::de::{self, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

/// The line that opens and closes the block.
const FENCE: &str = "---";

/// What a `SKILL.md` may declare. Every field is optional: a file with no
/// frontmatter at all is still a skill, and a key this plugin does not know
/// (`when_to_use`, `license`, `metadata`, …) is ignored rather than refused.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct Frontmatter {
    /// The name the skill answers to; the directory name when absent.
    pub name: Option<String>,
    /// What the skill is for. The body's first line when absent.
    pub description: Option<String>,
    /// What to show beside the name while completing, e.g. `[issue-number]`.
    pub argument_hint: Option<String>,
    /// Names for the positional arguments, in order, for `$name` substitution.
    #[serde(default)]
    arguments: TextOrList,
    /// Read and recorded, never enforced (M7 non-goal), so it is kept exactly
    /// as written: splitting `Bash(git add *)` is the enforcer's problem.
    #[serde(default)]
    pub allowed_tools: TextOrList,
    /// Read and recorded, never enforced (M7 non-goal).
    pub model: Option<String>,
}

impl Frontmatter {
    /// The argument names, in declaration order. `arguments: issue branch` and
    /// `arguments: [issue, branch]` say the same thing, so both arrive here as
    /// two names.
    pub fn argument_names(&self) -> Vec<String> {
        self.arguments
            .0
            .iter()
            .flat_map(|entry| entry.split([',', ' ', '\t']))
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect()
    }
}

/// A field YAML lets a person write either as one scalar or as a list.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextOrList(pub Vec<String>);

impl<'de> Deserialize<'de> for TextOrList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(EitherShape)
    }
}

struct EitherShape;

impl<'de> Visitor<'de> for EitherShape {
    type Value = TextOrList;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a string or a list of strings")
    }

    fn visit_str<E: de::Error>(self, text: &str) -> Result<Self::Value, E> {
        Ok(TextOrList(vec![text.to_string()]))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        Ok(TextOrList::default())
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        let mut entries = Vec::new();
        while let Some(entry) = seq.next_element::<String>()? {
            entries.push(entry);
        }
        Ok(TextOrList(entries))
    }
}

/// The frontmatter block and the body that follows it.
///
/// A block counts only when `---` is the file's very first line and a closing
/// `---` follows; otherwise the whole file, markers and all, is the body.
pub fn split(source: &str) -> (Option<&str>, &str) {
    let mut lines = source.split_inclusive('\n');
    let Some(open) = lines.next().filter(|line| line.trim_end() == FENCE) else {
        return (None, source);
    };
    let start = open.len();
    let mut end = start;
    for line in lines {
        if line.trim_end() == FENCE {
            return (Some(&source[start..end]), body(&source[end + line.len()..]));
        }
        end += line.len();
    }
    (None, source)
}

/// The body starts at its first real line: the newline that ended the closing
/// fence is punctuation, not content.
fn body(rest: &str) -> &str {
    rest.trim_start_matches(['\n', '\r'])
}

/// The declared fields and the body.
///
/// A block YAML cannot read is treated as no block at all: a skill with a
/// mistyped header still runs, with its description taken from the body.
pub fn parse(source: &str) -> (Frontmatter, &str) {
    let (block, body) = split(source);
    let front = block
        .and_then(|block| serde_saphyr::from_str::<Frontmatter>(block).ok())
        .unwrap_or_default();
    (front, body)
}

/// The first line with something on it, which is what a skill that declared no
/// description is about.
pub fn first_line(body: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_without_frontmatter_is_all_body() {
        let source = "# Deploy\n\nRun the thing.\n";
        assert_eq!(split(source), (None, source));
        let (front, body) = parse(source);
        assert_eq!(front, Frontmatter::default());
        assert_eq!(body, source);
    }

    #[test]
    fn a_fence_that_is_not_the_first_line_is_content() {
        let source = "intro\n---\nname: x\n---\nbody\n";
        assert_eq!(split(source), (None, source));
    }

    #[test]
    fn an_unclosed_block_is_content() {
        let source = "---\nname: x\nbody without a closing fence\n";
        assert_eq!(split(source), (None, source));
    }

    #[test]
    fn the_block_is_split_from_the_body() {
        let (block, body) = split("---\nname: deploy\n---\n\n# Deploy\n");
        assert_eq!(block, Some("name: deploy\n"));
        assert_eq!(body, "# Deploy\n");
    }

    #[test]
    fn every_field_this_plugin_reads() {
        let (front, body) = parse(
            "---\n\
             name: deploy\n\
             description: Ship the build\n\
             argument-hint: \"[env]\"\n\
             arguments: [env, tag]\n\
             allowed-tools: Bash(git add *)\n\
             model: fake/fake-2\n\
             ---\n\
             Deploy $env at $tag.\n",
        );
        assert_eq!(front.name.as_deref(), Some("deploy"));
        assert_eq!(front.description.as_deref(), Some("Ship the build"));
        assert_eq!(front.argument_hint.as_deref(), Some("[env]"));
        assert_eq!(front.argument_names(), ["env", "tag"]);
        assert_eq!(front.allowed_tools.0, ["Bash(git add *)"]);
        assert_eq!(front.model.as_deref(), Some("fake/fake-2"));
        assert_eq!(body, "Deploy $env at $tag.\n");
    }

    #[test]
    fn a_folded_description_is_one_line() {
        let (front, _) = parse(
            "---\n\
             description: >-\n\
             \x20 Review a diff for the standards\n\
             \x20 this repository writes down.\n\
             ---\n\
             body\n",
        );
        assert_eq!(
            front.description.as_deref(),
            Some("Review a diff for the standards this repository writes down.")
        );
    }

    #[test]
    fn a_literal_description_keeps_its_newlines() {
        let (front, _) = parse("---\ndescription: |\n  first\n  second\n---\nbody\n");
        assert_eq!(front.description.as_deref(), Some("first\nsecond\n"));
    }

    #[test]
    fn arguments_written_as_one_string_are_the_same_names() {
        let (front, _) = parse("---\narguments: issue branch\n---\nbody\n");
        assert_eq!(front.argument_names(), ["issue", "branch"]);
        let (front, _) = parse("---\narguments: [issue, branch]\n---\nbody\n");
        assert_eq!(front.argument_names(), ["issue", "branch"]);
    }

    #[test]
    fn allowed_tools_written_as_a_list_keeps_its_entries() {
        let (front, _) = parse("---\nallowed-tools:\n  - Read\n  - Bash(git status:*)\n---\nx\n");
        assert_eq!(front.allowed_tools.0, ["Read", "Bash(git status:*)"]);
    }

    #[test]
    fn a_key_this_plugin_does_not_know_is_ignored() {
        let (front, _) = parse(
            "---\nname: x\nwhen_to_use: whenever\nlicense: MIT\nmetadata:\n  a: 1\n---\nbody\n",
        );
        assert_eq!(front.name.as_deref(), Some("x"));
    }

    #[test]
    fn a_block_yaml_cannot_read_leaves_the_skill_with_its_body() {
        let (front, body) = parse("---\nname: [unterminated\n---\nthe body survives\n");
        assert_eq!(front, Frontmatter::default());
        assert_eq!(body, "the body survives\n");
    }

    #[test]
    fn the_first_line_is_the_first_one_with_something_on_it() {
        assert_eq!(first_line("\n\n  what it does  \nmore\n"), "what it does");
        assert_eq!(first_line("   \n"), "");
    }
}

//! An agent definition: one `<name>.md`, its frontmatter and its body. Pure:
//! text in, values out, no file ever touched.

use serde::Deserialize;

/// The line that opens and closes the frontmatter block.
const FENCE: &str = "---";

/// What a definition file may declare above its body. Every field is optional:
/// a file with no frontmatter at all is still a definition, and a key this
/// plugin does not know is ignored rather than refused.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Frontmatter {
    /// The name the agent answers to; the file's own name when absent.
    name: Option<String>,
    /// What the agent is for. The body's first line when absent.
    description: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    /// How hard the child thinks: `off`, or a level. A word, kept a word
    /// here; the spawn is where it becomes a level or a refusal.
    thinking: Option<String>,
    /// The tools the child may call, by name.
    tools: Option<Vec<String>>,
}

/// A named persona `SpawnAgent` may ask for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Definition {
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    /// How hard the child thinks, as the file said it; `None` leaves the
    /// level to the caller and then to the parent.
    pub thinking: Option<String>,
    /// `None` leaves the choice to the caller and then to the host.
    pub tools: Option<Vec<String>>,
    /// The body: the child's system prompt, after the sub-agent note.
    pub system: String,
}

impl Definition {
    /// The definition in `source`, named after its file when it names itself
    /// nothing. A block YAML cannot read is treated as no block at all: a
    /// definition with a mistyped header still has its system prompt.
    pub fn parse(file_name: &str, source: &str) -> Definition {
        let (block, body) = split(source);
        let front = block
            .and_then(|block| serde_saphyr::from_str::<Frontmatter>(block).ok())
            .unwrap_or_default();
        Definition {
            name: front.name.unwrap_or_else(|| file_name.to_string()),
            description: front.description.unwrap_or_else(|| first_line(body)),
            model: front.model,
            provider: front.provider,
            thinking: front.thinking,
            tools: front.tools,
            system: body.to_string(),
        }
    }
}

/// The frontmatter block and the body that follows it.
///
/// A block counts only when `---` is the file's very first line and a closing
/// `---` follows; otherwise the whole file, markers and all, is the body.
fn split(source: &str) -> (Option<&str>, &str) {
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

/// The first line with something on it, which is what a definition that
/// declared no description is about.
fn first_line(body: &str) -> String {
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
    fn every_field_this_plugin_reads() {
        let definition = Definition::parse(
            "file",
            "---\n\
             name: reviewer\n\
             description: Reviews a diff\n\
             model: fake-1\n\
             provider: fake\n\
             thinking: high\n\
             tools: [Read, Grep]\n\
             ---\n\
             You review diffs.\n",
        );
        assert_eq!(definition.name, "reviewer");
        assert_eq!(definition.description, "Reviews a diff");
        assert_eq!(definition.model.as_deref(), Some("fake-1"));
        assert_eq!(definition.provider.as_deref(), Some("fake"));
        assert_eq!(definition.thinking.as_deref(), Some("high"));
        assert_eq!(definition.tools, Some(vec!["Read".into(), "Grep".into()]));
        assert_eq!(definition.system, "You review diffs.\n");
    }

    #[test]
    fn a_definition_that_names_itself_nothing_is_named_after_its_file() {
        let definition = Definition::parse("reviewer", "---\ndescription: d\n---\nbody\n");
        assert_eq!(definition.name, "reviewer");
        assert_eq!(definition.tools, None);
        assert_eq!(definition.model, None);
    }

    #[test]
    fn a_file_without_frontmatter_is_all_system_prompt() {
        let definition = Definition::parse("plain", "# Plain\n\nDo the thing.\n");
        assert_eq!(definition.system, "# Plain\n\nDo the thing.\n");
        assert_eq!(definition.description, "# Plain");
    }

    #[test]
    fn a_block_yaml_cannot_read_leaves_the_system_prompt_alone() {
        let definition = Definition::parse("broken", "---\nname: [unterminated\n---\nthe body\n");
        assert_eq!(definition.name, "broken");
        assert_eq!(definition.system, "the body\n");
    }

    #[test]
    fn a_fence_that_is_not_the_first_line_is_content() {
        let source = "intro\n---\nname: x\n---\nbody\n";
        assert_eq!(split(source), (None, source));
    }

    #[test]
    fn an_unclosed_block_is_content() {
        let source = "---\nname: x\nno closing fence\n";
        assert_eq!(split(source), (None, source));
    }

    /// serde-saphyr reads YAML 1.2, where `off` is a word and not `false`
    /// (it was a boolean in 1.1). The spawn parses a word, so a definition
    /// that turns thinking off must arrive as one.
    #[test]
    fn thinking_off_arrives_as_the_word_and_not_a_boolean() {
        let definition = Definition::parse("quiet", "---\nthinking: off\n---\nbody\n");
        assert_eq!(definition.thinking.as_deref(), Some("off"));
        let leveled = Definition::parse("deep", "---\nthinking: xhigh\n---\nbody\n");
        assert_eq!(leveled.thinking.as_deref(), Some("xhigh"));
        assert_eq!(Definition::parse("plain", "body\n").thinking, None);
    }

    #[test]
    fn a_tool_list_written_as_a_block_keeps_its_entries() {
        let definition = Definition::parse("t", "---\ntools:\n  - Read\n  - Bash\n---\nb\n");
        assert_eq!(definition.tools, Some(vec!["Read".into(), "Bash".into()]));
    }
}

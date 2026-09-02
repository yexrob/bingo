//! One skill run, whichever door it came through.
//!
//! The model calls the `Skill` tool; a person types `/guide the wire format`.
//! It is the same thing happening — a skill's body enters the session — so the
//! surface draws one row for it (design §4), and the question "is this item a
//! skill run, and which one" is asked here, once, for both doors.
//!
//! Nothing here draws: [`crate::transcript`] turns a [`Run`] into the row, and
//! [`crate::blocks`] asks the same question to know whether a cached block is
//! still what the catalogue says it is.

use bingo_sdk::{CommandSpec, ContentPart, Item, ItemBody};
use serde_json::Value;

use crate::commands;

/// The tool the model reaches a skill through (`bingo-skills`), and the name
/// the row wears for both doors.
pub const TOOL: &str = "Skill";

/// The family `bingo-skills` files its `/name` commands under. Every command
/// that answers with a `Prompt` is a skill today, but that is a fact about
/// what has been written so far and not about what a `Prompt` means — so the
/// row asks the catalogue what a name *is*, never what it returns.
const FAMILY: &str = "skill";

/// One run: the skill's own name, and the free text it was given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run<'a> {
    pub name: &'a str,
    pub args: &'a str,
}

/// The skill run an item is, if it is one. `specs` is the commands catalogue
/// the surface read at start ([`crate::ui::Catalogs`]) — the only place a
/// `/name` says which family it belongs to.
pub fn of<'a>(item: &'a Item, specs: &[CommandSpec]) -> Option<Run<'a>> {
    match &item.body {
        ItemBody::ToolCall { name, input, .. } if name == TOOL => called(input),
        ItemBody::User { parts, origin } if origin.surface == commands::SURFACE => {
            typed(asked(parts), specs)
        }
        _ => None,
    }
}

/// The model's way in: the call names the skill and the text it hands it. A
/// call whose input says neither is no run — the row falls back to the tool it
/// is, rather than inventing a name for the screen.
fn called(input: &Value) -> Option<Run<'_>> {
    Some(Run {
        name: input.get("name").and_then(Value::as_str)?,
        args: input
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    })
}

/// A person's way in: the `/name args` line, when the catalogue files that
/// name under the skills. A command that answers with a prompt and is *not* a
/// skill keeps the row it has — the line somebody typed.
fn typed<'a>(line: &'a str, specs: &[CommandSpec]) -> Option<Run<'a>> {
    let (name, args) = commands::split(line)?;
    let spec = specs
        .iter()
        .find(|spec| spec.name == name || spec.aliases.iter().any(|alias| alias == name))?;
    (spec.family == FAMILY).then_some(Run { name, args })
}

/// The line the run was asked by. A command's prompt leads with it and is
/// written nowhere else (ADR-0008 §3, `Invocation::prompt`), and the kernel
/// submits that prompt as one text part.
fn asked(parts: &[ContentPart]) -> &str {
    parts
        .iter()
        .find_map(ContentPart::as_text)
        .unwrap_or_default()
        .lines()
        .next()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{delivered, item, tool, user};
    use bingo_sdk::{ArgSpec, ItemStatus};
    use serde_json::json;

    fn spec(name: &str, family: &str) -> CommandSpec {
        CommandSpec {
            name: name.into(),
            aliases: Vec::new(),
            hint: String::new(),
            args: ArgSpec::Free {
                hint: String::new(),
            },
            instant: false,
            family: family.into(),
        }
    }

    fn catalogue() -> Vec<CommandSpec> {
        vec![spec("guide", "skill"), spec("compact", "kernel")]
    }

    /// A command's prompt, as the kernel journals it: the typed line, then the
    /// body it expanded to.
    fn prompt(line: &str) -> Item {
        delivered(
            "itm_1",
            commands::SURFACE,
            None,
            &format!("{line}\n\nBase directory for this skill: /skills/guide\n\nDo the thing."),
        )
    }

    /// The two doors, answering with the same run.
    #[test]
    fn a_skill_is_the_same_run_whether_the_model_called_it_or_a_person_typed_it() {
        let called = tool(
            "itm_1",
            "Skill",
            json!({"name": "guide", "arguments": "the wire format"}),
            None,
            ItemStatus::Running,
        );
        let run = Some(Run {
            name: "guide",
            args: "the wire format",
        });
        assert_eq!(of(&called, &catalogue()), run);
        assert_eq!(of(&prompt("/guide the wire format"), &catalogue()), run);
    }

    #[test]
    fn a_skill_asked_for_with_no_arguments_carries_none() {
        assert_eq!(
            of(&prompt("/guide"), &catalogue()),
            Some(Run {
                name: "guide",
                args: ""
            })
        );
    }

    /// The catalogue decides, not the outcome: a command the surface has never
    /// heard of, and one filed under another family, are both left alone.
    #[test]
    fn a_command_the_catalogue_does_not_file_as_a_skill_is_not_one() {
        assert_eq!(of(&prompt("/compact why"), &catalogue()), None);
        assert_eq!(of(&prompt("/unknown"), &catalogue()), None);
        assert_eq!(
            of(&prompt("/guide the wire format"), &[]),
            None,
            "and before the catalogue lands nothing is a skill"
        );
    }

    /// Prose that starts with a slash is prose: only what a command produced
    /// carries the command surface.
    #[test]
    fn a_line_from_any_other_surface_is_not_a_run() {
        assert_eq!(
            of(&user("itm_1", "/guide the wire format"), &catalogue()),
            None
        );
        assert_eq!(
            of(
                &delivered("itm_1", "agent", None, "/guide the wire format"),
                &catalogue()
            ),
            None
        );
    }

    /// A call with nothing to name is the tool it is, drawn as any other.
    #[test]
    fn a_skill_call_that_names_nothing_is_no_run() {
        let malformed = tool(
            "itm_1",
            "Skill",
            json!({"arguments": "the wire format"}),
            None,
            ItemStatus::Running,
        );
        assert_eq!(of(&malformed, &catalogue()), None);
        let other = tool(
            "itm_1",
            "Read",
            json!({"name": "guide"}),
            None,
            ItemStatus::Running,
        );
        assert_eq!(of(&other, &catalogue()), None);
    }

    #[test]
    fn an_item_that_is_neither_door_is_no_run() {
        let assistant = item(
            "itm_1",
            ItemStatus::Completed,
            ItemBody::Assistant {
                text: "/guide".into(),
            },
        );
        assert_eq!(of(&assistant, &catalogue()), None);
    }
}

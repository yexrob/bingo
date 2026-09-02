//! The screens one skill run is read through (§4's skill row): the two doors
//! it can arrive by, and the look a terminal with no glyphs gets. The row is
//! the same through either door, which is what these snapshots are for.

use super::*;

/// A skill's prompt as the kernel journals it: the line the person typed,
/// then what the command made of it.
pub(super) const SKILL_PROMPT: &str = "/guide the wire format\n\n\
     Base directory for this skill: ~/.bingo/skills/guide\n\n\
     Read this before answering a question about bingo itself.\n\n\
     - the kernel is small, and everything else is a plugin\n\
     - one ordered event stream, and every surface is a client of it\n\
     - `--print` is the headless surface, `serve --stdio` the JSON-RPC one\n\
     - a skill is a directory under `.bingo/skills` with a `SKILL.md` in it\n\n\
     ARGUMENTS: the wire format";

/// The catalogue that files `guide` under the skills, which is how the surface
/// tells a skill from any other command that answers with a prompt.
pub(super) fn skills_in_the_catalogue() -> Vec<bingo_sdk::CommandSpec> {
    vec![bingo_sdk::CommandSpec {
        name: "guide".into(),
        aliases: vec![],
        hint: "[topic]".into(),
        args: bingo_sdk::ArgSpec::Free {
            hint: "[topic]".into(),
        },
        instant: false,
        family: "skill".into(),
    }]
}

/// The answer the skill's body was read for, which is what closes each scene.
const ANSWER: &str = "One `Event` enum, one ordered stream per session.";

/// The person's door: the `/guide` line the kernel put at the head of the
/// prompt, with the page it expanded to under it.
fn typed() -> bingo_sdk::SessionState {
    folded(vec![
        item(1, delivered("itm_1", "command", None, SKILL_PROMPT)),
        item(2, assistant("itm_2", ANSWER, ItemStatus::Completed)),
    ])
}

/// The model's door: the `Skill` call names the skill and hands it the same
/// words, and the body comes back as the call's output.
fn called() -> bingo_sdk::SessionState {
    folded(vec![
        item(1, user("itm_1", "read the guide on the wire format")),
        item(
            2,
            tool(
                "itm_2",
                "Skill",
                json!({"name": "guide", "arguments": "the wire format"}),
                Some(ToolOutput::text(body())),
                ItemStatus::Completed,
            ),
        ),
        item(3, assistant("itm_3", ANSWER, ItemStatus::Completed)),
    ])
}

/// The skill's page without the line that asked for it: what the command's
/// prompt carries under its head, and what the call answers with.
fn body() -> &'static str {
    SKILL_PROMPT
        .split_once("\n\n")
        .map(|(_, body)| body)
        .unwrap_or(SKILL_PROMPT)
}

/// `/guide` typed: the row is the run — `❖ Skill(guide) the wire format` — and
/// the page the command produced folds under it. The line the person typed is
/// still in the item, where a rewind reads it back; it is no longer the
/// headline, because a skill is one row however it was asked for.
#[test]
fn a_skill_command() {
    let (mut ui, now) = scene();
    ui.catalogs.commands = skills_in_the_catalogue();
    both("skill_command", &solo(&typed()), &ui, now);
}

/// The same skill, the model's own way in: the row is the one above to the
/// cell. The catalogue is not consulted here — the call already says what it
/// is.
#[test]
fn a_skill_the_model_called() {
    let (ui, now) = scene();
    both("skill_tool", &solo(&called()), &ui, now);
}

/// A terminal with no glyphs (§7): the mark degrades to the bullet's own `*`
/// and the row still says which kind it is, because it says so in words.
#[test]
fn a_skill_without_the_glyphs() {
    let (mut ui, now) = scene();
    ui.catalogs.commands = skills_in_the_catalogue();
    without_glyphs("ascii_skill", &solo(&typed()), &ui, now);
}

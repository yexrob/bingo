//! Skills: a `SKILL.md` on disk is a `/name` command, a `Skill` tool the model
//! calls, and a line in the system prompt (ADR-0009 §3).
//!
//! # What the reference says, and where this differs
//!
//! Checked against <https://code.claude.com/docs/en/skills> (which
//! `docs.claude.com/en/docs/claude-code/skills` and the slash-commands page
//! both redirect to) on 2026-08-29.
//!
//! *Frontmatter.* The reference lists `name`, `description`, `when_to_use`,
//! `argument-hint`, `arguments`, `disable-model-invocation`, `user-invocable`,
//! `allowed-tools`, `disallowed-tools`, `model`, `effort`, `context`, `agent`,
//! `background`, `hooks`, `paths`, `shell`, `metadata`, `license`,
//! `compatibility`. This plugin reads six of them — `name`, `description`,
//! `argument-hint`, `arguments`, `allowed-tools`, `model` — and ignores the
//! rest rather than refusing a file that carries them. `allowed-tools` and
//! `model` are recorded and never enforced (an M7 non-goal). Frontmatter
//! counts only when `---` is the file's first line, as the reference says.
//!
//! *Layout.* The reference discovers `~/.claude/skills/<name>/SKILL.md`,
//! `.claude/skills/` from the start directory up to the repository root, and
//! plugin skills. Here it is `<config_dir>/skills/<name>/SKILL.md` first, then
//! `.bingo/skills/<name>/SKILL.md` walking up from the working directory to
//! the filesystem root, nearest first, then the bundled guide, which any disk
//! skill of the same name overrides. ADR-0009 says "the git common root";
//! walking up from cwd is what happens instead, so no `git` is needed and a
//! directory outside a repository has skills too.
//!
//! *Substitution.* Three deliberate differences from the reference:
//!
//! - `$N` is **1-based** here (`$1` is the first word), as the M7 plan and
//!   ADR-0009 spell it. The reference's `$N` is 0-based (`$0` is the first),
//!   and its `$ARGUMENTS[N]` indexed form is not read at all — left whole, so
//!   it stays legible rather than half-expanded.
//! - The variable is `${BINGO_SKILL_DIR}`, not `${CLAUDE_SKILL_DIR}`; the
//!   session, effort, project and plugin variables have no counterpart yet.
//! - `\$1` does not escape a placeholder. Nothing in this codebase needs it
//!   yet, and an escape that only half works is worse than none.
//!
//! Kept from the reference: `$ARGUMENTS` is the whole argument text; a named
//! argument with nothing at its position becomes empty while an indexed one is
//! left as written; a substituted value is never rescanned; and arguments no
//! placeholder asked for are appended as `ARGUMENTS: <text>` rather than
//! dropped.
//!
//! *The base directory.* The reference puts `Base directory for this skill:
//! <path>` before every body it loads, which is why a skill written for it
//! says `scripts/check.sh` and means the file beside its own `SKILL.md`.
//! [`expand`] writes the same line, so such a skill works here unchanged and
//! `${BINGO_SKILL_DIR}` stays for a body that wants the path in its own
//! sentence. A bundled skill has no directory and gets no line.

mod bundled;
mod command;
mod contributor;
mod expand;
mod frontmatter;
mod layers;
mod library;
mod listing;
mod scan;
mod skill;
mod tool;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ContextContributor, Contribution, Plugin, PluginError, PluginManifest, Registrar, Tool,
};

pub use command::{SkillCommand, SkillCommands};
pub use contributor::SkillsContributor;
pub use expand::expand;
pub use library::Library;
pub use skill::Skill;
pub use tool::{SkillArgs, SkillTool};

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.skills",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["command:skills", "tool:Skill", "context:skills"],
    requires: &[],
    config: None,
};

/// Registers the command source, the `Skill` tool and the prompt line, all
/// reading one library.
#[derive(Debug, Default, Clone, Copy)]
pub struct SkillsPlugin;

#[async_trait]
impl Plugin for SkillsPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    /// Registration does no I/O: the library is built here and reads the disk
    /// the first time something asks it what exists.
    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let library = Arc::new(Library::new(registrar.env().clone()));
        registrar.add(Contribution::Commands(Arc::new(SkillCommands::new(
            Arc::clone(&library),
        ))));
        registrar.tool(Arc::new(SkillTool::new(Arc::clone(&library))) as Arc<dyn Tool>);
        registrar.add(Contribution::Context(
            Arc::new(SkillsContributor::new(library)) as Arc<dyn ContextContributor>,
        ));
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use bingo_sdk::Env;

    #[test]
    fn the_manifest_says_what_it_provides() {
        assert_eq!(MANIFEST.id, "bingo.skills");
        assert_eq!(
            MANIFEST.provides,
            ["command:skills", "tool:Skill", "context:skills"]
        );
        assert!(MANIFEST.requires.is_empty());
        assert!(MANIFEST.config.is_none(), "skills are files, not settings");
    }

    #[test]
    fn registering_reads_nothing_and_contributes_three_things() {
        let mut registrar = Registrar::new(
            "bingo.skills",
            serde_json::Value::Null,
            Env::rooted("/nowhere/at/all"),
        );
        SkillsPlugin.register(&mut registrar).expect("register");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), 3);
        assert!(matches!(contributions[0], Contribution::Commands(_)));
        assert!(matches!(contributions[1], Contribution::Tool(_)));
        assert!(matches!(contributions[2], Contribution::Context(_)));
    }
}

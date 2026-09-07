//! Experience (ADR-0014): procedural playbooks a project accumulates —
//! *when this happens, do this, check it worked* — as hand-editable files
//! under one directory per project, ranked back into the prompt by a
//! zero-dependency BM25.
//!
//! Facts about a project are the memory extractor's; this store keeps only
//! procedure, and the two never share a corpus or a prompt block.
//!
//! Four tools, two prompt blocks, one command:
//!
//! - `ExperienceCommit` writes a playbook down or revises one, showing the
//!   file it would write on the permission card; `ExperienceQuery` searches;
//!   `ExperienceOutcome` records what happened, with evidence; and
//!   `ExperienceForget` deletes one.
//! - A system block lists what there is, and a line after the person's turn
//!   recalls what fits it.
//! - `/experience` shows a person the same library as a table.

pub mod bm25;
mod command;
mod contributor;
mod diff;
pub mod entry;
mod frontmatter;
mod id;
mod project;
mod rank;
mod render;
pub mod store;
pub mod tools;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Command, ConfigClaim, ContextContributor, Contribution, Merge, Plugin, PluginError,
    PluginManifest, Registrar, Tool,
};
use schemars::JsonSchema;
use serde::Deserialize;

pub use command::ExperienceCommand;
pub use contributor::{IndexContributor, RecallContributor};
pub use store::Library;
pub use tools::{
    ExperienceCommitTool, ExperienceForgetTool, ExperienceOutcomeTool, ExperienceQueryTool,
};

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.experience",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &[
        "tool:ExperienceCommit",
        "tool:ExperienceQuery",
        "tool:ExperienceOutcome",
        "tool:ExperienceForget",
        "command:experience",
        "context:experience:index",
        "context:experience:recall",
    ],
    requires: &[],
    // Where the library lives is not a setting — it follows the config
    // directory, and what is in it is written by the tools. The one setting is
    // whether this project keeps playbooks at all (ADR-0014, amended
    // 2026-09-07).
    config: Some(ConfigClaim {
        keys: &[(SETTING, Merge::Replace)],
        schema,
    }),
};

/// The top-level settings key this plugin claims.
const SETTING: &str = "experience";

fn schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

/// The claimed slice, as the kernel hands it over.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub experience: Experience,
}

/// A typo here would silently leave the library off when a person meant it
/// on, so an unknown key is a startup failure rather than a silence.
#[derive(Clone, Copy, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Experience {
    /// Whether this project keeps playbooks at all. Off by default: four tool
    /// descriptions and a prompt block ride every request, and a project that
    /// has written no playbook pays for all of it to be told it has none.
    #[serde(default)]
    pub enabled: bool,
}

/// Registers the four tools, the two prompt blocks and `/experience`, all
/// over one library rooted in the config directory — and, where a person has
/// not asked for playbooks, nothing at all.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExperiencePlugin;

#[async_trait]
impl Plugin for ExperiencePlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        if !settings.experience.enabled {
            return Ok(());
        }
        let library = Arc::new(Library::new(&registrar.env().config_dir));
        registrar.tool(Arc::new(ExperienceCommitTool::new(library.clone())) as Arc<dyn Tool>);
        registrar.tool(Arc::new(ExperienceQueryTool::new(library.clone())) as Arc<dyn Tool>);
        registrar.tool(Arc::new(ExperienceOutcomeTool::new(library.clone())) as Arc<dyn Tool>);
        registrar.tool(Arc::new(ExperienceForgetTool::new(library.clone())) as Arc<dyn Tool>);
        registrar.add(Contribution::Command(
            Arc::new(ExperienceCommand::new(library.clone())) as Arc<dyn Command>,
        ));
        registrar.add(Contribution::Context(
            Arc::new(IndexContributor::new(library.clone())) as Arc<dyn ContextContributor>,
        ));
        registrar.add(Contribution::Context(
            Arc::new(RecallContributor::new(library)) as Arc<dyn ContextContributor>,
        ));
        Ok(())
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod plugin_tests {
    use super::*;
    use bingo_sdk::Env;
    use serde_json::json;

    /// The slice a plugin is handed when a person has asked for playbooks.
    fn registrar(slice: serde_json::Value) -> Registrar {
        Registrar::new("bingo.experience", slice, Env::rooted("/nowhere"))
    }

    #[test]
    fn the_manifest_says_what_it_provides_and_claims_one_setting() {
        assert_eq!(MANIFEST.id, "bingo.experience");
        assert!(MANIFEST.requires.is_empty());
        assert_eq!(
            MANIFEST.config.map(|claim| claim.keys),
            Some(&[("experience", Merge::Replace)][..])
        );
        assert!(
            !Experience::default().enabled,
            "playbooks are off until asked for"
        );
    }

    /// The one setting: what a person turns on, and what a typo does.
    #[test]
    fn the_settings_slice_says_whether_this_project_keeps_playbooks() {
        let read = |slice| serde_json::from_value::<Settings>(slice);
        assert!(!read(json!({})).expect("an empty slice").experience.enabled);
        assert!(
            read(json!({"experience": {"enabled": true}}))
                .expect("a slice")
                .experience
                .enabled
        );
        assert!(
            read(json!({"experience": {"enable": true}})).is_err(),
            "a typo leaves the library off silently unless it is refused"
        );
    }

    /// Nothing registered is nothing in the prompt: no tool description, no
    /// index block, and no `/experience` for a person who has not asked.
    #[test]
    fn a_project_that_did_not_ask_for_playbooks_is_offered_none() {
        let mut registrar = registrar(json!({}));
        ExperiencePlugin
            .register(&mut registrar)
            .expect("registering does no i/o");
        assert!(registrar.into_contributions().is_empty());
    }

    #[test]
    fn registering_reads_nothing_and_contributes_what_the_manifest_promises() {
        let mut registrar = registrar(json!({"experience": {"enabled": true}}));
        ExperiencePlugin
            .register(&mut registrar)
            .expect("registering does no i/o");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), MANIFEST.provides.len());
        let tools: Vec<String> = contributions
            .iter()
            .filter_map(|c| match c {
                Contribution::Tool(tool) => Some(tool.spec().name),
                _ => None,
            })
            .collect();
        assert_eq!(
            tools,
            [
                "ExperienceCommit",
                "ExperienceQuery",
                "ExperienceOutcome",
                "ExperienceForget"
            ]
        );
        assert!(matches!(contributions[4], Contribution::Command(_)));
        assert!(matches!(contributions[5], Contribution::Context(_)));
        assert!(matches!(contributions[6], Contribution::Context(_)));
    }
}

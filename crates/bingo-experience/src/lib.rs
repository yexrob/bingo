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
    Command, ContextContributor, Contribution, Plugin, PluginError, PluginManifest, Registrar, Tool,
};

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
    // The library is a directory, not a setting: where it lives follows the
    // config directory, and what is in it is written by the tools.
    config: None,
};

/// Registers the four tools, the two prompt blocks and `/experience`, all
/// over one library rooted in the config directory.
#[derive(Debug, Default, Clone, Copy)]
pub struct ExperiencePlugin;

#[async_trait]
impl Plugin for ExperiencePlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
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

    #[test]
    fn the_manifest_says_what_it_provides_and_claims_no_settings() {
        assert_eq!(MANIFEST.id, "bingo.experience");
        assert!(MANIFEST.requires.is_empty());
        assert!(MANIFEST.config.is_none());
    }

    #[test]
    fn registering_reads_nothing_and_contributes_what_the_manifest_promises() {
        let mut registrar = Registrar::new(
            "bingo.experience",
            serde_json::Value::Null,
            Env::rooted("/nowhere"),
        );
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

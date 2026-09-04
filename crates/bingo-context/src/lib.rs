//! Context: what the model is told about the project, and what happens when
//! the conversation outgrows the window.
//!
//! The kernel owns the ruler — the thresholds, the acceptance rule and the
//! breaker — and this plugin owns the strategy: what a summary says, which
//! files reach the prompt, and what a working turn leaves behind (ADR-0006).

mod compact;
mod estimate;
mod files;
mod hook;
mod instructions;
mod memory;
mod prompt;
mod root;
mod split;
mod stream;
mod tail;
mod transcript;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod git;
#[cfg(test)]
mod query;
#[cfg(test)]
mod scripted;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    Command, ConfigClaim, ContextContributor, Contribution, Hook, Merge, Plugin, PluginError,
    PluginManifest, Registrar,
};
use schemars::JsonSchema;
use serde::Deserialize;

pub use compact::SummaryCompactor;
pub use hook::MemoryHook;
pub use instructions::InstructionsContributor;
pub use memory::{MemoryCommand, MemoryContributor};

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.context",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &[
        "compactor:summary",
        "context:instructions",
        "context:memory",
        "command:memory",
    ],
    requires: &[],
    config: Some(ConfigClaim {
        keys: &[("context", Merge::Replace)],
        schema,
    }),
};

fn schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

/// The claimed slice, as the kernel hands it over.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub context: Context,
}

/// A typo here would silently turn memory off, so an unknown key is a startup
/// failure rather than a silence.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Context {
    /// Whether a working turn is asked what it learned. The memory that was
    /// already written still reaches the prompt.
    #[serde(default = "on")]
    pub memory: bool,
}

impl Default for Context {
    fn default() -> Self {
        Self { memory: on() }
    }
}

fn on() -> bool {
    true
}

/// Registers the summary compactor, the instruction files, the two memory
/// scopes, the command that lists them and the hook that writes to them.
#[derive(Debug, Default, Clone, Copy)]
pub struct ContextPlugin;

#[async_trait]
impl Plugin for ContextPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        let config_dir = registrar.env().config_dir.clone();
        let data_dir: PathBuf = registrar.env().data_dir.clone();
        registrar.add(Contribution::Compactor(Arc::new(SummaryCompactor)));
        registrar.add(Contribution::Context(
            Arc::new(InstructionsContributor::new(config_dir)) as Arc<dyn ContextContributor>,
        ));
        registrar.add(Contribution::Context(
            Arc::new(MemoryContributor::new(data_dir.clone())) as Arc<dyn ContextContributor>,
        ));
        registrar.add(Contribution::Command(
            Arc::new(MemoryCommand::new(data_dir.clone())) as Arc<dyn Command>,
        ));
        if settings.context.memory {
            registrar.add(Contribution::Hook(
                Arc::new(MemoryHook::new(data_dir)) as Arc<dyn Hook>
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::Env;
    use serde_json::json;

    fn contributions(config: serde_json::Value) -> Vec<Contribution> {
        let mut registrar = Registrar::new("bingo.context", config, Env::rooted("/tmp/home"));
        ContextPlugin.register(&mut registrar).expect("register");
        registrar.into_contributions()
    }

    #[test]
    fn the_manifest_says_what_it_provides() {
        assert_eq!(MANIFEST.id, "bingo.context");
        assert_eq!(
            MANIFEST.provides,
            [
                "compactor:summary",
                "context:instructions",
                "context:memory",
                "command:memory"
            ]
        );
        let claim = MANIFEST.config.expect("a config claim");
        assert_eq!(claim.keys, [("context", Merge::Replace)]);
    }

    #[test]
    fn memory_is_on_unless_it_is_turned_off() {
        let settings: Settings = serde_json::from_value(json!({})).expect("an empty slice");
        assert!(settings.context.memory);
        let settings: Settings =
            serde_json::from_value(json!({ "context": { "memory": false } })).expect("a slice");
        assert!(!settings.context.memory);
    }

    #[test]
    fn a_misspelled_key_is_a_startup_failure_not_a_silence() {
        let slice = json!({ "context": { "memoy": false } });
        assert!(serde_json::from_value::<Settings>(slice).is_err());
    }

    #[test]
    fn the_plugin_registers_a_compactor_two_contributors_the_command_and_the_hook() {
        let contributions = contributions(json!({}));
        assert_eq!(contributions.len(), 5);
        assert!(matches!(contributions[0], Contribution::Compactor(_)));
        assert!(matches!(contributions[1], Contribution::Context(_)));
        assert!(matches!(contributions[2], Contribution::Context(_)));
        assert!(matches!(contributions[3], Contribution::Command(_)));
        assert!(matches!(contributions[4], Contribution::Hook(_)));
    }

    #[test]
    fn memory_turned_off_registers_no_hook_and_still_contributes() {
        let contributions = contributions(json!({ "context": { "memory": false } }));
        assert_eq!(contributions.len(), 4);
        assert!(
            !contributions
                .iter()
                .any(|c| matches!(c, Contribution::Hook(_)))
        );
    }
}

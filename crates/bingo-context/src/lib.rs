//! Context: what the model is told about the project, and what happens when
//! the conversation outgrows the window.
//!
//! The kernel owns the ruler — the thresholds, the acceptance rule and the
//! breaker — and this plugin owns the strategy: what a summary says, which
//! files reach the prompt, and where the model keeps what it remembers
//! (ADR-0006, ADR-0044, ADR-0049).

mod baseline;
mod compact;
mod estimate;
mod files;
mod instructions;
mod memory;
mod prompt;
mod recap;
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

/// A typo here would silently turn the recap off, so an unknown key is a
/// startup failure rather than a silence.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Context {
    /// Whether a long turn is asked, once it is over, for a recap the TUI
    /// draws under the worked row (M84).
    #[serde(default = "on")]
    pub recap: bool,
}

impl Default for Context {
    fn default() -> Self {
        Self { recap: on() }
    }
}

fn on() -> bool {
    true
}

/// Registers the summary compactor, the instruction files, the two memory
/// scopes, the command that lists them, and the recap. Nothing writes a
/// memory but the model, with the tools it already has (ADR-0049).
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
        registrar.add(Contribution::Hook(Arc::new(baseline::BaselineHook::new(
            data_dir,
        ))));
        if settings.context.recap {
            registrar.add(Contribution::Hook(
                Arc::new(recap::RecapHook) as Arc<dyn Hook>
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
    fn the_manifest_says_what_it_provides_and_claims_its_settings() {
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
    fn the_recap_is_on_unless_it_is_turned_off() {
        let settings: Settings = serde_json::from_value(json!({})).expect("an empty slice");
        assert!(settings.context.recap);
        let settings: Settings =
            serde_json::from_value(json!({ "context": { "recap": false } })).expect("a slice");
        assert!(!settings.context.recap);
    }

    /// The switch the extractor had (ADR-0044 §5) is gone with it (ADR-0049):
    /// a settings file that still names it is a startup failure, as any
    /// unknown key here is.
    #[test]
    fn a_misspelled_or_retired_key_is_a_startup_failure_not_a_silence() {
        for slice in [
            json!({ "context": { "reacp": false } }),
            json!({ "context": { "memory": false } }),
        ] {
            assert!(serde_json::from_value::<Settings>(slice).is_err());
        }
    }

    /// The baseline hook and the recap: nothing here writes a memory.
    #[test]
    fn the_plugin_registers_a_compactor_two_contributors_the_command_and_two_hooks() {
        let contributions = contributions(json!({}));
        assert_eq!(contributions.len(), 6);
        assert!(matches!(contributions[0], Contribution::Compactor(_)));
        assert!(matches!(contributions[1], Contribution::Context(_)));
        assert!(matches!(contributions[2], Contribution::Context(_)));
        assert!(matches!(contributions[3], Contribution::Command(_)));
        let ids: Vec<&str> = contributions[4..]
            .iter()
            .map(|c| match c {
                Contribution::Hook(hook) => hook.id(),
                _ => panic!("the last two are hooks"),
            })
            .collect();
        assert_eq!(ids, ["context:baselines", "context:recap"]);
    }

    #[test]
    fn the_recap_turned_off_registers_no_hook_for_it() {
        let contributions = contributions(json!({ "context": { "recap": false } }));
        assert_eq!(contributions.len(), 5);
        let Contribution::Hook(hook) = &contributions[4] else {
            panic!("the fifth contribution is the baseline hook");
        };
        assert_eq!(hook.id(), "context:baselines");
    }
}

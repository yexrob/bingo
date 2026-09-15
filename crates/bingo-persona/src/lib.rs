//! Persona (ADR-0059): the stance bingo takes when it does not agree.
//!
//! The kernel's identity says what to do once the person has decided; it says
//! nothing about the case where the person's plan is the thing the model can
//! see is wrong. This plugin is that one paragraph — a colleague raises a
//! better path before taking it, and leaves the decision where it belongs.
//!
//! One contributor, one cacheable system block, and one setting: `persona.text`
//! is the whole block in the person's own words. Turning the stance off
//! altogether is `enabledPlugins["bingo.persona"] = false` (ADR-0057 §1).

mod judgement;

#[cfg(test)]
mod query;

use std::sync::Arc;

use async_trait::async_trait;
use bingo_sdk::{
    ConfigClaim, ContextContributor, Contribution, Merge, Plugin, PluginError, PluginManifest,
    Registrar,
};
use schemars::JsonSchema;
use serde::Deserialize;

pub use judgement::{JudgementContributor, TEXT};

pub(crate) static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.persona",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["context:persona:judgement"],
    requires: &[],
    // One block, so one key: what it says. Whether it is said at all is the
    // plugin switch's, not a second `enabled` here (ADR-0059 §1).
    config: Some(ConfigClaim {
        keys: &[(SETTING, Merge::Replace)],
        schema,
    }),
};

/// The top-level settings key this plugin claims.
const SETTING: &str = "persona";

fn schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

/// The claimed slice, as the kernel hands it over.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default)]
    pub persona: Persona,
}

/// A typo here would leave the shipped stance in force while the person
/// believes they replaced it, so an unknown key is a startup failure rather
/// than a silence.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Persona {
    /// The whole block, in the person's words. Absent, the crate's own text
    /// is used; empty, nothing is contributed at all.
    #[serde(default)]
    pub text: Option<String>,
}

/// Registers the one contributor, with the words the settings layers left.
#[derive(Debug, Default, Clone, Copy)]
pub struct PersonaPlugin;

#[async_trait]
impl Plugin for PersonaPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        let text = settings.persona.text.unwrap_or_else(|| TEXT.to_string());
        registrar.add(Contribution::Context(
            Arc::new(JudgementContributor::new(text)) as Arc<dyn ContextContributor>
        ));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Asked;
    use bingo_sdk::{ContextPiece, Env};
    use serde_json::json;

    fn registrar(slice: serde_json::Value) -> Registrar {
        Registrar::new("bingo.persona", slice, Env::rooted("/nowhere"))
    }

    /// The words the one contributor would put in a request, if any.
    async fn block(slice: serde_json::Value) -> Option<String> {
        let mut registrar = registrar(slice);
        PersonaPlugin
            .register(&mut registrar)
            .expect("registering does no i/o");
        let contributions = registrar.into_contributions();
        let Contribution::Context(contributor) = &contributions[0] else {
            panic!("the one contribution is a contributor, got {contributions:?}");
        };
        let asked = Asked::new();
        let pieces = contributor
            .contribute(asked.query())
            .await
            .expect("the stance reads nothing that can fail");
        pieces.into_iter().find_map(|piece| match piece {
            ContextPiece::System(block) => Some(block.text),
            ContextPiece::User { .. } => None,
        })
    }

    #[test]
    fn the_manifest_says_what_it_provides_and_claims_one_setting() {
        assert_eq!(MANIFEST.id, "bingo.persona");
        assert_eq!(MANIFEST.provides, ["context:persona:judgement"]);
        assert!(MANIFEST.requires.is_empty());
        assert_eq!(
            MANIFEST.config.map(|claim| claim.keys),
            Some(&[("persona", Merge::Replace)][..])
        );
    }

    #[test]
    fn registering_contributes_the_one_block_the_manifest_promises() {
        let mut registrar = registrar(json!({}));
        PersonaPlugin
            .register(&mut registrar)
            .expect("registering does no i/o");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), MANIFEST.provides.len());
        let Contribution::Context(contributor) = &contributions[0] else {
            panic!("the one contribution is a contributor, got {contributions:?}");
        };
        assert_eq!(contributor.id(), judgement::ID);
    }

    /// The three readings of the one key: unwritten, written, and written
    /// empty.
    #[tokio::test]
    async fn an_unwritten_setting_leaves_the_shipped_stance() {
        assert_eq!(block(json!({})).await.as_deref(), Some(TEXT));
        assert_eq!(block(json!({"persona": {}})).await.as_deref(), Some(TEXT));
    }

    #[tokio::test]
    async fn a_written_setting_replaces_the_whole_stance() {
        let said = block(json!({"persona": {"text": "You are Bingo the pirate."}})).await;
        assert_eq!(said.as_deref(), Some("You are Bingo the pirate."));
    }

    #[tokio::test]
    async fn an_empty_setting_leaves_the_plugin_on_and_silent() {
        assert_eq!(block(json!({"persona": {"text": ""}})).await, None);
    }

    /// A misspelled field would otherwise leave the shipped stance in force
    /// while the person believes they replaced it.
    #[test]
    fn a_typo_under_the_key_stops_the_host_and_names_the_field() {
        let error = PersonaPlugin
            .register(&mut registrar(json!({"persona": {"txet": "arr"}})))
            .expect_err("an unknown field is refused");
        assert!(matches!(error, PluginError::Config(_)), "{error}");
        assert!(error.to_string().contains("txet"), "{error}");
    }
}

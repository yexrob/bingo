//! The models.dev catalogue, read from the embedded snapshot in the shape
//! models.dev publishes, so a refreshed download parses with the same code.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::Deserialize;

/// Pruned by `scripts/models_dev.sh`; the fields below are all it keeps.
const SNAPSHOT: &str = include_str!("../../models.dev.json");

/// What the catalogue knows about one model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelFacts {
    pub context_window: u64,
    pub max_output: u64,
    pub reasoning: bool,
    /// Accepts image input.
    pub images: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct ModelCatalog {
    #[serde(flatten)]
    providers: BTreeMap<String, ProviderEntry>,
}

#[derive(Debug, Default, Deserialize)]
struct ProviderEntry {
    #[serde(default)]
    models: BTreeMap<String, Entry>,
}

#[derive(Debug, Deserialize)]
struct Entry {
    limit: Limit,
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    modalities: Modalities,
}

#[derive(Debug, Deserialize)]
struct Limit {
    context: u64,
    output: u64,
}

#[derive(Debug, Default, Deserialize)]
struct Modalities {
    #[serde(default)]
    input: Vec<String>,
}

impl Entry {
    fn facts(&self) -> ModelFacts {
        ModelFacts {
            context_window: self.limit.context,
            max_output: self.limit.output,
            reasoning: self.reasoning,
            images: self.modalities.input.iter().any(|m| m == "image"),
        }
    }
}

impl ModelCatalog {
    /// The snapshot compiled into this build. A snapshot that does not parse
    /// is an empty catalogue, which a test below turns into a build failure.
    pub fn embedded() -> &'static ModelCatalog {
        static EMBEDDED: LazyLock<ModelCatalog> =
            LazyLock::new(|| ModelCatalog::parse(SNAPSHOT).unwrap_or_default());
        &EMBEDDED
    }

    pub fn parse(json: &str) -> Result<ModelCatalog, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn is_empty(&self) -> bool {
        self.providers.values().all(|p| p.models.is_empty())
    }

    /// The provider's own entry first, exact then by family prefix (a dated
    /// snapshot resolves like its family); then the same id under any
    /// provider, for an OpenAI-compatible endpoint fronting another vendor.
    pub fn lookup(&self, provider: &str, model: &str) -> Option<ModelFacts> {
        self.in_provider(provider, model)
            .or_else(|| self.anywhere(model))
    }

    /// Every model the catalogue lists under a provider, in id order.
    pub fn models_of(&self, provider: &str) -> impl Iterator<Item = &str> {
        self.providers
            .get(provider)
            .into_iter()
            .flat_map(|p| p.models.keys().map(String::as_str))
    }

    fn in_provider(&self, provider: &str, model: &str) -> Option<ModelFacts> {
        let models = &self.providers.get(provider)?.models;
        exact(models, model).or_else(|| by_prefix(models, model))
    }

    fn anywhere(&self, model: &str) -> Option<ModelFacts> {
        self.providers
            .values()
            .find_map(|p| exact(&p.models, model))
    }
}

fn exact(models: &BTreeMap<String, Entry>, model: &str) -> Option<ModelFacts> {
    models.get(model).map(Entry::facts)
}

/// The longest catalogue id that is a prefix of `model` at a separator, so
/// `gpt-5.4-2026-03-05` reads as `gpt-5.4` and never as `gpt-5`.
fn by_prefix(models: &BTreeMap<String, Entry>, model: &str) -> Option<ModelFacts> {
    models
        .iter()
        .filter(|(id, _)| is_family_prefix(id, model))
        .max_by_key(|(id, _)| id.len())
        .map(|(_, entry)| entry.facts())
}

fn is_family_prefix(id: &str, model: &str) -> bool {
    model
        .strip_prefix(id)
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| matches!(c, '-' | '@' | ':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> ModelCatalog {
        ModelCatalog::parse(
            r#"{
              "openai": { "models": {
                "gpt-5":   { "limit": {"context": 400000, "output": 128000}, "reasoning": true, "modalities": {"input": ["text", "image"]} },
                "gpt-5.4": { "limit": {"context": 1050000, "output": 128000}, "reasoning": true, "modalities": {"input": ["text", "image"]} }
              }},
              "deepseek": { "models": {
                "deepseek-v4-pro": { "limit": {"context": 1000000, "output": 384000}, "reasoning": true, "modalities": {"input": ["text"]} }
              }}
            }"#,
        )
        .expect("a readable fixture")
    }

    #[test]
    fn an_exact_id_under_its_provider_wins() {
        let facts = fixture().lookup("openai", "gpt-5.4").expect("known");
        assert_eq!(facts.context_window, 1_050_000);
        assert!(facts.images && facts.reasoning);
    }

    #[test]
    fn a_dated_snapshot_reads_as_the_longest_family_at_a_separator() {
        let catalog = fixture();
        let dated = catalog
            .lookup("openai", "gpt-5.4-2026-03-05")
            .expect("family");
        assert_eq!(dated.context_window, 1_050_000, "gpt-5.4, not gpt-5");
        assert_eq!(
            catalog
                .lookup("openai", "gpt-5-2025-08-07")
                .map(|f| f.context_window),
            Some(400_000)
        );
        assert_eq!(
            catalog
                .lookup("openai", "gpt-5.4x")
                .map(|f| f.context_window),
            None,
            "a prefix without a separator is another model"
        );
    }

    #[test]
    fn a_proxied_vendor_model_is_found_under_any_provider_by_exact_id_only() {
        let catalog = fixture();
        let facts = catalog
            .lookup("openai", "deepseek-v4-pro")
            .expect("cross-provider");
        assert_eq!(facts.max_output, 384_000);
        assert!(!facts.images);
        assert!(catalog.lookup("openai", "deepseek-v4-pro-0528").is_none());
        assert!(catalog.lookup("nobody", "unknown-model").is_none());
    }

    #[test]
    fn the_embedded_snapshot_parses_and_knows_both_first_party_providers() {
        let catalog = ModelCatalog::embedded();
        assert!(!catalog.is_empty(), "the snapshot must parse");
        let claude = catalog
            .lookup("anthropic", "claude-sonnet-4-5")
            .expect("anthropic entries");
        assert!(claude.reasoning && claude.images);
        assert!(catalog.lookup("openai", "gpt-5").is_some());
    }

    #[test]
    fn a_provider_lists_its_models_in_id_order() {
        let ids: Vec<&str> = ModelCatalog::embedded().models_of("anthropic").collect();
        assert!(ids.len() > 3, "{ids:?}");
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(ModelCatalog::embedded().models_of("nobody").count(), 0);
    }
}

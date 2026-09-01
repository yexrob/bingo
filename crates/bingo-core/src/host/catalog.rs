//! What a client may choose from: the registry read out as flat entries, one
//! reader per kind.
//!
//! The providers are handed in already resolved — the registered ones and the
//! sources' both (ADR-0030 §2) — so a plugin's provider is listed by the one
//! reader that lists every other, and its models by the one that lists theirs.

use std::sync::Arc;

use bingo_sdk::*;
use serde_json::{Map, Value, json};

use super::Registry;
use crate::models::{ModelCatalog, ModelFacts};

pub(super) async fn entries(
    registry: &Registry,
    resolved: &[Arc<dyn Provider>],
    model: Option<&str>,
    kind: CatalogKind,
) -> Vec<CatalogEntry> {
    match kind {
        CatalogKind::Models => models(resolved, model),
        CatalogKind::Providers => providers(resolved),
        CatalogKind::Tools => tools(registry).await,
        CatalogKind::Commands => commands(registry).await,
        CatalogKind::Skills => Vec::new(),
        CatalogKind::Plugins => plugins(registry),
    }
}

/// The embedded catalogue's models for each provider, plus the configured one;
/// nothing here asks a provider for its list, which would be a network call
/// (ADR-0026 §4).
fn models(resolved: &[Arc<dyn Provider>], configured: Option<&str>) -> Vec<CatalogEntry> {
    let catalogue = ModelCatalog::embedded();
    resolved
        .iter()
        .flat_map(|p| {
            let mut ids: Vec<&str> = configured.into_iter().collect();
            // Filed by family, not id: a named instance (ADR-0017) has no
            // models of its own — it serves its wire shape's.
            ids.extend(
                catalogue
                    .models_of(p.family())
                    .filter(|m| Some(*m) != configured),
            );
            ids.into_iter().map(|model| CatalogEntry {
                id: format!("{}/{model}", p.id()),
                label: model.to_string(),
                meta: model_meta(p.id(), catalogue.lookup(p.family(), model)),
            })
        })
        .collect()
}

/// What a client is told about one model: the provider that serves it, and
/// the facts the embedded catalogue holds for it (ADR-0026 §1). A model the
/// catalogue does not know — a configured override, a private endpoint's id —
/// carries the provider alone: an absent fact is never guessed.
fn model_meta(provider: &str, facts: Option<ModelFacts>) -> Value {
    let mut meta = Map::new();
    meta.insert("provider".into(), json!(provider));
    if let Some(facts) = facts {
        meta.insert("context".into(), json!(facts.context_window));
        meta.insert("output".into(), json!(facts.max_output));
        meta.insert("reasoning".into(), json!(facts.reasoning));
        meta.insert("images".into(), json!(facts.images));
    }
    Value::Object(meta)
}

fn providers(resolved: &[Arc<dyn Provider>]) -> Vec<CatalogEntry> {
    resolved
        .iter()
        .map(|p| CatalogEntry {
            id: p.id().to_string(),
            label: p.id().to_string(),
            meta: json!({ "auth": p.auth() }),
        })
        .collect()
}

/// The registered tools, then every source's (ADR-0009); a tool's own
/// `meta` rides beside its description.
async fn tools(registry: &Registry) -> Vec<CatalogEntry> {
    let mut all = registry.tools.clone();
    for source in &registry.tool_sources {
        all.extend(source.tools().await);
    }
    all.iter()
        .map(|t| {
            let spec = t.spec();
            let mut meta = spec.meta;
            meta.insert("description".into(), Value::String(spec.description));
            CatalogEntry {
                id: spec.name.clone(),
                label: spec.name,
                meta: Value::Object(meta),
            }
        })
        .collect()
}

/// The catalogue has no session, so a source is asked for the process's own
/// directory; a session working elsewhere still dispatches by its own.
async fn commands(registry: &Registry) -> Vec<CatalogEntry> {
    let here = std::env::current_dir().unwrap_or_default();
    let mut all = registry.commands.clone();
    for source in &registry.command_sources {
        all.extend(source.commands(&here).await);
    }
    all.iter()
        .map(|c| {
            let spec = c.spec();
            CatalogEntry {
                id: spec.name.clone(),
                label: spec.hint.clone(),
                meta: serde_json::to_value(spec).unwrap_or(Value::Null),
            }
        })
        .collect()
}

fn plugins(registry: &Registry) -> Vec<CatalogEntry> {
    registry
        .plugins
        .iter()
        .map(|p| CatalogEntry {
            id: p.id.clone(),
            label: format!("{} {}", p.id, p.version),
            meta: json!({ "enabled": p.enabled, "reason": p.reason }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::*;

    /// An endpoint that serves a catalogued family under a name of its own
    /// (ADR-0017), which is how a model's facts are found at all.
    struct Endpoint;

    #[async_trait]
    impl Provider for Endpoint {
        fn id(&self) -> &str {
            "house"
        }

        fn family(&self) -> &str {
            "anthropic"
        }

        fn endpoint(&self, _model: &str) -> EndpointCapabilities {
            EndpointCapabilities::default()
        }

        async fn stream(
            &self,
            _request: ModelRequest,
            _cancel: CancellationToken,
        ) -> Result<ModelStream, ProviderError> {
            unreachable!("the catalogue runs no turn")
        }
    }

    fn listed(configured: Option<&str>) -> Vec<CatalogEntry> {
        models(&[Arc::new(Endpoint) as Arc<dyn Provider>], configured)
    }

    fn entry(entries: &[CatalogEntry], id: &str) -> CatalogEntry {
        entries
            .iter()
            .find(|e| e.id == id)
            .unwrap_or_else(|| panic!("no {id} in {entries:?}"))
            .clone()
    }

    fn keys(entry: &CatalogEntry) -> Vec<&str> {
        let mut keys: Vec<&str> = entry
            .meta
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    /// The wire shape a client reads (ADR-0026 §1): these keys, these types.
    /// A rename here is a break, so it is pinned rather than described.
    #[test]
    fn a_catalogued_model_carries_the_provider_and_the_four_facts() {
        let entries = listed(None);
        let sonnet = entry(&entries, "house/claude-sonnet-4-5");
        assert_eq!(
            keys(&sonnet),
            ["context", "images", "output", "provider", "reasoning"]
        );
        assert_eq!(sonnet.meta["provider"], json!("house"));
        assert!(sonnet.meta["context"].is_u64(), "{:?}", sonnet.meta);
        assert!(sonnet.meta["output"].is_u64(), "{:?}", sonnet.meta);
        assert!(sonnet.meta["reasoning"].is_boolean(), "{:?}", sonnet.meta);
        assert!(sonnet.meta["images"].is_boolean(), "{:?}", sonnet.meta);
        assert_eq!(sonnet.label, "claude-sonnet-4-5");
    }

    /// A configured id the snapshot never heard of keeps the one fact the
    /// kernel knows. Nothing is filled in for it.
    #[test]
    fn a_model_the_catalogue_does_not_know_carries_the_provider_alone() {
        let entries = listed(Some("house-private-1"));
        let private = entry(&entries, "house/house-private-1");
        assert_eq!(keys(&private), ["provider"]);
        assert_eq!(private.meta, json!({ "provider": "house" }));
    }

    /// The same facts a lookup returns, unrenamed and unrounded.
    #[test]
    fn the_facts_are_the_catalogue_s_own() {
        let facts = ModelCatalog::embedded()
            .lookup("anthropic", "claude-sonnet-4-5")
            .expect("the snapshot knows it");
        let sonnet = entry(&listed(None), "house/claude-sonnet-4-5");
        assert_eq!(sonnet.meta["context"], json!(facts.context_window));
        assert_eq!(sonnet.meta["output"], json!(facts.max_output));
        assert_eq!(sonnet.meta["reasoning"], json!(facts.reasoning));
        assert_eq!(sonnet.meta["images"], json!(facts.images));
    }
}

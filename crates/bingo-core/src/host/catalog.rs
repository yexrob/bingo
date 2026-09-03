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
use crate::models::{ModelCatalog, ModelFacts, Offer, ServedModels, Source, served};

pub(super) async fn entries(
    registry: &Registry,
    resolved: &[Arc<dyn Provider>],
    model: Option<&str>,
    served: &ServedModels,
    kind: CatalogKind,
) -> Vec<CatalogEntry> {
    match kind {
        CatalogKind::Models => models(resolved, model, served),
        CatalogKind::Providers => providers(resolved),
        CatalogKind::Tools => tools(registry).await,
        CatalogKind::Commands => commands(registry).await,
        CatalogKind::Skills => Vec::new(),
        CatalogKind::Plugins => plugins(registry),
    }
}

/// Each provider's ids — the ones its endpoint answered with if it has ever
/// answered, else the embedded catalogue's for its family — plus the
/// configured one. Nothing here asks a provider anything: the list was
/// fetched in the background and cached (ADR-0026 §4).
fn models(
    resolved: &[Arc<dyn Provider>],
    configured: Option<&str>,
    served: &ServedModels,
) -> Vec<CatalogEntry> {
    let catalogue = ModelCatalog::embedded();
    resolved
        .iter()
        .flat_map(|p| {
            // Filed by family, not id: a named instance (ADR-0017) has no
            // models of its own — it serves its wire shape's.
            let shelf: Vec<&str> = catalogue.models_of(p.family()).collect();
            let endpoint = served.get(p.id());
            served::merge(endpoint.as_ref(), &shelf, configured)
                .into_iter()
                .map(|offer| entry(p.as_ref(), offer))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn entry(provider: &dyn Provider, offer: Offer) -> CatalogEntry {
    let facts = ModelCatalog::embedded().facts_for(provider.family(), provider.id(), &offer.id);
    CatalogEntry {
        id: format!("{}/{}", provider.id(), offer.id),
        meta: model_meta(provider.id(), offer.source, facts),
        label: offer.id,
    }
}

/// What a client is told about one model: the provider that serves it, who
/// says it exists there, and the facts the embedded catalogue holds for it
/// (ADR-0026 §1). A model the catalogue does not know — a configured
/// override, a private endpoint's id — carries no facts: an absent fact is
/// never guessed.
fn model_meta(provider: &str, source: Source, facts: Option<ModelFacts>) -> Value {
    let mut meta = Map::new();
    meta.insert("provider".into(), json!(provider));
    meta.insert("source".into(), json!(source.as_str()));
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
/// `meta` rides beside the facts the registry already holds about it.
async fn tools(registry: &Registry) -> Vec<CatalogEntry> {
    let mut all = registry.tools.clone();
    for source in &registry.sources.tools {
        all.extend(source.tools().await);
    }
    all.iter().map(|t| tool_entry(t.as_ref())).collect()
}

/// One tool as a client reads it: what it does, the schema a caller fills in,
/// and the traits the gate reads — the facts the registry already holds, so a
/// client that has to describe a tool to someone else copies them rather than
/// keeping a description of its own.
///
/// The traits are asked with no input, because a catalogue has no call: a
/// tool that sharpens them per call (a fetch of a documentation page is
/// read-only, of anything else is not) shows its baseline here.
fn tool_entry(tool: &dyn Tool) -> CatalogEntry {
    let spec = tool.spec();
    let mut meta = spec.meta;
    meta.insert("description".into(), Value::String(spec.description));
    meta.insert("inputSchema".into(), spec.input_schema);
    meta.insert(
        "traits".into(),
        serde_json::to_value(tool.traits(&Value::Null)).unwrap_or(Value::Null),
    );
    CatalogEntry {
        id: spec.name.clone(),
        label: spec.name,
        meta: Value::Object(meta),
    }
}

/// The catalogue has no session, so a source is asked for the process's own
/// directory; a session working elsewhere still dispatches by its own.
async fn commands(registry: &Registry) -> Vec<CatalogEntry> {
    let here = std::env::current_dir().unwrap_or_default();
    let mut all = registry.commands.clone();
    for source in &registry.sources.commands {
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
        listed_with(configured, &ServedModels::default())
    }

    fn listed_with(configured: Option<&str>, served: &ServedModels) -> Vec<CatalogEntry> {
        models(
            &[Arc::new(Endpoint) as Arc<dyn Provider>],
            configured,
            served,
        )
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
            [
                "context",
                "images",
                "output",
                "provider",
                "reasoning",
                "source"
            ]
        );
        assert_eq!(sonnet.meta["provider"], json!("house"));
        assert_eq!(sonnet.meta["source"], json!("catalogue"));
        assert!(sonnet.meta["context"].is_u64(), "{:?}", sonnet.meta);
        assert!(sonnet.meta["output"].is_u64(), "{:?}", sonnet.meta);
        assert!(sonnet.meta["reasoning"].is_boolean(), "{:?}", sonnet.meta);
        assert!(sonnet.meta["images"].is_boolean(), "{:?}", sonnet.meta);
        assert_eq!(sonnet.label, "claude-sonnet-4-5");
    }

    /// A configured id the snapshot never heard of keeps the two facts the
    /// kernel knows. Nothing is filled in for it.
    #[test]
    fn a_model_the_catalogue_does_not_know_carries_the_provider_alone() {
        let entries = listed(Some("house-private-1"));
        let private = entry(&entries, "house/house-private-1");
        assert_eq!(keys(&private), ["provider", "source"]);
        assert_eq!(
            private.meta,
            json!({ "provider": "house", "source": "configured" })
        );
    }

    /// Once the endpoint has answered, its ids are the list — the snapshot's
    /// are not offered beside them — and each one still carries whatever the
    /// snapshot knows about it, wherever that model is filed.
    #[test]
    fn an_endpoint_that_has_answered_says_which_ids_exist_there() {
        let served = ServedModels::default();
        served.record(
            "house",
            vec![
                ModelInfo {
                    id: "claude-sonnet-4-5".into(),
                    display: None,
                },
                ModelInfo {
                    id: "deepseek-v4-pro".into(),
                    display: None,
                },
            ],
            jiff::Timestamp::UNIX_EPOCH,
        );
        let entries = listed_with(None, &served);
        assert_eq!(
            entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["house/claude-sonnet-4-5", "house/deepseek-v4-pro"],
            "the endpoint's list, and only it"
        );
        let proxied = entry(&entries, "house/deepseek-v4-pro");
        assert_eq!(proxied.meta["source"], json!("endpoint"));
        assert_eq!(
            proxied.meta["output"],
            json!(
                ModelCatalog::embedded()
                    .lookup("deepseek", "deepseek-v4-pro")
                    .expect("the snapshot knows it")
                    .max_output
            ),
            "a model the endpoint fronts keeps its maker's facts"
        );
    }

    /// A tool that is nothing but a name, a schema and its traits.
    struct Declared {
        name: &'static str,
        traits: ToolTraits,
    }

    #[async_trait]
    impl Tool for Declared {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name.into(),
                description: "what it does".into(),
                input_schema: json!({"type": "object", "properties": {"v": {"type": "string"}}}),
                meta: Default::default(),
            }
        }

        fn traits(&self, _input: &Value) -> ToolTraits {
            self.traits
        }

        async fn call(&self, _input: Value, _cx: &ToolContext) -> Result<ToolOutput, ToolError> {
            unreachable!("the catalogue runs no call")
        }
    }

    fn declared(name: &'static str, traits: ToolTraits) -> Arc<dyn Tool> {
        Arc::new(Declared { name, traits })
    }

    /// What a client reads a tool as: the schema a caller fills in and the
    /// traits the gate reads ride beside the description, so nothing
    /// downstream has to keep a second description of what a tool is.
    #[tokio::test]
    async fn a_tool_entry_carries_its_schema_and_its_traits() {
        let registry = Registry {
            tools: vec![
                declared("Reads", ToolTraits::read_only()),
                declared("Writes", ToolTraits::edit()),
            ],
            ..Registry::default()
        };
        let entries = tools(&registry).await;

        let reads = entry(&entries, "Reads");
        assert_eq!(keys(&reads), ["description", "inputSchema", "traits"]);
        assert_eq!(reads.meta["description"], json!("what it does"));
        assert_eq!(reads.meta["inputSchema"]["type"], json!("object"));
        assert_eq!(
            reads.meta["traits"],
            json!({
                "concurrencySafe": true,
                "readOnly": true,
                "destructive": false,
                "edit": false,
                "resultLimit": "global",
                "trusted": true,
            }),
            "every trait the gate reads, spelled as the wire spells it"
        );

        let writes = entry(&entries, "Writes");
        assert_eq!(writes.meta["traits"]["edit"], json!(true));
        assert_eq!(writes.meta["traits"]["readOnly"], json!(false));
        assert_eq!(
            writes.meta["inputSchema"], reads.meta["inputSchema"],
            "every tool is described as fully, whatever it does"
        );
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

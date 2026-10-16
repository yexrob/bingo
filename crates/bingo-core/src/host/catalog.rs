//! What a client may choose from: the registry read out as flat entries, one
//! reader per kind.
//!
//! The providers are handed in already resolved — the registered ones and the
//! sources' both (ADR-0030 §2) — so a plugin's provider is listed by the one
//! reader that lists every other, and its models by the one that lists theirs.

pub(super) mod limits;

use std::sync::Arc;

use bingo_sdk::*;
use serde_json::{Map, Value, json};

use super::Registry;
use crate::models::{ModelCatalog, ModelFacts, Offer, ServedModels, Source, served};

pub(super) async fn model_granted(host: &super::Host, plugin: &str) -> bool {
    let status = crate::plugins::listing(&host.registry)
        .await
        .into_iter()
        .find(|status| status.id == plugin);
    match status.and_then(|status| status.grant.map(|grant| (status.state.is_on(), grant))) {
        Some((enabled, grant)) => {
            enabled && grant.tier != Tier::Tighten && grant.uses.contains("model")
        }
        None => host
            .settings
            .kernel
            .plugin_trust
            .get(plugin)
            .is_some_and(|grant| grant.uses.iter().any(|noun| noun == "model")),
    }
}

pub(super) async fn entries(
    registry: &Registry,
    resolved: &[Arc<dyn Provider>],
    model: Option<&str>,
    served: &ServedModels,
    kind: CatalogKind,
    cwd: &std::path::Path,
) -> Vec<CatalogEntry> {
    let mut entries = match kind {
        CatalogKind::Events => events(registry),
        CatalogKind::Models => models(resolved, model, served),
        CatalogKind::Providers => providers(registry, resolved),
        CatalogKind::Tools => tools(registry).await,
        CatalogKind::Commands => commands(registry, cwd).await,
        CatalogKind::Plugins => plugins(registry).await,
    };
    for agent in &registry.agents {
        match kind {
            CatalogKind::Providers => entries.push(CatalogEntry {
                id: agent.id().into(),
                label: agent.id().into(),
                meta: json!({"auth": AuthStatus::NotApplicable, "shape":null, "address":null, "instance":false, "reach":registry.provider_state.reach.get(agent.id())}),
            }),
            CatalogKind::Models => {
                for offer in served::merge(served.get(agent.id()).as_ref(), &[], model) {
                    entries.push(CatalogEntry {
                        id: format!("{}/{}", agent.id(), offer.id),
                        label: offer.id,
                        meta: json!({"provider": agent.id(), "source": offer.source.as_str(), "chat": true}),
                    });
                }
            }
            _ => {}
        }
    }
    entries
}

fn events(registry: &Registry) -> Vec<CatalogEntry> {
    let mut entries = catalogue::document()["events"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    for key in registry.services.keys() {
        if let Some(face) = registry.services.face(&key) {
            for method in face.methods {
                let entry = method.entry;
                entries.insert(entry.name.to_string(),json!({"kind":entry.kind,"input":entry.input.schema(),"result":entry.result.schema(),"matcher":entry.matcher,"pinned":entry.pinned,"acts":entry.acts,"unsafe":method.unsafe_call}));
            }
        }
    }
    entries
        .into_iter()
        .map(|(id, meta)| CatalogEntry {
            label: id.clone(),
            id,
            meta,
        })
        .collect()
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
            let offers = served::merge(endpoint.as_ref(), &shelf, configured);
            offers
                .iter()
                .map(|offer| entry(p.as_ref(), offer, &offers))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn entry(provider: &dyn Provider, offer: &Offer, listed: &[Offer]) -> CatalogEntry {
    let facts = ModelCatalog::embedded().facts_for(provider.family(), provider.id(), &offer.id);
    let mut meta = model_meta(provider.id(), offer.source, facts);
    if let Some(facts) = facts {
        meta["efforts"] = json!(if facts.reasoning {
            provider.efforts(&offer.id)
        } else {
            vec![]
        });
        meta["pinned"] = json!(false);
        if let Some(alias) =
            crate::models::alias_of(&offer.id, listed.iter().map(|offer| offer.id.as_str()))
        {
            meta["aliasOf"] = json!(alias);
        }
    }
    CatalogEntry {
        id: format!("{}/{}", provider.id(), offer.id),
        meta,
        label: offer.id.clone(),
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
    meta.insert("chat".into(), json!(facts.is_none_or(|facts| facts.chat)));
    if let Some(facts) = facts {
        if let Some(price) = facts.price {
            meta.insert("price".into(), json!(price));
        }
        meta.insert("context".into(), json!(facts.context_window));
        meta.insert("output".into(), json!(facts.max_output));
        meta.insert("reasoning".into(), json!(facts.reasoning));
        meta.insert("images".into(), json!(facts.images));
    }
    Value::Object(meta)
}

fn providers(registry: &Registry, resolved: &[Arc<dyn Provider>]) -> Vec<CatalogEntry> {
    resolved
        .iter()
        .map(|p| CatalogEntry {
            id: p.id().to_string(),
            label: p.id().to_string(),
            meta: json!({ "auth": p.auth(), "shape": p.shape(),
                "address": p.address().and_then(super::instances::address),
                "instance": p.instance(), "reach": registry.provider_state.reach.get(p.id()) }),
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

async fn commands(registry: &Registry, cwd: &std::path::Path) -> Vec<CatalogEntry> {
    let mut all = Vec::new();
    for source in registry.command_sources() {
        all.extend(source.commands(cwd).await);
    }
    all.iter()
        .map(|command| command.spec())
        .filter(|spec| !spec.hidden)
        .map(|spec| CatalogEntry {
            id: spec.name.clone(),
            label: spec.hint.clone(),
            meta: serde_json::to_value(spec).unwrap_or(Value::Null),
        })
        .collect()
}

/// Description links rewrite specs, so derived catalogue facts are applied last.
pub(super) fn command_folds(registry: &Registry, entries: &mut [CatalogEntry]) {
    let available: std::collections::BTreeSet<_> =
        entries.iter().map(|entry| entry.id.clone()).collect();
    for entry in entries {
        let Some(meta) = entry.meta.as_object_mut() else {
            continue;
        };
        meta.remove("foldedInto");
        let Some(name) = meta.get("name").and_then(Value::as_str) else {
            continue;
        };
        let aliases = match meta.get("aliases") {
            None => &[][..],
            Some(Value::Array(aliases)) if aliases.iter().all(Value::is_string) => {
                aliases.as_slice()
            }
            _ => continue,
        };
        if let Some(into) = registry.folded_into(name, aliases.iter().filter_map(Value::as_str))
            && available.contains(into)
        {
            meta.insert("foldedInto".into(), json!(into));
        }
    }
}

/// What this build registered, then what every source found (ADR-0057 §5):
/// the name a person completes is the name `/plugins` lists (ADR-0008 §6a).
async fn plugins(registry: &Registry) -> Vec<CatalogEntry> {
    crate::plugins::listing(registry)
        .await
        .iter()
        .map(plugin_entry)
        .collect()
}

/// One plugin as a client reads it: whether it runs, why not, and which
/// listing it came from — the row `/plugins` draws, in a catalogue's shape.
pub(super) fn plugin_entry(status: &PluginStatus) -> CatalogEntry {
    let mut meta = json!({ "state": status.state, "from": status.from, "version": status.version });
    for part in [json!(status.placement), json!(status.grant)] {
        if let (Value::Object(fields), Some(meta)) = (part, meta.as_object_mut()) {
            meta.extend(fields);
        }
    }
    CatalogEntry {
        id: status.id.clone(),
        label: format!("{} {}", status.id, status.version),
        meta,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::super::registry::Sources;
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
                "chat",
                "context",
                "efforts",
                "images",
                "output",
                "pinned",
                "price",
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
        assert_eq!(keys(&private), ["chat", "provider", "source"]);
        assert_eq!(
            private.meta,
            json!({ "provider": "house", "source": "configured", "chat": true })
        );
    }

    #[test]
    fn a_dated_offer_names_only_a_twin_in_the_same_provider_list() {
        let served = ServedModels::default();
        served.record(
            "house",
            vec![
                ModelInfo {
                    id: "claude-sonnet-4-5".into(),
                    display: None,
                },
                ModelInfo {
                    id: "claude-sonnet-4-5-20250929".into(),
                    display: None,
                },
            ],
            jiff::Timestamp::UNIX_EPOCH,
        );
        let entries = listed_with(None, &served);
        assert_eq!(
            entry(&entries, "house/claude-sonnet-4-5-20250929").meta["aliasOf"],
            "claude-sonnet-4-5"
        );
        let model = entry(&entries, "house/claude-sonnet-4-5");
        assert!(model.meta.get("aliasOf").is_none());
        assert!(model.meta["price"]["input"].is_number());
        assert_eq!(model.meta["efforts"], json!(Effort::ALL));
        assert_eq!(model.meta["chat"], true);
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
                "quiet": false,
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

    /// A bridge that found a plugin of its own on the machine.
    struct Found;

    #[async_trait]
    impl PluginSource for Found {
        fn id(&self) -> &str {
            "bridge"
        }

        async fn plugins(&self) -> Vec<PluginStatus> {
            vec![PluginStatus {
                id: "wordcount".into(),
                version: "0.2.0".into(),
                state: bingo_sdk::PluginState::Off {
                    why: "switched off in the settings".into(),
                },
                placement: None,
                grant: None,
                from: "bridge".into(),
            }]
        }
    }

    fn registered(id: &str) -> PluginStatus {
        PluginStatus {
            id: id.into(),
            version: "1.0.0".into(),
            state: bingo_sdk::PluginState::On,
            placement: None,
            grant: None,
            from: BUILT_IN.into(),
        }
    }

    /// ADR-0008 §6a: a person completes the name `/plugins` lists, so the
    /// catalogue is that same listing — this build's, then every source's,
    /// in that order.
    /// Where a plugin came from is said once, by its listing: a repository's
    /// plugin still waiting for a yes is placed before any grant exists.
    #[test]
    fn a_plugin_entry_places_a_waiting_plugin_from_its_status_alone() {
        let waiting = PluginStatus {
            id: "plugins:p".into(),
            version: "1".into(),
            state: PluginState::Waiting { changed: false },
            from: "bingo.mods".into(),
            placement: Some(PluginPlacement {
                origin: PluginOrigin::Project,
                runtime: PluginRuntime::Module,
            }),
            grant: None,
        };
        let entry = plugin_entry(&waiting);
        assert_eq!(entry.meta["origin"], "project");
        assert_eq!(entry.meta["runtime"], "module");
        assert!(entry.meta.get("tier").is_none());
        let granted = PluginStatus {
            grant: Some(PluginGrant {
                tier: Tier::Rewrite,
                uses: Default::default(),
            }),
            ..waiting
        };
        assert_eq!(plugin_entry(&granted).meta["tier"], "rewrite");
    }

    #[tokio::test]
    async fn the_plugins_catalogue_lists_the_sources_after_the_registry() {
        let registry = Registry {
            plugins: vec![registered("bingo.tools.web")],
            switches: crate::host::registry::Switches {
                wishes: std::sync::RwLock::new(["wordcount".into()].into()),
                ..Default::default()
            },
            sources: Sources {
                plugins: vec![Arc::new(Found)],
                ..Default::default()
            },
            ..Registry::default()
        };
        let entries = plugins(&registry).await;
        assert_eq!(
            entries.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(),
            ["bingo.tools.web", "wordcount"]
        );
        let web = entry(&entries, "bingo.tools.web");
        assert_eq!(web.label, "bingo.tools.web 1.0.0");
        assert_eq!(
            web.meta,
            json!({ "state": {"kind":"on"}, "from": BUILT_IN, "version":"1.0.0" })
        );
        assert_eq!(
            entry(&entries, "wordcount").meta,
            json!({
                "state": {"kind":"off", "why":"switched off in the settings"},
                "from": "bridge", "version":"0.2.0",
            }),
            "a plugin only a source has seen says where it came from"
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

#[cfg(test)]
mod event_contract {
    use super::*;
    struct Face;
    #[async_trait::async_trait]
    impl WireService for Face {
        fn face(&self) -> Option<bingo_sdk::Face> {
            let mut face = bingo_sdk::Face::new("kit");
            face.methods.push(service::Method {
                entry: catalogue::Entry::face(
                    "kit.echo".into(),
                    schemars::schema_for!(String),
                    schemars::schema_for!(String),
                ),
                unsafe_call: false,
            });
            Some(face)
        }
        async fn call(&self, _: &str, _: Value) -> Result<Value, ServiceError> {
            Err(ServiceError("catalogue must not execute a method".into()))
        }
    }
    #[tokio::test]
    async fn event_catalogue_includes_kernel_and_late_service_schemas() {
        let home = tempfile::tempdir().unwrap();
        let host = crate::Host::build(vec![], crate::HostConfig::new(Env::rooted(home.path())))
            .await
            .unwrap();
        host.open_service("kit", Arc::new(Face)).unwrap();
        let kind = serde_json::from_value(json!("events")).expect("event catalogue wire kind");
        let result = host.catalog(kind).await.unwrap();
        assert!(result.entries.iter().any(|entry| entry.id == "ui.render"
            && entry.meta["input"]["properties"]["component"].is_object()));
        let echo = result
            .entries
            .iter()
            .find(|entry| entry.id == "kit.echo")
            .unwrap();
        assert_eq!(echo.meta["input"]["type"], "string");
        assert_eq!(echo.meta["result"]["type"], "string");
        host.shutdown().await;
    }
}

pub(super) struct Scoped(pub std::sync::Weak<super::Host>);
impl command::CommandCatalog for Scoped {
    fn catalog(&self, mut catalog: Catalog, view: &command::SessionView<'_>) -> Catalog {
        let Some(host) = self.0.upgrade() else {
            return catalog;
        };
        let commands = host
            .registry
            .commands
            .entries
            .iter()
            .map(|command| (command.spec().name, command))
            .collect::<std::collections::HashMap<_, _>>();
        for entry in &mut catalog.entries {
            if let Some(command) = commands.get(&entry.id) {
                entry.meta["now"] = json!(command.now(view));
                if let Some(offered) = command.offered(view) {
                    entry.meta["offered"] = json!(offered);
                }
            }
        }
        catalog
    }
}

#[cfg(test)]
mod current_values {
    use super::*;
    #[tokio::test]
    async fn current_values_use_the_supplied_session_without_host_calls() {
        let home = tempfile::tempdir().unwrap();
        let host = crate::Host::build(vec![], crate::HostConfig::new(Env::rooted(home.path())))
            .await
            .unwrap();
        let attachment = host
            .open(
                SessionSelector::Create {
                    spec: SessionSpec {
                        cwd: home.path().into(),
                        driver: Driver::Log,
                        ..Default::default()
                    },
                },
                ClientIdentity {
                    name: "fixture".into(),
                    surface: "terminal".into(),
                },
                OpenOptions::default(),
            )
            .await
            .unwrap();
        let mut state = attachment.snapshot;
        state.summary.title = Some("A title".into());
        state.summary.provider = Some("fake".into());
        state.summary.model = Some("large".into());
        state.config.kernel["thinking"] = json!("xHigh");
        state.context = Some(ContextUsage {
            parts: None,
            used: 123,
            window: 1000,
            trigger: 900,
        });
        let base = host.catalog(CatalogKind::Commands).await.unwrap();
        let reader = HostHandle(host.clone())
            .service(&command::COMMAND_CATALOG)
            .unwrap();
        let catalog = reader.catalog(
            base,
            &command::SessionView {
                state: &state,
                providers: &[],
                models: &[],
            },
        );
        for (name, want) in [
            ("model", json!("fake/large")),
            ("think", json!("xhigh")),
            ("rename", json!("A title")),
            ("compact", json!("123 used")),
            ("login", Value::Null),
        ] {
            let entry = catalog
                .entries
                .iter()
                .find(|entry| entry.id == name)
                .unwrap();
            assert_eq!(entry.meta["now"], want, "{name}");
            assert!(entry.meta.as_object().unwrap().contains_key("now"));
        }
        let providers = [
            AuthStatus::Missing {
                hint: "test".into(),
            },
            AuthStatus::Ready { from: None },
            AuthStatus::Expired {
                hint: "test".into(),
            },
        ];
        let view = command::SessionView {
            state: &state,
            providers: &providers,
            models: &[],
        };
        for command in &host.registry.commands.entries {
            let value = command.now(&view);
            if command.spec().name == "login" {
                assert_eq!(value.as_deref(), Some("2 signed out"));
            }
        }
        host.shutdown().await;
    }
}

#[cfg(test)]
mod linear_values {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Counting {
        name: String,
        calls: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl Command for Counting {
        fn spec(&self) -> CommandSpec {
            self.calls.fetch_add(1, Ordering::Relaxed);
            CommandSpec::ui(&self.name)
        }
        async fn run(&self, _: &str, _: &CommandContext) -> Result<CommandOutcome, KernelError> {
            Ok(CommandOutcome::Handled { handled: true })
        }
    }
    struct Fixture(Arc<AtomicUsize>);
    impl Plugin for Fixture {
        fn manifest(&self) -> &'static PluginManifest {
            static MANIFEST: PluginManifest = PluginManifest {
                id: "counting",
                version: "1",
                uses: &[],
                requires: &[],
                provides: &[],
                config: None,
            };
            &MANIFEST
        }
        fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
            for name in ["one", "two", "three"] {
                registrar.add(Contribution::Command(Arc::new(Counting {
                    name: name.into(),
                    calls: self.0.clone(),
                })));
            }
            Ok(())
        }
    }
    #[tokio::test]
    async fn a_scoped_catalogue_reads_each_command_spec_once() {
        let home = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let host = crate::Host::build(
            vec![Box::new(Fixture(calls.clone()))],
            crate::HostConfig::new(Env::rooted(home.path())),
        )
        .await
        .unwrap();
        let reader = HostHandle(host.clone())
            .service(&command::COMMAND_CATALOG)
            .unwrap();
        let summary = serde_json::from_value(json!({"id":"s","cwd":"/unused","createdAt":"2026-10-15T00:00:00Z","updatedAt":"2026-10-15T00:00:00Z"})).unwrap();
        let state = SessionState::new(summary);
        let entries = ["one:ui", "two:ui", "three:ui"]
            .map(|id| CatalogEntry {
                id: id.into(),
                label: id.into(),
                meta: json!({}),
            })
            .to_vec();
        calls.store(0, Ordering::Relaxed);
        reader.catalog(
            Catalog {
                kind: CatalogKind::Commands,
                entries,
            },
            &command::SessionView {
                state: &state,
                providers: &[],
                models: &[],
            },
        );
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        host.shutdown().await;
    }
}

mod details;
pub(super) use details::details;

#[cfg(test)]
mod command_fold_tests {
    use super::*;

    fn registry() -> Registry {
        let mut registry = Registry::default();
        registry.add_command_fold(
            "fixture",
            CommandFold {
                into: "panel".into(),
                names: vec!["first".into()],
            },
        );
        registry.validate_command_folds();
        registry
    }

    fn entries(meta: Value) -> [CatalogEntry; 2] {
        [
            CatalogEntry {
                id: "first".into(),
                label: "A command".into(),
                meta,
            },
            CatalogEntry {
                id: "panel".into(),
                label: "A destination".into(),
                meta: json!({"name":"panel", "aliases":["first"]}),
            },
        ]
    }

    #[test]
    fn fold_projection_reads_only_the_final_name_and_aliases() {
        for meta in [
            json!({"name":"first"}),
            json!({"name":"renamed", "aliases":["first"]}),
        ] {
            let mut entries = entries(meta.clone());
            command_folds(&registry(), &mut entries);
            assert_eq!(entries[0].meta["foldedInto"], "panel");
            entries[0]
                .meta
                .as_object_mut()
                .unwrap()
                .remove("foldedInto");
            assert_eq!(entries[0].meta, meta);
            assert!(entries[1].meta.get("foldedInto").is_none());
        }
    }

    #[test]
    fn malformed_command_identities_never_keep_a_fold_mark() {
        for mut meta in [
            json!({}),
            json!({"name":false}),
            json!({"name":"first", "aliases":null}),
            json!({"name":"first", "aliases":"first"}),
            json!({"name":"first", "aliases":["first", false]}),
        ] {
            meta["foldedInto"] = json!("stale");
            let mut entries = entries(meta);
            command_folds(&registry(), &mut entries);
            assert!(entries[0].meta.get("foldedInto").is_none());
        }
    }

    #[test]
    fn a_missing_destination_or_disabled_owner_removes_the_fold_mark() {
        let registry = registry();
        let mut entries = entries(json!({"name":"first", "foldedInto":"stale"}));
        command_folds(&registry, &mut entries[..1]);
        assert!(entries[0].meta.get("foldedInto").is_none());
        command_folds(&registry, &mut entries);
        assert_eq!(entries[0].meta["foldedInto"], "panel");
        registry
            .switches
            .running
            .write()
            .unwrap()
            .insert("fixture".into());
        command_folds(&registry, &mut entries);
        assert!(entries[0].meta.get("foldedInto").is_none());
    }
}

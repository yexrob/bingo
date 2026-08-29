//! What a client may choose from: the registry read out as flat entries, one
//! reader per kind.

use bingo_sdk::*;
use serde_json::{Value, json};

use super::Registry;
use crate::models::ModelCatalog;

pub(super) async fn entries(
    registry: &Registry,
    model: Option<&str>,
    kind: CatalogKind,
) -> Vec<CatalogEntry> {
    match kind {
        CatalogKind::Models => models(registry, model),
        CatalogKind::Providers => providers(registry),
        CatalogKind::Tools => tools(registry).await,
        CatalogKind::Commands => commands(registry).await,
        CatalogKind::Skills => Vec::new(),
        CatalogKind::Plugins => plugins(registry),
    }
}

/// The embedded catalogue's models for each registered provider, plus the
/// configured one; nothing here asks a provider for its list, which would
/// be a network call.
fn models(registry: &Registry, configured: Option<&str>) -> Vec<CatalogEntry> {
    let catalogue = ModelCatalog::embedded();
    registry
        .providers
        .iter()
        .flat_map(|p| {
            let mut ids: Vec<&str> = configured.into_iter().collect();
            ids.extend(
                catalogue
                    .models_of(p.id())
                    .filter(|m| Some(*m) != configured),
            );
            ids.into_iter().map(|model| CatalogEntry {
                id: format!("{}/{model}", p.id()),
                label: model.to_string(),
                meta: json!({ "provider": p.id() }),
            })
        })
        .collect()
}

fn providers(registry: &Registry) -> Vec<CatalogEntry> {
    registry
        .providers
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

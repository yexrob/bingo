//! What a client may choose from: the registry read out as flat entries, one
//! reader per kind.

use bingo_sdk::*;
use serde_json::{Value, json};

use super::Registry;
use crate::models::ModelCatalog;

pub(super) fn entries(
    registry: &Registry,
    model: Option<&str>,
    kind: CatalogKind,
) -> Vec<CatalogEntry> {
    match kind {
        CatalogKind::Models => models(registry, model),
        CatalogKind::Providers => providers(registry),
        CatalogKind::Tools => tools(registry),
        CatalogKind::Commands => commands(registry),
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

fn tools(registry: &Registry) -> Vec<CatalogEntry> {
    registry
        .tools
        .iter()
        .map(|t| {
            let spec = t.spec();
            CatalogEntry {
                id: spec.name.clone(),
                label: spec.name,
                meta: json!({ "description": spec.description }),
            }
        })
        .collect()
}

fn commands(registry: &Registry) -> Vec<CatalogEntry> {
    registry
        .commands
        .iter()
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

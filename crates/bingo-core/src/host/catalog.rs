//! What a client may choose from: the registry read out as flat entries, one
//! reader per kind.

use bingo_sdk::*;
use serde_json::{Value, json};

use super::Registry;

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

/// Only the configured model, once per provider; nothing here asks a provider
/// for its list, which would be a network call.
fn models(registry: &Registry, model: Option<&str>) -> Vec<CatalogEntry> {
    registry
        .providers
        .iter()
        .filter_map(|p| {
            let model = model?.to_string();
            Some(CatalogEntry {
                id: format!("{}/{model}", p.id()),
                label: model,
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

//! Per-model metadata: context window and thinking support.
//!
//! Three tiers, most specific first (D65): what the user declared for this
//! provider's model, then the prefix table, then the conservative default.
//! `/status` percentages, the auto-compact threshold and the thinking gate all
//! read the model actually in use — the old fixed 200k window measured every
//! non-Claude model with a Claude ruler. Unknown models fall back to the
//! Claude defaults (200k window, thinking supported), which preserves the old
//! behavior exactly where nothing better is known.
//!
//! The catalog is a value, not a global: it hangs off `Client` (the provider
//! authority) and reaches the measuring sites as a [`ModelResolver`] already
//! bound to the current provider. A process-wide table would make two sessions
//! on different providers share one ruler.

use std::collections::HashMap;
use std::sync::Arc;

use crate::settings::{ModelEntry, Settings};

/// Metadata for one model family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelMeta {
    pub context_window: u64,
    pub supports_thinking: bool,
}

/// Conservative default (Claude family): what the whole app assumed for
/// every model before this table existed.
pub const DEFAULT_META: ModelMeta = ModelMeta {
    context_window: 200_000,
    supports_thinking: true,
};

/// Longest-prefix match over known families. Kept deliberately small: entries
/// earn their place by a real behavioral difference (window size or a wire
/// parameter that would 400).
const PREFIXES: &[(&str, ModelMeta)] = &[
    (
        "claude-",
        ModelMeta {
            context_window: 200_000,
            supports_thinking: true,
        },
    ),
    // Codex subscription family (gpt-5.x): larger window, reasoning effort.
    (
        "gpt-5",
        ModelMeta {
            context_window: 400_000,
            supports_thinking: true,
        },
    ),
    // DeepSeek chat endpoints reject anthropic thinking parameters — the
    // documented reason `/think off` exists. The gate skips the parameter for
    // them regardless of the configured level.
    (
        "deepseek",
        ModelMeta {
            context_window: 128_000,
            supports_thinking: false,
        },
    ),
];

/// Prefix-table tier. Private on purpose: every measuring site must go through
/// a [`ModelResolver`], or a provider's declaration would apply in one place
/// and not the next.
fn meta(model: &str) -> ModelMeta {
    PREFIXES
        .iter()
        .filter(|(prefix, _)| model.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(_, meta)| *meta)
        .unwrap_or(DEFAULT_META)
}

/// One model a provider declared in settings (`models`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModel {
    pub id: String,
    /// Menu label; falls back to the id.
    pub display: Option<String>,
    /// Declared overrides — absent fields defer to the prefix table.
    pub context_window: Option<u64>,
    pub thinking: Option<bool>,
}

impl CatalogModel {
    pub fn label(&self) -> &str {
        self.display.as_deref().unwrap_or(&self.id)
    }
}

/// Settings' declared models, resolved once per `Client`: provider name → the
/// models that provider declared, in the order they were written (the menu
/// shows them in that order — a sorted list would hide the user's own ranking).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelCatalog {
    by_provider: HashMap<String, Vec<CatalogModel>>,
}

impl ModelCatalog {
    /// Build from settings: `providers.<name>.models` plus the top-level
    /// `models`, which belongs to the reserved "default" provider.
    pub fn from_settings(settings: &Settings) -> Self {
        let mut by_provider = HashMap::new();
        for (name, config) in &settings.providers {
            if let Some(entries) = &config.models {
                by_provider.insert(name.clone(), convert(entries));
            }
        }
        if let Some(entries) = &settings.models {
            by_provider.insert("default".to_string(), convert(entries));
        }
        Self { by_provider }
    }

    /// What this provider declared; None when it declared nothing (the `/model`
    /// menu then pulls the list from the endpoint). An empty declaration is
    /// treated as no declaration — an empty menu is never the intent.
    pub fn declared(&self, provider: &str) -> Option<&[CatalogModel]> {
        self.by_provider
            .get(provider)
            .map(Vec::as_slice)
            .filter(|models| !models.is_empty())
    }

    fn entry(&self, provider: &str, model: &str) -> Option<&CatalogModel> {
        self.by_provider
            .get(provider)?
            .iter()
            .find(|entry| entry.id == model)
    }
}

fn convert(entries: &[ModelEntry]) -> Vec<CatalogModel> {
    entries
        .iter()
        .map(|entry| CatalogModel {
            id: entry.id().to_string(),
            display: entry.display().map(str::to_string),
            context_window: entry.context_window(),
            thinking: entry.thinking(),
        })
        .collect()
}

/// A metadata lookup bound to one provider — the ruler every measuring site
/// must share. `Client` hands one out for its current endpoint; `default()` is
/// the catalog-free ruler (prefix table only) used where no provider exists.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelResolver {
    catalog: Arc<ModelCatalog>,
    provider: String,
}

impl ModelResolver {
    pub fn new(catalog: Arc<ModelCatalog>, provider: String) -> Self {
        Self { catalog, provider }
    }

    /// Declared value first, prefix table for whatever the declaration left
    /// out, conservative default underneath (field by field, not all-or-none:
    /// declaring only `contextWindow` must not silently reset `thinking`).
    pub fn meta(&self, model: &str) -> ModelMeta {
        let table = meta(model);
        let Some(entry) = self.catalog.entry(&self.provider, model) else {
            return table;
        };
        ModelMeta {
            context_window: entry.context_window.unwrap_or(table.context_window),
            supports_thinking: entry.thinking.unwrap_or(table.supports_thinking),
        }
    }

    pub fn context_window(&self, model: &str) -> u64 {
        self.meta(model).context_window
    }

    pub fn supports_thinking(&self, model: &str) -> bool {
        self.meta(model).supports_thinking
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(json: &str) -> Settings {
        serde_json::from_str(json).unwrap()
    }

    fn resolver(json: &str, provider: &str) -> ModelResolver {
        ModelResolver::new(
            Arc::new(ModelCatalog::from_settings(&settings(json))),
            provider.to_string(),
        )
    }

    /// Lookup priority: declared > prefix table > conservative default, decided
    /// field by field.
    #[test]
    fn declaration_outranks_prefix_table() {
        let json = r#"{
            "models": [{"id": "claude-sonnet-5", "contextWindow": 1000}],
            "providers": {
                "proxy": {"apiKey": "k", "models": [
                    "gpt-5.6-sol",
                    {"id": "deepseek-v4", "contextWindow": 131072, "thinking": true},
                    {"id": "house-model", "thinking": false}
                ]}
            }
        }"#;
        let proxy = resolver(json, "proxy");
        // Declared with no overrides → prefix table.
        assert_eq!(proxy.context_window("gpt-5.6-sol"), 400_000);
        // Declared overrides beat the prefix table, including the thinking gate.
        assert_eq!(proxy.context_window("deepseek-v4"), 131_072);
        assert!(
            proxy.supports_thinking("deepseek-v4"),
            "a declaration may re-enable thinking the prefix table denies"
        );
        // Partial declaration: the untouched field still falls through.
        assert_eq!(proxy.context_window("house-model"), 200_000);
        assert!(!proxy.supports_thinking("house-model"));
        // Undeclared model on a declaring provider → prefix table.
        assert_eq!(proxy.context_window("claude-sonnet-5"), 200_000);

        // The catalog is per provider: the top-level declaration governs
        // "default" only.
        assert_eq!(
            resolver(json, "default").context_window("claude-sonnet-5"),
            1_000
        );
        assert_eq!(proxy.context_window("claude-sonnet-5"), 200_000);
        // No catalog at all → the old prefix-table behavior, unchanged.
        assert_eq!(
            ModelResolver::default().context_window("claude-sonnet-5"),
            200_000
        );
    }

    /// `declared` is what the menu keys off: absent and empty both mean "pull
    /// the list from the endpoint".
    #[test]
    fn declared_list_keeps_order_and_labels() {
        let catalog = ModelCatalog::from_settings(&settings(
            r#"{"providers": {
                "proxy": {"apiKey": "k", "models": ["z-model", {"id": "a-model", "display": "A"}]},
                "dynamic": {"apiKey": "k"},
                "empty": {"apiKey": "k", "models": []}
            }}"#,
        ));
        let declared = catalog.declared("proxy").unwrap();
        assert_eq!(declared.len(), 2);
        assert_eq!(declared[0].id, "z-model");
        assert_eq!(declared[0].label(), "z-model", "no display → the id shows");
        assert_eq!(declared[1].label(), "A", "display is the menu label");
        assert!(catalog.declared("dynamic").is_none());
        assert!(
            catalog.declared("empty").is_none(),
            "an empty declaration is not a declaration"
        );
        assert!(catalog.declared("unknown").is_none());
    }

    #[test]
    fn prefix_table_and_default() {
        let table = ModelResolver::default();
        assert_eq!(table.context_window("claude-sonnet-5"), 200_000);
        assert_eq!(table.context_window("gpt-5.6-sol"), 400_000);
        assert_eq!(table.context_window("deepseek-chat"), 128_000);
        assert_eq!(
            table.context_window("some-unknown-model"),
            200_000,
            "conservative default"
        );
        assert!(table.supports_thinking("claude-sonnet-5"));
        assert!(
            !table.supports_thinking("deepseek-chat"),
            "DeepSeek does not send thinking"
        );
        assert!(
            table.supports_thinking("totally-new-model"),
            "unknown models keep the default (preserves old behavior)"
        );
    }
}

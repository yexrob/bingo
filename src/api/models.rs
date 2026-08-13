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

use crate::api::types::DEFAULT_MAX_TOKENS;
use crate::settings::{ModelEntry, Settings};

/// Metadata for one model family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelMeta {
    pub context_window: u64,
    /// Output budget sent as `max_tokens`. It is also the headroom the input
    /// window must reserve, so a family whose real ceiling is lower than
    /// `DEFAULT_MAX_TOKENS` both stops 400ing and gets that headroom back.
    pub max_tokens: u32,
    pub supports_thinking: bool,
}

/// Conservative default (Claude family): what the whole app assumed for
/// every model before this table existed.
pub const DEFAULT_META: ModelMeta = ModelMeta {
    context_window: 200_000,
    max_tokens: DEFAULT_MAX_TOKENS,
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
            max_tokens: DEFAULT_MAX_TOKENS,
            supports_thinking: true,
        },
    ),
    // Codex subscription family (gpt-5.x): larger window, reasoning effort.
    (
        "gpt-5",
        ModelMeta {
            context_window: 400_000,
            max_tokens: DEFAULT_MAX_TOKENS,
            supports_thinking: true,
        },
    ),
    // DeepSeek chat endpoints reject anthropic thinking parameters — the
    // documented reason `/think off` exists. The gate skips the parameter for
    // them regardless of the configured level. Their documented output ceiling
    // is 8k, so the other 56k of reserved headroom belongs to the input window.
    (
        "deepseek",
        ModelMeta {
            context_window: 128_000,
            max_tokens: 8_000,
            supports_thinking: false,
        },
    ),
];

/// The compiled family table, exposed so the catalog file (D73) can mirror it
/// into its `builtin` section.
pub(crate) fn builtin_families() -> &'static [(&'static str, ModelMeta)] {
    PREFIXES
}

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
    pub max_tokens: Option<u32>,
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
    /// Family overrides from the catalog file (D73), longest prefix first —
    /// the tier between a settings declaration and the compiled table.
    families: Vec<(String, crate::model_families::FamilyMeta)>,
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
        Self {
            by_provider,
            families: Vec::new(),
        }
    }

    /// Attach the catalog file's overrides (already longest-prefix-first, as
    /// [`crate::model_families::load_overrides`] returns them).
    pub fn with_families(
        mut self,
        families: Vec<(String, crate::model_families::FamilyMeta)>,
    ) -> Self {
        self.families = families;
        self
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
            max_tokens: entry.max_tokens(),
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

    /// Declared value first, then the catalog file's family overrides, then
    /// the prefix table, conservative default underneath — each tier decided
    /// field by field, not all-or-none: declaring only `contextWindow` must
    /// not silently reset `thinking`, and a family override of `maxTokens`
    /// must not hide the table's window.
    pub fn meta(&self, model: &str) -> ModelMeta {
        let table = meta(model);
        // Family tier: first Some per field along the longest-prefix-first
        // list wins, so "deepseek-v4-flash" outranks "deepseek" where they
        // both speak and defers where it is silent.
        let mut family = crate::model_families::FamilyMeta::default();
        for (prefix, entry) in &self.catalog.families {
            if model.starts_with(prefix.as_str()) {
                family.context_window = family.context_window.or(entry.context_window);
                family.max_tokens = family.max_tokens.or(entry.max_tokens);
                family.thinking = family.thinking.or(entry.thinking);
            }
        }
        let base = ModelMeta {
            context_window: family.context_window.unwrap_or(table.context_window),
            max_tokens: family.max_tokens.unwrap_or(table.max_tokens),
            supports_thinking: family.thinking.unwrap_or(table.supports_thinking),
        };
        let Some(entry) = self.catalog.entry(&self.provider, model) else {
            return base;
        };
        ModelMeta {
            context_window: entry.context_window.unwrap_or(base.context_window),
            max_tokens: entry.max_tokens.unwrap_or(base.max_tokens),
            supports_thinking: entry.thinking.unwrap_or(base.supports_thinking),
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
                    {"id": "house-model", "thinking": false},
                    {"id": "small-model", "contextWindow": 32768, "maxTokens": 4096}
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
        assert_eq!(
            proxy.meta("deepseek-v4").max_tokens,
            8_000,
            "an undeclared output budget still falls through to the prefix table"
        );
        // Partial declaration: the untouched field still falls through.
        assert_eq!(proxy.context_window("house-model"), 200_000);
        assert!(!proxy.supports_thinking("house-model"));
        assert_eq!(proxy.meta("house-model").max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(proxy.meta("small-model").max_tokens, 4_096);
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

    /// The family tier sits between the declaration and the prefix table,
    /// decided field by field across matching prefixes (longest first).
    #[test]
    fn family_overrides_sit_between_declaration_and_table() {
        let fam = |cw: Option<u64>, mt: Option<u32>, th: Option<bool>| {
            crate::model_families::FamilyMeta {
                context_window: cw,
                max_tokens: mt,
                thinking: th,
            }
        };
        let json = r#"{"providers": {"proxy": {"apiKey": "k", "models": [
            {"id": "deepseek-v4-flash", "contextWindow": 131072}
        ]}}}"#;
        let catalog = ModelCatalog::from_settings(&settings(json)).with_families(vec![
            // load_overrides hands the list longest-prefix-first.
            (
                "deepseek-v4-flash".to_string(),
                fam(None, Some(32_000), None),
            ),
            (
                "deepseek".to_string(),
                fam(Some(200_000), Some(64_000), Some(true)),
            ),
        ]);
        let proxy = ModelResolver::new(Arc::new(catalog), "proxy".to_string());
        // Declared beats the family tier; the family tier fills what the
        // declaration left out; longest prefix wins per field.
        assert_eq!(proxy.context_window("deepseek-v4-flash"), 131_072);
        assert_eq!(proxy.meta("deepseek-v4-flash").max_tokens, 32_000);
        assert!(proxy.supports_thinking("deepseek-v4-flash"));
        // An undeclared sibling still gets the family-wide values, and the
        // family tier beats the compiled prefix table.
        assert_eq!(proxy.context_window("deepseek-chat"), 200_000);
        assert_eq!(proxy.meta("deepseek-chat").max_tokens, 64_000);
        // Models no family entry matches fall through to the table untouched.
        assert_eq!(proxy.meta("claude-sonnet-5").max_tokens, DEFAULT_MAX_TOKENS);
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
        assert_eq!(table.meta("claude-sonnet-5").max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(table.meta("gpt-5.6-sol").max_tokens, DEFAULT_MAX_TOKENS);
        assert_eq!(
            table.meta("deepseek-chat").max_tokens,
            8_000,
            "DeepSeek's documented output ceiling, not the Claude default"
        );
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

//! Built-in official provider presets (D34 §6.5): zero-config visibility and
//! login for official subscriptions. Users override presets field-by-field
//! via settings `providers.<name>` (absent fields fall back to the preset);
//! settings itself stays user-only — presets are resolved at build time.
//!
//! A preset templates the endpoint, never the model list: bingo does not
//! filter models (D65). Narrowing what a subscription offers is the user's
//! call, spelled `providers.<name>.models` in settings.

/// One built-in subscription template.
pub struct ProviderPreset {
    pub name: &'static str,
    /// Listing label (user-facing).
    #[allow(dead_code)] // future /provider detail view
    pub display: &'static str,
    pub protocol: &'static str,
    /// Default endpoint base URL. (The OpenAI adapter variant is derived
    /// from `oauth_kind`: "codex" → Codex, None → Default.)
    pub base_url: &'static str,
    /// OAuth flow kind (None = apiKey preset — key set via `/provider login`).
    pub oauth_kind: Option<&'static str>,
    pub supports_images: bool,
}

/// The official-subscription registry (compile-time, no IO).
pub const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "codex",
        display: "Codex (ChatGPT subscription)",
        protocol: "openai",
        base_url: "https://chatgpt.com/backend-api",
        oauth_kind: Some("codex"),
        supports_images: true,
    },
    ProviderPreset {
        name: "opencode-go",
        display: "opencode Go (subscription)",
        protocol: "openai",
        base_url: "https://opencode.ai/zen/go",
        oauth_kind: None,
        supports_images: false,
    },
];

pub fn preset(name: &str) -> Option<&'static ProviderPreset> {
    PRESETS.iter().find(|p| p.name == name)
}

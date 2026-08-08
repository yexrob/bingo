//! Built-in official provider presets (D34 §6.5): zero-config visibility and
//! login for official subscriptions. Users override presets field-by-field
//! via settings `providers.<name>` (absent fields fall back to the preset);
//! settings itself stays user-only — presets are resolved at build time.

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
    /// Static model allowlist (None = pull the endpoint's model list).
    pub model_allowlist: Option<&'static [&'static str]>,
}

/// The official-subscription registry (compile-time, no IO).
pub const PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "codex",
        display: "Codex (ChatGPT 订阅)",
        protocol: "openai",
        base_url: "https://chatgpt.com/backend-api",
        oauth_kind: Some("codex"),
        supports_images: true,
        model_allowlist: Some(&[
            "gpt-5.5",
            "gpt-5.6-luna",
            "gpt-5.3-codex-spark",
            "gpt-5.4",
            "gpt-5.4-mini",
        ]),
    },
    ProviderPreset {
        name: "opencode-go",
        display: "opencode Go (订阅)",
        protocol: "openai",
        base_url: "https://opencode.ai/zen/go",
        oauth_kind: None,
        supports_images: false,
        model_allowlist: Some(&["gpt-5.6-luna"]),
    },
];

pub fn preset(name: &str) -> Option<&'static ProviderPreset> {
    PRESETS.iter().find(|p| p.name == name)
}

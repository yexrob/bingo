//! `Client` is the provider facade (D33): it owns the provider table,
//! the current-provider switch and the display info used by `/provider`
//! and the `/model` menu. Protocol behavior lives in the adapters
//! (`api::providers`); consumers never see wire JSON.

use std::collections::HashMap;
use std::sync::Arc;

use crate::api::contract::{BoxStream, NeutralRequest, ProviderClient, SystemBlock};
use crate::api::providers;
use crate::api::types::Message;

pub use crate::api::contract::{AssistantAccumulator, ClientError};

/// cfg(test) test hook re-export (tui/chat.rs timeout tests, error.rs drift
/// test use these).
#[cfg(test)]
pub(crate) use crate::api::contract::transport_offline_code;
#[cfg(test)]
pub(crate) use crate::api::providers::anthropic::test_hooks;

/// Per-protocol default endpoint base URL (used when a named provider leaves
/// `apiBaseUrl` empty, D33).
fn protocol_default_base_url(protocol: Option<&str>) -> String {
    match protocol.unwrap_or("anthropic") {
        "openai" => providers::openai::API_BASE.to_string(),
        _ => providers::anthropic::API_BASE.to_string(),
    }
}

/// A provider's static key: `apiKey` first, then the variable named by
/// `envKey` (D64). None hands the decision down to the existing mechanisms
/// (auth.json stored key / OAuth) — an `envKey` pointing at an unset variable
/// is not a credential, so it must not shadow them.
fn resolve_key(
    cfg: &crate::settings::ProviderConfig,
    env: &impl Fn(&str) -> std::result::Result<String, std::env::VarError>,
) -> Option<String> {
    cfg.api_key.clone().or_else(|| {
        cfg.env_key
            .as_deref()
            .and_then(|name| env(name).ok())
            .filter(|key| !key.is_empty())
    })
}

/// Display info for a provider (the `/provider` listing and `/model` menu):
/// auth material (masked by the caller) + endpoint URL.
#[derive(Clone)]
struct EndpointInfo {
    /// Provider name this endpoint is registered under — the catalog key, so
    /// the current endpoint can name its own ruler (D64).
    name: String,
    /// Static key (None = OAuth/stored-key provider — no key to mask).
    api_key: Option<String>,
    base_url: String,
    /// Wire protocol label ("anthropic" / "openai") — the /provider listing.
    protocol: String,
    /// Shared OAuth token provider: `/provider login|logout` mutate the same
    /// instance the adapter reads, so credentials take effect live.
    oauth: Option<Arc<crate::api::auth::TokenProvider>>,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    /// Current provider: adapter (source of truth for capabilities) + the
    /// display info paired with it (kept in lockstep on switches).
    endpoint: Arc<std::sync::RwLock<(Arc<dyn ProviderClient>, EndpointInfo)>>,
    /// Named-provider table (default is not in the table for listing, but is
    /// stored under the reserved key "default").
    providers: HashMap<String, (Arc<dyn ProviderClient>, EndpointInfo)>,
    /// Names coming from the built-in preset registry (D34): the /provider
    /// listing shows them with a built-in badge.
    preset_names: Vec<String>,
    /// Settings' declared models (D64), resolved once: the `/model` menu reads
    /// it instead of the endpoint, and every context-window measurement goes
    /// through the resolver it hands out.
    catalog: Arc<crate::api::models::ModelCatalog>,
}

impl Client {
    #[cfg(test)]
    pub fn from_settings(settings: &crate::settings::Settings) -> Result<Self, ClientError> {
        Self::from_settings_at(settings, std::path::Path::new("/tmp/bingo-test-home"))
    }

    #[cfg(test)]
    pub(crate) fn from_settings_with(
        settings: &crate::settings::Settings,
        env: impl Fn(&str) -> std::result::Result<String, std::env::VarError>,
    ) -> Result<Self, ClientError> {
        Self::from_settings_at_with(settings, env, std::path::Path::new("/tmp/bingo-test-home"))
    }

    /// Build against an explicitly resolved home. Production passes the same home used
    /// by every state store so HOME-less Windows never falls back to cwd.
    pub fn from_settings_at(
        settings: &crate::settings::Settings,
        home: &std::path::Path,
    ) -> Result<Self, ClientError> {
        Self::build(settings, |name| std::env::var(name), home)
    }

    /// Build against an explicit home and injected environment (tests isolate auth.json
    /// and avoid real environment variables).
    #[cfg(test)]
    pub(crate) fn from_settings_at_with(
        settings: &crate::settings::Settings,
        env: impl Fn(&str) -> std::result::Result<String, std::env::VarError>,
        home: &std::path::Path,
    ) -> Result<Self, ClientError> {
        Self::build(settings, env, home)
    }

    fn build(
        settings: &crate::settings::Settings,
        env: impl Fn(&str) -> std::result::Result<String, std::env::VarError>,
        home: &std::path::Path,
    ) -> Result<Self, ClientError> {
        let http = reqwest::Client::new();
        // A missing top-level key no longer aborts construction: the default
        // provider degrades to an unconfigured fail-fast adapter so the TUI
        // (and `/provider login`) stays reachable — the old hard failure
        // locked subscription-only users out before the login command.
        let api_key = settings
            .api_key
            .clone()
            .or_else(|| env("ANTHROPIC_API_KEY").ok())
            .or_else(|| env("DEEPSEEK_API_KEY").ok());
        let base_url = settings.api_base_url.clone().unwrap_or_else(|| {
            env("ANTHROPIC_BASE_URL").unwrap_or_else(|_| providers::anthropic::API_BASE.to_string())
        });
        let mut providers: HashMap<String, (Arc<dyn ProviderClient>, EndpointInfo)> =
            HashMap::new();
        let mut preset_names = Vec::new();
        // ① Built-in official subscriptions (D34 §6.5): visible and loginable
        // with zero config; a user `providers.<name>` entry overrides the
        // preset field-by-field (absent fields fall back to the preset).
        for preset in providers::presets::PRESETS {
            let user = settings.providers.get(preset.name);
            let protocol = user
                .and_then(|c| c.protocol.clone())
                .unwrap_or_else(|| preset.protocol.to_string());
            // apiKey presets (opencode-go) resolve their key from auth.json
            // live per request (`stored_key` below) — a /provider login in
            // the running session takes effect without a restart.
            let api_key = user.and_then(|c| resolve_key(c, &env));
            let base_url = match user {
                Some(c) if !c.api_base_url.is_empty() => c.api_base_url.clone(),
                _ => preset.base_url.to_string(),
            };
            let supports_images = user
                .and_then(|c| c.supports_images)
                .unwrap_or(preset.supports_images);
            let oauth = user.and_then(|c| c.oauth.clone()).or_else(|| {
                preset.oauth_kind.map(|kind| crate::settings::OauthConfig {
                    kind: kind.to_string(),
                    account: None,
                })
            });
            let built = providers::build_provider(
                preset.name,
                http.clone(),
                Some(&protocol),
                api_key.clone(),
                base_url.clone(),
                supports_images,
                oauth.as_ref(),
                home,
                true,
            )
            .map_err(|message| {
                ClientError::Config(format!("provider \"{}\": {message}", preset.name))
            })?;
            providers.insert(
                preset.name.to_string(),
                (
                    built.adapter,
                    EndpointInfo {
                        name: preset.name.to_string(),
                        api_key,
                        base_url,
                        protocol,
                        oauth: built.token_provider,
                    },
                ),
            );
            preset_names.push(preset.name.to_string());
        }
        // ② User-only providers (preset names are already merged above).
        for (name, cfg) in &settings.providers {
            if providers::presets::preset(name).is_some() {
                continue;
            }
            let protocol = cfg.protocol.as_deref();
            let base_url = if cfg.api_base_url.is_empty() {
                protocol_default_base_url(protocol)
            } else {
                cfg.api_base_url.clone()
            };
            let api_key = resolve_key(cfg, &env);
            let built = providers::build_provider(
                name,
                http.clone(),
                protocol,
                api_key.clone(),
                base_url.clone(),
                // Both protocols define image content blocks: the capability is the
                // baseline, and `supportsImages: false` is the opt-out for an endpoint that
                // speaks the protocol but rejects them (some compat proxies).
                cfg.supports_images.unwrap_or(true),
                cfg.oauth.as_ref(),
                home,
                false,
            )
            .map_err(|message| {
                // Config error (e.g. unknown protocol) — surfaced at
                // startup with the same code family as settings parse
                // failures, before any request goes out.
                ClientError::Config(format!("provider \"{name}\": {message}"))
            })?;
            providers.insert(
                name.clone(),
                (
                    built.adapter,
                    EndpointInfo {
                        name: name.clone(),
                        api_key,
                        base_url,
                        protocol: protocol.unwrap_or("anthropic").to_string(),
                        oauth: built.token_provider,
                    },
                ),
            );
        }
        // The default endpoint also enters the providers table (key "default"): set_provider /
        // with_provider("default") works (including switching back to default); the /model
        // secondary list uses the top-level endpoint for default; label and content match (P0-C). default is a
        // reserved name: the top-level config wins (later inserts override a same-named user providers entry).
        let default_adapter = match &api_key {
            Some(key) => providers::anthropic(
                http.clone(),
                key.clone(),
                base_url.clone(),
                settings.send_images.unwrap_or(true),
            ),
            None => providers::unconfigured(),
        };
        let default_info = EndpointInfo {
            name: "default".to_string(),
            api_key,
            base_url,
            protocol: "anthropic".to_string(),
            oauth: None,
        };
        providers.insert(
            "default".to_string(),
            (default_adapter.clone(), default_info.clone()),
        );
        Ok(Self {
            http,
            endpoint: Arc::new(std::sync::RwLock::new((default_adapter, default_info))),
            providers,
            preset_names,
            catalog: Arc::new(crate::api::models::ModelCatalog::from_settings(settings)),
        })
    }

    #[cfg(test)]
    pub fn new(api_key: String, base_url: String) -> Self {
        let http = reqwest::Client::new();
        let adapter = providers::anthropic(http.clone(), api_key.clone(), base_url.clone(), false);
        let info = EndpointInfo {
            name: "default".to_string(),
            api_key: Some(api_key),
            base_url,
            protocol: "anthropic".to_string(),
            oauth: None,
        };
        Self {
            http,
            endpoint: Arc::new(std::sync::RwLock::new((adapter.clone(), info.clone()))),
            providers: HashMap::from([("default".to_string(), (adapter, info))]),
            preset_names: Vec::new(),
            catalog: Arc::new(crate::api::models::ModelCatalog::default()),
        }
    }

    /// Named-provider list (default excluded; the /provider listing — callers prepend "default"
    /// explicitly, so both the /model menu and /provider output lead with "default").
    pub fn provider_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .providers
            .keys()
            .filter(|n| n.as_str() != "default")
            .cloned()
            .collect();
        names.sort();
        names
    }

    /// Endpoint of a named provider (key/url; "default" = the top-level config).
    /// Unknown names return None.
    pub fn provider_endpoint(&self, name: &str) -> Option<(Option<String>, String)> {
        self.providers
            .get(name)
            .map(|(_, info)| (info.api_key.clone(), info.base_url.clone()))
    }

    /// Named providers whose endpoint accepts image blocks. A text-only session can still put an
    /// attachment in front of a model by forking a subagent onto one of these, so the list is
    /// what makes that route discoverable instead of guessable. "default" is excluded: as a
    /// sub-agent argument it means "share the parent endpoint", which is the one that can't.
    pub fn image_capable_providers(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .providers
            .iter()
            .filter(|(name, (adapter, _))| {
                name.as_str() != "default" && adapter.capabilities().supports_images
            })
            .map(|(name, _)| name.clone())
            .collect();
        names.sort();
        names
    }

    /// Wire protocol label of a named provider ("anthropic"/"openai";
    /// the /provider listing). Unknown names return None.
    pub fn provider_protocol(&self, name: &str) -> Option<String> {
        self.providers
            .get(name)
            .map(|(_, info)| info.protocol.clone())
    }

    /// The currently active provider endpoint (key/url reference).
    pub fn current_endpoint(&self) -> (Option<String>, String) {
        let current = self.endpoint.read().unwrap_or_else(|p| p.into_inner());
        (current.1.api_key.clone(), current.1.base_url.clone())
    }

    /// Whether the current endpoint accepts image content blocks
    /// (`supportsImages`/`sendImages` config).
    pub fn supports_images(&self) -> bool {
        self.endpoint
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .0
            .capabilities()
            .supports_images
    }

    /// Switch to a named provider; unknown names error out ("default" = the
    /// top-level endpoint, always switchable back to).
    pub fn set_provider(&self, name: &str) -> Result<(), String> {
        let Some((adapter, info)) = self.providers.get(name).cloned() else {
            return Err(format!(
                "provider \"{name}\" not found (see /provider for the list)"
            ));
        };
        *self.endpoint.write().unwrap_or_else(|p| p.into_inner()) = (adapter, info);
        Ok(())
    }

    /// Derive an endpoint-independent Client (for sub-agents that pin a
    /// provider): the new Client locks that provider's endpoint, and the
    /// providers table is shared (same name table). Without a provider you
    /// should just clone (shared endpoint, follows the parent session's
    /// switches).
    pub fn with_provider(&self, name: &str) -> Result<Client, String> {
        let Some((adapter, info)) = self.providers.get(name).cloned() else {
            return Err(format!(
                "provider \"{name}\" not found (see /provider for the list)"
            ));
        };
        Ok(Client {
            http: self.http.clone(),
            endpoint: Arc::new(std::sync::RwLock::new((adapter, info))),
            providers: self.providers.clone(),
            preset_names: self.preset_names.clone(),
            catalog: self.catalog.clone(),
        })
    }

    /// Start a streaming request on the current provider.
    pub async fn stream(&self, request: &NeutralRequest) -> Result<BoxStream, ClientError> {
        self.current().stream(request).await
    }

    /// Non-streaming completion on the current provider (compact summaries,
    /// memory extraction).
    pub async fn complete_text(&self, request: &NeutralRequest) -> Result<String, ClientError> {
        self.current().complete_text(request).await
    }

    /// The models the current provider offers. A settings declaration answers
    /// without asking the endpoint (D64) — this is the single authority, so
    /// the `/model` menu and the JSON protocol's models.list cannot disagree
    /// about what a provider offers.
    pub async fn list_models(&self) -> Result<Vec<String>, ClientError> {
        let declared = {
            let current = self.endpoint.read().unwrap_or_else(|p| p.into_inner());
            self.catalog
                .declared(&current.1.name)
                .map(|models| models.iter().map(|m| m.id.clone()).collect())
        };
        match declared {
            Some(models) => Ok(models),
            None => self.current().list_models().await,
        }
    }

    /// Input token count on the current provider (D12: the budget display
    /// goes through the official count_tokens API).
    pub async fn count_tokens(
        &self,
        model: &str,
        system: &[SystemBlock],
        messages: &[Message],
        tools: &[serde_json::Value],
    ) -> Result<u64, ClientError> {
        self.current()
            .count_tokens(model, system, messages, tools)
            .await
    }

    /// Whether the provider comes from the built-in preset registry (D34) —
    /// the /provider listing shows a built-in badge.
    pub fn is_preset(&self, name: &str) -> bool {
        self.preset_names.iter().any(|p| p == name)
    }

    /// Auth state of a named provider (the /provider listing; "default" is
    /// the top-level endpoint). Unknown names return None.
    pub fn auth_status(&self, name: &str) -> Option<crate::api::contract::AuthStatus> {
        self.providers.get(name).map(|(p, _)| p.auth_status())
    }

    /// The shared OAuth token provider of a named provider — the SAME
    /// instance the adapter authenticates with. `/provider login|logout`
    /// must go through this so credentials take effect in the running
    /// session instead of after a restart.
    pub fn token_provider(&self, name: &str) -> Option<Arc<crate::api::auth::TokenProvider>> {
        self.providers
            .get(name)
            .and_then(|(_, info)| info.oauth.clone())
    }

    /// Whether a provider has any usable credential right now (read live).
    pub fn is_configured(&self, name: &str) -> bool {
        match self.auth_status(name) {
            Some(crate::api::contract::AuthStatus::ApiKey) => true,
            Some(crate::api::contract::AuthStatus::StoredKey { configured }) => configured,
            Some(crate::api::contract::AuthStatus::OAuth { account }) => account.is_some(),
            Some(crate::api::contract::AuthStatus::Unconfigured) | None => false,
        }
    }

    /// The model a provider starts with when none was remembered: a declared
    /// list leads with its first entry (the user's own ranking); anthropic-
    /// protocol endpoints get the built-in default. None = no safe guess (the
    /// caller warns).
    pub fn provider_default_model(&self, name: &str) -> Option<String> {
        if let Some(first) = self.catalog.declared(name).and_then(<[_]>::first) {
            return Some(first.id.clone());
        }
        match self.provider_protocol(name).as_deref() {
            Some("anthropic") => Some(crate::api::types::DEFAULT_MODEL.to_string()),
            _ => None,
        }
    }

    /// The models a provider declared in settings (D64). Some = authoritative,
    /// the `/model` menu shows exactly this and never asks the endpoint.
    pub fn declared_models(&self, name: &str) -> Option<Vec<crate::api::models::CatalogModel>> {
        self.catalog.declared(name).map(<[_]>::to_vec)
    }

    /// Metadata ruler for the current endpoint: every context-window and
    /// thinking-gate decision measures with this one, so a provider's
    /// declaration cannot apply in one place and not the next.
    pub fn models(&self) -> crate::api::models::ModelResolver {
        let current = self.endpoint.read().unwrap_or_else(|p| p.into_inner());
        crate::api::models::ModelResolver::new(self.catalog.clone(), current.1.name.clone())
    }

    fn current(&self) -> Arc<dyn ProviderClient> {
        self.endpoint
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .0
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_home_routes_stored_credentials_outside_cwd() {
        let home = std::env::temp_dir().join(format!("bingo-client-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        crate::auth::AuthStore::new(&home)
            .set(
                "opencode-go",
                crate::auth::AuthEntry::Api {
                    key: "stored-key".into(),
                },
            )
            .unwrap();
        let settings = crate::settings::Settings::default();

        let client = Client::from_settings_at_with(
            &settings,
            |_| Err(std::env::VarError::NotPresent),
            &home,
        )
        .unwrap();

        assert!(client.is_configured("opencode-go"));
        assert!(crate::auth::auth_path(&home).exists());
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn from_settings_prefers_settings_over_env() {
        let settings = crate::settings::Settings {
            api_key: Some("sk-settings".into()),
            api_base_url: Some("https://settings.example".into()),
            ..Default::default()
        };
        let env = |name: &str| -> Result<String, std::env::VarError> {
            match name {
                "ANTHROPIC_API_KEY" => Ok("sk-env".into()),
                "ANTHROPIC_BASE_URL" => Ok("https://env.example".into()),
                _ => Err(std::env::VarError::NotPresent),
            }
        };
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-settings"));
        assert_eq!(client.current_endpoint().1, "https://settings.example");
    }

    #[test]
    fn from_settings_falls_back_to_env() {
        let settings = crate::settings::Settings::default();
        let env = |name: &str| -> Result<String, std::env::VarError> {
            match name {
                "DEEPSEEK_API_KEY" => Ok("sk-deepseek".into()),
                "ANTHROPIC_BASE_URL" => Ok("https://deepseek.example".into()),
                _ => Err(std::env::VarError::NotPresent),
            }
        };
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-deepseek"));
        assert_eq!(client.current_endpoint().1, "https://deepseek.example");
    }

    /// A missing top-level key no longer aborts construction: the default
    /// provider degrades to an unconfigured fail-fast adapter, so the TUI
    /// (where `/provider login` lives) stays reachable.
    #[test]
    fn missing_key_degrades_default_to_unconfigured() {
        let settings = crate::settings::Settings::default();
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert!(matches!(
            client.auth_status("default"),
            Some(crate::api::contract::AuthStatus::Unconfigured)
        ));
        assert!(!client.is_configured("default"));
        // Presets stay reachable for login.
        assert!(client.provider_names().contains(&"codex".to_string()));
    }

    #[test]
    fn from_settings_defaults_base_url() {
        let settings = crate::settings::Settings {
            api_key: Some("sk".into()),
            ..Default::default()
        };
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().1, providers::anthropic::API_BASE);
    }

    #[test]
    fn provider_switch_changes_endpoint() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                env_key: None,
                models: None,
                api_key: Some("sk-ds".into()),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        settings.providers.insert(
            "local".to_string(),
            crate::settings::ProviderConfig {
                env_key: None,
                models: None,
                api_key: Some("sk-local".into()),
                api_base_url: "http://127.0.0.1:11434".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-main"));
        assert_eq!(
            client.provider_names(),
            vec!["codex", "deepseek", "local", "opencode-go"],
            "presets and user providers are merged"
        );

        client.set_provider("deepseek").unwrap();
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-ds"));
        assert_eq!(client.current_endpoint().1, "https://api.deepseek.com");

        assert!(
            client.set_provider("nope").is_err(),
            "unknown provider errors"
        );
        // An unknown provider does not affect the current endpoint.
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-ds"));
    }

    /// P0-C: "default" endpoint is switchable and resolvable — provider_names excludes it,
    /// set_provider/with_provider("default") reach the top-level endpoint, provider_endpoint
    /// returns its URL (for the /provider listing).
    #[test]
    fn default_provider_is_switchable_and_listed_as_endpoint() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            api_base_url: Some("https://main.example".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "deepseek".to_string(),
            crate::settings::ProviderConfig {
                env_key: None,
                models: None,
                api_key: Some("sk-ds".into()),
                api_base_url: "https://api.deepseek.com".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        // default does not appear in the named list (callers add it explicitly).
        assert_eq!(
            client.provider_names(),
            vec!["codex", "deepseek", "opencode-go"],
            "presets are visible"
        );
        assert_eq!(
            client.provider_endpoint("default"),
            Some((
                Some("sk-main".to_string()),
                "https://main.example".to_string()
            ))
        );
        assert_eq!(
            client.provider_endpoint("deepseek").unwrap().1,
            "https://api.deepseek.com"
        );
        assert_eq!(client.provider_endpoint("nope"), None);

        // Switch to deepseek and back to default: the top-level endpoint is restored (including supports_images).
        client.set_provider("deepseek").unwrap();
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-ds"));
        client.set_provider("default").unwrap();
        assert_eq!(client.current_endpoint().0.as_deref(), Some("sk-main"));
        assert_eq!(client.current_endpoint().1, "https://main.example");

        // with_provider("default") forks the top-level endpoint (the /model secondary list
        // uses it for default; label and content match).
        let fork = client.with_provider("default").unwrap();
        assert_eq!(fork.current_endpoint().0.as_deref(), Some("sk-main"));
    }

    /// apiKey wins; with both missing → CONFIG_INVALID (main-verified bug regression;
    /// D33 §5: apiKey wins over OAuth, both missing → config error).
    #[test]
    fn oauth_config_resolution() {
        // 2) Both apiKey and oauth configured → ApiKey wins.
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "codex".to_string(),
            crate::settings::ProviderConfig {
                env_key: None,
                models: None,
                api_key: Some("sk-static".into()),
                api_base_url: String::new(),
                supports_images: None,
                protocol: Some("openai".into()),
                oauth: Some(crate::settings::OauthConfig {
                    kind: "codex".into(),
                    account: None,
                }),
            },
        );
        let client =
            Client::from_settings_with(&settings, |_| Err(std::env::VarError::NotPresent)).unwrap();
        assert!(
            matches!(
                client.auth_status("codex"),
                Some(crate::api::contract::AuthStatus::ApiKey)
            ),
            "apiKey wins over oauth"
        );

        // 3) No apiKey and no oauth → CONFIG_INVALID (reported at startup).
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "bare".to_string(),
            crate::settings::ProviderConfig {
                env_key: None,
                models: None,
                api_key: None,
                api_base_url: String::new(),
                supports_images: None,
                protocol: Some("openai".into()),
                oauth: None,
            },
        );
        let err = Client::from_settings_with(&settings, |_| Err(std::env::VarError::NotPresent))
            .err()
            .unwrap();
        assert_eq!(crate::error::map_error(&err), "CONFIG_INVALID");
        assert!(err.to_string().contains("missing apiKey or oauth"), "{err}");
    }

    /// D64 credential order: `apiKey` > `envKey`'s variable > the existing
    /// mechanisms. `is_configured` must agree — the JSON protocol's
    /// providers.result reads it, and a second opinion there is how #43
    /// started.
    #[test]
    fn env_key_resolves_between_api_key_and_the_stored_credential() {
        let home = std::env::temp_dir().join(format!("bingo-envkey-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let env = |name: &str| -> Result<String, std::env::VarError> {
            match name {
                "PROXY_KEY" => Ok("sk-from-env".into()),
                _ => Err(std::env::VarError::NotPresent),
            }
        };
        let provider =
            |api_key: Option<&str>, env_key: Option<&str>| crate::settings::ProviderConfig {
                api_key: api_key.map(str::to_string),
                api_base_url: "https://proxy.example".into(),
                protocol: Some("openai".into()),
                oauth: None,
                supports_images: None,
                env_key: env_key.map(str::to_string),
                models: None,
            };
        let mut settings = crate::settings::Settings::default();
        settings
            .providers
            .insert("from-env".to_string(), provider(None, Some("PROXY_KEY")));
        settings.providers.insert(
            "settings-wins".to_string(),
            provider(Some("sk-explicit"), Some("PROXY_KEY")),
        );
        // An envKey naming an unset variable must not shadow the fallbacks —
        // for an openai provider with no oauth that is a config error, the
        // same one an omitted key produces.
        settings
            .providers
            .insert("unset".to_string(), provider(None, Some("NOT_SET")));

        let err = Client::from_settings_at_with(&settings, env, &home)
            .err()
            .unwrap();
        assert_eq!(crate::error::map_error(&err), "CONFIG_INVALID");
        assert!(err.to_string().contains("missing apiKey or oauth"), "{err}");

        settings.providers.remove("unset");
        let client = Client::from_settings_at_with(&settings, env, &home).unwrap();
        assert_eq!(
            client.provider_endpoint("from-env").unwrap().0.as_deref(),
            Some("sk-from-env"),
            "envKey supplies the key"
        );
        assert!(client.is_configured("from-env"));
        assert_eq!(
            client
                .provider_endpoint("settings-wins")
                .unwrap()
                .0
                .as_deref(),
            Some("sk-explicit"),
            "apiKey outranks envKey"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// D64: declared models are the menu's source and the endpoint's default
    /// model; a provider that declares nothing keeps pulling its own list.
    #[test]
    fn declared_models_reach_the_menu_and_the_default_model() {
        let settings: crate::settings::Settings = serde_json::from_str(
            r#"{
                "apiKey": "sk-main",
                "models": ["claude-opus-5"],
                "providers": {
                    "opencode-go": {"models": [{"id": "gpt-5.6-luna", "display": "Luna"}]},
                    "proxy": {"apiKey": "k", "protocol": "openai"}
                }
            }"#,
        )
        .unwrap();
        let client =
            Client::from_settings_with(&settings, |_| Err(std::env::VarError::NotPresent)).unwrap();
        let declared = client.declared_models("opencode-go").unwrap();
        assert_eq!(declared[0].id, "gpt-5.6-luna");
        assert_eq!(declared[0].label(), "Luna");
        assert_eq!(
            client.provider_default_model("opencode-go").as_deref(),
            Some("gpt-5.6-luna"),
            "a declared list leads with the user's own first entry"
        );
        assert_eq!(
            client.declared_models("proxy"),
            None,
            "no declaration → the endpoint's list"
        );
        assert_eq!(
            client.provider_default_model("proxy"),
            None,
            "nothing safe to guess for an undeclared openai endpoint"
        );
        // The top-level `models` belongs to "default".
        assert_eq!(
            client.declared_models("default").map(|m| m[0].id.clone()),
            Some("claude-opus-5".to_string())
        );
    }

    /// list_models is the one authority: a declaration answers it too, so the
    /// JSON protocol's models.list and the `/model` menu describe the same
    /// provider. Reaching the endpoint here would be a network call — the
    /// unreachable base URL proves none was made.
    #[tokio::test]
    async fn list_models_answers_from_the_declaration() {
        let settings: crate::settings::Settings = serde_json::from_str(
            r#"{
                "apiKey": "sk-main",
                "apiBaseUrl": "http://127.0.0.1:1",
                "models": ["declared-a", {"id": "declared-b", "display": "B"}]
            }"#,
        )
        .unwrap();
        let client =
            Client::from_settings_with(&settings, |_| Err(std::env::VarError::NotPresent)).unwrap();
        assert_eq!(
            client.list_models().await.unwrap(),
            vec!["declared-a".to_string(), "declared-b".to_string()],
            "ids, in declaration order"
        );
    }

    /// The ruler follows the endpoint switch: a provider's declared window
    /// must not measure another provider's model (#40's root cause was one
    /// measurement site disagreeing with another).
    #[test]
    fn model_resolver_follows_the_provider_switch() {
        let settings: crate::settings::Settings = serde_json::from_str(
            r#"{
                "apiKey": "sk-main",
                "providers": {
                    "narrow": {"apiKey": "k", "models": [{"id": "claude-sonnet-5",
                                                          "contextWindow": 32000}]}
                }
            }"#,
        )
        .unwrap();
        let client =
            Client::from_settings_with(&settings, |_| Err(std::env::VarError::NotPresent)).unwrap();
        assert_eq!(
            client.models().context_window("claude-sonnet-5"),
            200_000,
            "default provider: the prefix table"
        );
        client.set_provider("narrow").unwrap();
        assert_eq!(client.models().context_window("claude-sonnet-5"), 32_000);
        // A forked client pins its own provider's ruler.
        let fork = client.with_provider("default").unwrap();
        assert_eq!(fork.models().context_window("claude-sonnet-5"), 200_000);
        assert_eq!(client.models().context_window("claude-sonnet-5"), 32_000);
    }

    /// P5 (D34): with empty settings the built-in presets are visible with correct endpoints/auth; an apiKey-style
    /// preset (opencode-go) with no key configured gets an empty auth string (leading to a 401).
    #[test]
    fn preset_visibility_zero_config() {
        let tmp = std::env::temp_dir().join(format!("bingo-preset-vis-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        let client =
            Client::from_settings_at_with(&settings, |_| Err(std::env::VarError::NotPresent), &tmp)
                .unwrap();
        assert_eq!(
            client.provider_names(),
            vec!["codex", "opencode-go"],
            "presets visible with zero config"
        );
        assert!(client.is_preset("codex"));
        assert!(client.is_preset("opencode-go"));
        assert!(!client.is_preset("default"));
        // codex: preset endpoint + OAuth (not logged in).
        assert_eq!(
            client.provider_endpoint("codex").unwrap().1,
            "https://chatgpt.com/backend-api",
            "codex preset endpoint"
        );
        assert!(
            matches!(
                client.auth_status("codex"),
                Some(crate::api::contract::AuthStatus::OAuth { account: None })
            ),
            "codex preset not logged in under an isolated home"
        );
        // opencode-go: apiKey-style; not configured → StoredKey (auth.json read live,
        // logging in this session immediately flips it to configured).
        assert_eq!(
            client.provider_endpoint("opencode-go").unwrap().1,
            "https://opencode.ai/zen/go"
        );
        assert!(matches!(
            client.auth_status("opencode-go"),
            Some(crate::api::contract::AuthStatus::StoredKey { configured: false })
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// P5 merge matrix: the user only overrides apiBaseUrl → protocol/oauth fall back to the preset.
    #[test]
    fn preset_field_merge_keeps_template() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        settings.providers.insert(
            "codex".to_string(),
            crate::settings::ProviderConfig {
                env_key: None,
                models: None,
                api_key: None,
                api_base_url: "https://custom.example".into(),
                supports_images: None,
                protocol: None,
                oauth: None,
            },
        );
        let client =
            Client::from_settings_with(&settings, |_| Err(std::env::VarError::NotPresent)).unwrap();
        assert_eq!(
            client.provider_endpoint("codex").unwrap().1,
            "https://custom.example",
            "user overrides apiBaseUrl"
        );
        assert!(
            matches!(
                client.auth_status("codex"),
                Some(crate::api::contract::AuthStatus::OAuth { .. })
            ),
            "oauth falls back to the preset (no need to restate)"
        );
        assert_eq!(
            client.provider_protocol("codex").as_deref(),
            Some("openai"),
            "protocol falls back to the preset"
        );
    }

    /// P5 zero pollution: presets are not written back to settings (the user config file stays untouched).
    #[test]
    fn presets_do_not_pollute_settings() {
        let settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            ..Default::default()
        };
        let _client =
            Client::from_settings_with(&settings, |_| Err(std::env::VarError::NotPresent)).unwrap();
        assert!(
            settings.providers.is_empty(),
            "presets are a runtime concept; settings stay user-private"
        );
    }

    /// supports_images: default reads the top-level sendImages; named providers read their own
    /// supportsImages; it follows the endpoint switch.
    #[test]
    fn supports_images_follows_endpoint_switch() {
        let mut settings = crate::settings::Settings {
            api_key: Some("sk-main".into()),
            send_images: Some(true),
            ..Default::default()
        };
        settings.providers.insert(
            "vision".to_string(),
            crate::settings::ProviderConfig {
                env_key: None,
                models: None,
                api_key: Some("sk-v".into()),
                api_base_url: "https://vision.example".into(),
                supports_images: Some(true),
                protocol: None,
                oauth: None,
            },
        );
        settings.providers.insert(
            "text-only".to_string(),
            crate::settings::ProviderConfig {
                env_key: None,
                models: None,
                api_key: Some("sk-t".into()),
                api_base_url: "https://text.example".into(),
                supports_images: Some(false),
                protocol: None,
                oauth: None,
            },
        );
        let env = |_name: &str| Err(std::env::VarError::NotPresent);
        let client = Client::from_settings_with(&settings, env).unwrap();
        assert!(
            client.supports_images(),
            "default reads the top-level sendImages"
        );

        client.set_provider("text-only").unwrap();
        assert!(!client.supports_images(), "explicit false overrides");
        client.set_provider("vision").unwrap();
        assert!(client.supports_images(), "supportsImages=true takes effect");
    }
}

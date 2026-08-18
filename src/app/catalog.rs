//! What there is to choose from, answerable before a session exists.
//!
//! A GUI picks its provider and its model on the way in, which is the job
//! `--inspect` used to have (Amendment #7). So the catalogs are read from
//! settings and the two directories rather than from a running session: no
//! transcript, no system prompt, no team, no MCP connect, and no network — a
//! provider that declared no models answers from its own day-old disk cache
//! rather than asking the endpoint, because a read must not have a side effect
//! and must not hang.
//!
//! Two of the five have a live half a session adds: MCP servers know their
//! connection state only once something has connected, and images exist only
//! once something has registered them. Both are honest when empty — "nothing
//! connected" and "no images" are the truth before a session starts.
//!
//! **No credential ever leaves here.** `Client::provider_endpoint` returns the
//! plaintext key beside the base URL; this module takes the URL and drops the
//! key on the floor, reporting presence, source and status instead (spec
//! "Errors, load, and security").

use std::path::{Path, PathBuf};

use crate::app::snapshot::{
    Catalog, CatalogKind, CredentialSource, CredentialState, CredentialStatus, ImageInfo,
    McpServerState, McpStatus, ModelInfo, Page, ProviderInfo, SkillInfo, SkillSource,
};
use crate::settings::Settings;

/// The reserved name of the endpoint configured at the top level of settings.
pub const DEFAULT_PROVIDER: &str = "default";

/// Everything the catalogs are read from.
///
/// It owns its own `Client` because building one is synchronous, network-free,
/// and exactly what `--inspect` did before the transcript existed.
#[derive(Clone)]
pub struct CatalogSource {
    settings: Settings,
    home: PathBuf,
    user_dir: PathBuf,
    cwd: PathBuf,
    client: Option<crate::api::client::Client>,
}

impl std::fmt::Debug for CatalogSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogSource")
            .field("home", &self.home)
            .field("cwd", &self.cwd)
            .finish_non_exhaustive()
    }
}

impl Default for CatalogSource {
    fn default() -> Self {
        Self {
            settings: Settings::default(),
            home: PathBuf::new(),
            user_dir: PathBuf::new(),
            cwd: PathBuf::new(),
            client: None,
        }
    }
}

/// The live half a running session contributes: what MCP has actually
/// connected, and what has actually been registered.
#[derive(Debug, Default)]
pub struct Live<'a> {
    /// Absent means nothing has reported: the settings answer stands, which
    /// says "configured, not connected".
    pub mcp: Option<&'a [McpServerState]>,
    pub images: &'a [ImageInfo],
}

impl CatalogSource {
    /// Read the catalogs of one project, with no session at all.
    pub fn load(home: &Path, user_dir: &Path, cwd: &Path, settings: Settings) -> Self {
        // A settings file the endpoint table refuses leaves the catalogs empty
        // rather than failing the read: a GUI asking what there is to choose
        // from before a session exists is exactly the caller that cannot act on
        // an error here.
        let client = crate::api::client::Client::from_settings_at(&settings, home).ok();
        Self {
            settings,
            home: home.to_path_buf(),
            user_dir: user_dir.to_path_buf(),
            cwd: cwd.to_path_buf(),
            client,
        }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn user_dir(&self) -> &Path {
        &self.user_dir
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = cwd;
    }

    /// Re-read the catalogs after settings changed under them.
    pub fn reload(&mut self, settings: Settings) {
        self.client = crate::api::client::Client::from_settings_at(&settings, &self.home).ok();
        self.settings = settings;
    }

    /// One page of one catalog.
    pub fn page(
        &self,
        kind: CatalogKind,
        provider: Option<&str>,
        cursor: Option<&str>,
        limit: Option<u32>,
        live: &Live<'_>,
    ) -> Catalog {
        match kind {
            CatalogKind::Providers => Catalog::Providers(page(self.providers(), cursor, limit)),
            CatalogKind::Models => Catalog::Models(page(self.models(provider), cursor, limit)),
            CatalogKind::Skills => Catalog::Skills(page(self.skills(), cursor, limit)),
            CatalogKind::McpServers => {
                Catalog::McpServers(page(self.mcp_servers(live.mcp), cursor, limit))
            }
            CatalogKind::Images => Catalog::Images(page(live.images.to_vec(), cursor, limit)),
        }
    }

    /// The providers, in the order `/provider` lists them: the top-level
    /// endpoint, then the built-in presets, then the user's own.
    pub fn providers(&self) -> Vec<ProviderInfo> {
        let Some(client) = &self.client else {
            return Vec::new();
        };
        let mut names = vec![DEFAULT_PROVIDER.to_string()];
        let (mut builtin, mut declared): (Vec<String>, Vec<String>) = client
            .provider_names()
            .into_iter()
            .partition(|name| client.is_preset(name));
        builtin.sort();
        declared.sort();
        names.extend(builtin);
        names.extend(declared);
        let image_capable = client.image_capable_providers();
        names
            .into_iter()
            .filter_map(|name| {
                // The key half of the endpoint pair is deliberately dropped
                // here and nowhere else: this is the one place a snapshot could
                // have carried it.
                let (_key, api_base_url) = client.provider_endpoint(&name)?;
                Some(ProviderInfo {
                    protocol: client
                        .provider_protocol(&name)
                        .unwrap_or_else(|| "anthropic".to_string()),
                    api_base_url,
                    builtin: client.is_preset(&name),
                    supports_images: name == DEFAULT_PROVIDER
                        && self.settings.send_images != Some(false)
                        || image_capable.contains(&name),
                    credential: self.credential(client, &name),
                    name,
                })
            })
            .collect()
    }

    /// Presence, source and status — never a value.
    fn credential(&self, client: &crate::api::client::Client, name: &str) -> CredentialState {
        use crate::api::contract::AuthStatus;
        let configured = client.is_configured(name);
        let declared_in_settings = if name == DEFAULT_PROVIDER {
            self.settings.api_key.is_some()
        } else {
            self.settings
                .providers
                .get(name)
                .is_some_and(|provider| provider.api_key.is_some())
        };
        let source = match client.auth_status(name) {
            Some(AuthStatus::ApiKey) if declared_in_settings => CredentialSource::Settings,
            Some(AuthStatus::ApiKey) => CredentialSource::Environment,
            Some(AuthStatus::StoredKey { .. }) | Some(AuthStatus::OAuth { .. }) => {
                CredentialSource::OauthStore
            }
            Some(AuthStatus::Unconfigured) | None => CredentialSource::None,
        };
        CredentialState {
            configured,
            source,
            status: if configured {
                CredentialStatus::Present
            } else {
                CredentialStatus::Missing
            },
        }
    }

    /// The models of one provider — the current one when none is named.
    ///
    /// A settings declaration is authoritative (D65). Otherwise the day-old disk
    /// cache answers, because reading a catalog must not reach the network.
    pub fn models(&self, provider: Option<&str>) -> Vec<ModelInfo> {
        let Some(client) = &self.client else {
            return Vec::new();
        };
        let provider = provider
            .map(str::to_string)
            .or_else(|| self.settings.provider.clone())
            .unwrap_or_else(|| DEFAULT_PROVIDER.to_string());
        let resolver = match client.with_provider(&provider) {
            Ok(scoped) => scoped.models(),
            Err(_) => client.models(),
        };
        let describe = |id: String, display: Option<String>| {
            let meta = resolver.meta(&id);
            ModelInfo {
                family: family_of(&id),
                display_name: display.filter(|name| *name != id),
                context_window: Some(meta.context_window),
                supports_images: meta.supports_vision,
                supports_thinking: meta.supports_thinking,
                provider: provider.clone(),
                id,
            }
        };
        if let Some(declared) = client.declared_models(&provider) {
            return declared
                .into_iter()
                .map(|entry| {
                    let display = entry.display.clone();
                    describe(entry.id, display)
                })
                .collect();
        }
        let Some((_key, base_url)) = client.provider_endpoint(&provider) else {
            return Vec::new();
        };
        crate::model_cache::ModelCache::new(&self.home)
            .get(&provider, &base_url)
            .filter(crate::model_cache::CachedModels::fresh)
            .map(|cached| {
                cached
                    .models
                    .into_iter()
                    .map(|id| describe(id, None))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The skills this project and this user have.
    pub fn skills(&self) -> Vec<SkillInfo> {
        crate::skills::load_skills(&self.home, &self.cwd)
            .into_iter()
            .map(|skill| SkillInfo {
                source: skill_source(&skill, &self.home),
                name: skill.name,
                description: skill.description,
            })
            .collect()
    }

    /// The MCP servers settings configures, overlaid with whatever the manager
    /// has reported. Reading this never connects one.
    pub fn mcp_servers(&self, live: Option<&[McpServerState]>) -> Vec<McpServerState> {
        let mut names: Vec<&String> = self.settings.mcp_servers.keys().collect();
        names.sort();
        names
            .into_iter()
            .map(|name| {
                let enabled = !self.settings.disabled_mcp_servers.contains(name);
                live.and_then(|states| states.iter().find(|state| state.name == *name))
                    .cloned()
                    .unwrap_or(McpServerState {
                        name: name.clone(),
                        enabled,
                        status: McpStatus::Disconnected,
                        tools: 0,
                        error: None,
                    })
            })
            .collect()
    }
}

/// The family a model id belongs to: the longest prefix the built-in table
/// knows. It is what the metadata was resolved through, so a client showing
/// "claude-" beside a model is showing the rule that answered for it.
fn family_of(id: &str) -> Option<String> {
    crate::api::models::builtin_families()
        .iter()
        .filter(|(prefix, _)| id.starts_with(prefix))
        .max_by_key(|(prefix, _)| prefix.len())
        .map(|(prefix, _)| (*prefix).to_string())
}

/// Where a skill came from, read from where it lives. A bundled skill has no
/// directory at all; a user skill sits under the config directory; anything
/// else was found by walking up from the working directory.
fn skill_source(skill: &crate::skills::Skill, home: &Path) -> SkillSource {
    if skill.base_dir.as_os_str().is_empty() {
        return SkillSource::Bundled;
    }
    let user = crate::skills::user_skills_dir(home);
    if skill.base_dir.starts_with(&user) {
        SkillSource::User
    } else {
        SkillSource::Project
    }
}

/// One page of an already-materialised list.
///
/// The cursor is the offset it resumes at, which is all an in-memory listing
/// needs: these are re-read whole on every call, so there is no generation for a
/// cursor to go stale against.
pub fn page<T: Clone>(items: Vec<T>, cursor: Option<&str>, limit: Option<u32>) -> Page<T> {
    let total = items.len();
    let start = cursor
        .and_then(|cursor| cursor.parse::<usize>().ok())
        .unwrap_or(0)
        .min(total);
    let limit = limit.map_or(DEFAULT_PAGE, |limit| limit.max(1) as usize);
    let end = start.saturating_add(limit).min(total);
    Page {
        items: items[start..end].to_vec(),
        revision: total as u64,
        next_cursor: (end < total).then(|| end.to_string()),
    }
}

/// How much of a catalog a read carries when it does not say.
pub const DEFAULT_PAGE: usize = 100;

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(json: &str) -> Settings {
        serde_json::from_str(json).unwrap_or_else(|error| panic!("settings: {error}"))
    }

    fn source(tag: &str, json: &str) -> (CatalogSource, PathBuf) {
        let home = std::env::temp_dir().join(format!("bingo-catalog-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::create_dir_all(&home);
        let source = CatalogSource::load(&home, &home, &home, settings(json));
        (source, home)
    }

    /// Amendment #7: the catalogs answer before `session/start`, because a GUI
    /// picks its provider and its model on the way in.
    #[test]
    fn the_catalogs_answer_with_no_session_at_all() {
        let (source, home) = source(
            "presession",
            r#"{
                "apiKey": "sk-not-a-real-key",
                "providers": {
                    "house": {
                        "apiKey": "sk-house",
                        "apiBaseUrl": "https://example.invalid",
                        "models": ["house-large", {"id": "house-small", "display": "Small"}]
                    }
                }
            }"#,
        );
        let providers = source.providers();
        let names: Vec<&str> = providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["default", "codex", "opencode-go", "house"],
            "the order /provider lists them in: default, presets, then the user's"
        );
        let models = source.models(Some("house"));
        assert_eq!(
            models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            vec!["house-large", "house-small"],
            "a declaration is authoritative and needs no round trip"
        );
        assert_eq!(models[1].display_name.as_deref(), Some("Small"));
        assert!(models[0].context_window.is_some(), "metadata is resolved");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The one place a key could have reached a snapshot.
    #[test]
    fn a_provider_reports_presence_and_never_a_key() {
        let (source, home) = source("secret", r#"{"apiKey": "sk-secret-value"}"#);
        let providers = source.providers();
        let json = serde_json::to_string(&providers).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            !json.contains("sk-secret-value"),
            "no key material reaches the wire: {json}"
        );
        let default = providers
            .iter()
            .find(|p| p.name == "default")
            .unwrap_or_else(|| panic!("default is listed among {providers:?}"));
        assert!(default.credential.configured);
        assert_eq!(default.credential.source, CredentialSource::Settings);
        assert_eq!(default.credential.status, CredentialStatus::Present);
        let unsigned = providers
            .iter()
            .find(|p| p.name == "codex")
            .unwrap_or_else(|| panic!("the preset is listed"));
        assert!(
            !unsigned.credential.configured,
            "a subscription nobody signed in to is missing, not absent"
        );
        assert_eq!(unsigned.credential.status, CredentialStatus::Missing);
        assert_eq!(
            unsigned.credential.source,
            CredentialSource::OauthStore,
            "where the credential would come from, which is not the same as having one"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Configured is not connected, and reading must not make it so.
    #[test]
    fn an_mcp_server_is_configured_before_it_is_connected() {
        let (source, home) = source(
            "mcp",
            r#"{"mcpServers": {"docs": {"command": "docs-server"}, "off": {"command": "x"}},
                "disabledMcpServers": ["off"]}"#,
        );
        let servers = source.mcp_servers(None);
        assert_eq!(
            servers
                .iter()
                .map(|s| (s.name.as_str(), s.enabled, s.status))
                .collect::<Vec<_>>(),
            vec![
                ("docs", true, McpStatus::Disconnected),
                ("off", false, McpStatus::Disconnected),
            ]
        );
        let live = vec![McpServerState {
            name: "docs".to_string(),
            enabled: true,
            status: McpStatus::Connected,
            tools: 7,
            error: None,
        }];
        let overlaid = source.mcp_servers(Some(&live));
        assert_eq!(overlaid[0].status, McpStatus::Connected);
        assert_eq!(overlaid[0].tools, 7);
        assert_eq!(
            overlaid[1].status,
            McpStatus::Disconnected,
            "a server nothing reported keeps the settings answer"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_page_resumes_where_the_cursor_left_off() {
        let items: Vec<u32> = (0..5).collect();
        let first = page(items.clone(), None, Some(2));
        assert_eq!(first.items, vec![0, 1]);
        assert_eq!(first.next_cursor.as_deref(), Some("2"));
        assert_eq!(first.revision, 5, "the revision is what there is");
        let second = page(items.clone(), first.next_cursor.as_deref(), Some(2));
        assert_eq!(second.items, vec![2, 3]);
        let last = page(items, second.next_cursor.as_deref(), Some(2));
        assert_eq!(last.items, vec![4]);
        assert_eq!(last.next_cursor, None, "the end says so");
    }

    #[test]
    fn a_bundled_skill_says_where_it_came_from() {
        let (source, home) = source("skills", "{}");
        let skills = source.skills();
        assert!(
            skills
                .iter()
                .any(|skill| skill.name == "guide" && skill.source == SkillSource::Bundled),
            "the bundled skill is listed as bundled: {skills:?}"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}

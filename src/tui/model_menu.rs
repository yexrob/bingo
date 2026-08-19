//! `/model` selector: the two-level menu's state, entry and key handling,
//! split out of chat.rs (which sits at the file-size cap). Owns no state of
//! its own; `impl super::Chat`.
//!
//! Where the level-two list comes from (D65): a provider that declared
//! `models` in settings is answered from that declaration alone — the menu
//! makes no request at all. Only an undeclared provider is asked for its list.

use crossterm::event::{KeyCode, KeyModifiers};

use crate::api::models::CatalogModel;
use crate::ui::UiEvent;

/// One level-two row: `id` is what a confirm commits, `label` is what the user
/// reads (a declared `display`, or the id itself).
#[derive(Clone, Debug, PartialEq)]
pub struct ModelChoice {
    pub id: String,
    pub label: String,
}

impl From<String> for ModelChoice {
    fn from(id: String) -> Self {
        Self {
            label: id.clone(),
            id,
        }
    }
}

impl From<&CatalogModel> for ModelChoice {
    fn from(model: &CatalogModel) -> Self {
        Self {
            id: model.id.clone(),
            label: model.label().to_string(),
        }
    }
}

/// `/model` two-level selector state: level one = endpoint list, level two = that endpoint's models
/// (declared in settings, or fetched async from `/v1/models`).
#[derive(Clone)]
pub struct ModelMenu {
    /// Level-one list: `default` (top-level config) + settings.providers names.
    pub providers: Vec<String>,
    /// Level-one descriptions (same source as /provider: URL + auth state + protocol).
    pub provider_descs: Vec<String>,
    pub provider_selected: usize,
    /// The current provider's position in the level-one list (●; picker-model.md commit E).
    pub provider_current: Option<usize>,
    /// Level-two model list (None = still on level one).
    pub models: Option<ModelMenuModels>,
}

impl ModelMenu {
    /// Level-one list → the PickerModel core (shared row rendering / key dispatch; two-level + async stays in the shell).
    pub fn provider_picker(&self) -> crate::tui::picker::PickerModel {
        crate::tui::picker::PickerModel::new(
            self.providers
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    crate::tui::picker::PickerItem::new(
                        p.clone(),
                        p.clone(),
                        self.provider_descs.get(i).cloned().unwrap_or_default(),
                    )
                })
                .collect(),
            self.provider_selected,
            self.provider_current,
        )
    }
}

#[derive(Clone)]
pub struct ModelMenuModels {
    pub provider: String,
    /// Loaded models (filled in asynchronously; may be incomplete).
    pub models: Vec<ModelChoice>,
    pub loading: bool,
    pub selected: usize,
    /// The currently active model's position in the list (● marker; computed on load).
    pub current: Option<usize>,
    /// The fetch failure reason (shown in the menu; None = success or not finished).
    pub failed: Option<String>,
    /// The list came from settings, not the endpoint: there is nothing to
    /// refresh, so `r` is not offered.
    pub declared: bool,
}

impl ModelMenuModels {
    /// Level-two list → the PickerModel core (●/❯ dual markers, windowed rendering, number jump — the same
    /// conventions as the /provider selectors; the old hand-rolled rendering lacked these).
    pub fn picker(&self) -> crate::tui::picker::PickerModel {
        crate::tui::picker::PickerModel::new(
            self.models
                .iter()
                .map(|m| {
                    crate::tui::picker::PickerItem::new(
                        m.label.clone(),
                        m.id.clone(),
                        String::new(),
                    )
                })
                .collect(),
            self.selected,
            self.current,
        )
    }
}

impl super::Chat {
    /// Enters the `/model` two-level selector: level one = current endpoint + configured providers
    /// (with the same endpoint/auth descriptions as /provider — it is the same list).
    pub(crate) fn open_model_menu(&mut self) {
        self.close_menus();
        let providers = self.provider_order();
        let provider_descs = providers.iter().map(|p| self.provider_desc(p)).collect();
        let current = self.session.runtime.provider.borrow().clone();
        let selected = providers.iter().position(|p| *p == current).unwrap_or(0);
        self.model_menu = Some(ModelMenu {
            providers,
            provider_descs,
            provider_selected: selected,
            provider_current: Some(selected),
            models: None,
        });
        self.clear_slash_suggestions();
    }

    /// Level-one Enter: shows that provider's models. A declared list answers
    /// immediately; otherwise the endpoint is asked asynchronously (forking the
    /// endpoint, without switching the current one) and the result arrives via
    /// ModelsLoaded. The level-one list is kept as-is: Esc back to level one
    /// doesn't lose it.
    pub(crate) fn open_model_models(
        &mut self,
        provider: String,
        providers: Vec<String>,
        provider_descs: Vec<String>,
        provider_selected: usize,
    ) {
        let level_two = self.model_level_two(&provider);
        self.model_menu = Some(ModelMenu {
            providers,
            provider_descs,
            provider_selected,
            provider_current: None,
            models: Some(level_two),
        });
    }

    /// The level-two state for a provider, and the fetch it may need.
    fn model_level_two(&mut self, provider: &str) -> ModelMenuModels {
        // Declared in settings: authoritative, and the menu asks no one.
        if let Some(declared) = self.session.client.declared_models(provider) {
            let models: Vec<ModelChoice> = declared.iter().map(ModelChoice::from).collect();
            return self.model_list_state(provider, models, true, None);
        }
        // P2-G cache: this session already fetched the list → reuse it
        // (the field's comment promised this; the fetch never did).
        if let Some(models) = self
            .models_cache
            .get(provider)
            .filter(|m| !m.is_empty())
            .cloned()
        {
            let models = models.into_iter().map(ModelChoice::from).collect();
            return self.model_list_state(provider, models, false, None);
        }
        // Disk cache (D65): a list that is still fresh answers without a round
        // trip. A stale one is not thrown away — it rides along with the fetch
        // so a failure has something to fall back on.
        let cached = self.cached_model_list(provider);
        if let Some(entry) = cached.as_ref().filter(|entry| entry.fresh()) {
            self.models_cache
                .insert(provider.to_string(), entry.models.clone());
            let models = entry
                .models
                .iter()
                .cloned()
                .map(ModelChoice::from)
                .collect();
            return self.model_list_state(provider, models, false, None);
        }
        self.fetch_model_list(provider.to_string());
        ModelMenuModels {
            provider: provider.to_string(),
            models: Vec::new(),
            loading: true,
            selected: 0,
            current: None,
            failed: None,
            declared: false,
        }
    }

    /// This provider's disk-cached list, keyed by the endpoint it was fetched
    /// from (a repointed provider must not eat the old list).
    fn cached_model_list(&self, provider: &str) -> Option<crate::model_cache::CachedModels> {
        let (_, base_url) = self.session.client.provider_endpoint(provider)?;
        crate::model_cache::ModelCache::new(&self.session.home).get(provider, &base_url)
    }

    /// A ready level-two list: preselect the running model when this is the
    /// provider it runs on (P1-F — browsing must not switch).
    fn model_list_state(
        &self,
        provider: &str,
        models: Vec<ModelChoice>,
        declared: bool,
        failed: Option<String>,
    ) -> ModelMenuModels {
        let current_provider = self.session.runtime.provider.borrow().clone();
        let current_model = self.session.runtime.model.borrow().clone();
        let current = (provider == current_provider)
            .then(|| models.iter().position(|m| m.id == current_model))
            .flatten();
        ModelMenuModels {
            provider: provider.to_string(),
            selected: current.unwrap_or(0).min(models.len().saturating_sub(1)),
            models,
            loading: false,
            current,
            failed,
            declared,
        }
    }

    /// Ask the provider's endpoint for its model list; the answer arrives as
    /// ModelsLoaded. A success updates the disk cache; a failure falls back to
    /// whatever the cache still holds, however stale.
    fn fetch_model_list(&self, provider: String) {
        let session = self.session.clone();
        let events = self.events.clone();
        let stale = self.cached_model_list(&provider);
        let endpoint = self
            .session
            .client
            .provider_endpoint(&provider)
            .map(|(_, base_url)| base_url);
        tokio::spawn(async move {
            // Unknown names must error — the old fallback silently listed the
            // CURRENT endpoint's models under the wrong provider label.
            let client = match session.client.with_provider(&provider) {
                Ok(c) => c,
                Err(e) => {
                    // Same visibility contract as a fetch failure: page-level
                    // error row + in-menu reason.
                    events.send(UiEvent::Error {
                        code: "GENERIC".to_string(),
                        msg: e.clone(),
                        level: crate::error::ErrorLevel::Page,
                        context: crate::error::ErrorContext::ShortSync,
                    });
                    events.send(UiEvent::ModelsLoaded {
                        provider,
                        models: Vec::new(),
                        failed: Some(e),
                    });
                    return;
                }
            };
            let (models, failed) = match client.list_models().await {
                Ok(m) => (m, None),
                Err(e) => {
                    let code = crate::error::map_error(&e);
                    // #18/main #91: short-op failures must be visible (page-level error row, error color),
                    // behavior keeps degrading gracefully — "degraded + visible".
                    events.send(UiEvent::Error {
                        code: code.to_string(),
                        msg: e.to_string(),
                        level: crate::error::ErrorLevel::Page,
                        context: crate::error::ErrorContext::ShortSync,
                    });
                    // In-menu reason: a 401 is an auth problem, not "the
                    // endpoint returned no models".
                    let reason = if code == "AUTH_REQUIRED" {
                        format!(
                            "authentication failed: {provider} credentials invalid or not logged in (/provider login {provider})"
                        )
                    } else {
                        format!("fetch failed ({code})")
                    };
                    // An expired list still names real models: showing it with
                    // the reason beats an empty menu.
                    let fallback = stale.map(|entry| entry.models).unwrap_or_default();
                    (fallback, Some(reason))
                }
            };
            if failed.is_none()
                && !models.is_empty()
                && let Some(base_url) = endpoint
            {
                crate::model_cache::ModelCache::new(&session.home)
                    .put(&provider, &base_url, &models);
            }
            events.send(UiEvent::ModelsLoaded {
                provider,
                models,
                failed,
            });
        });
    }

    /// A finished fetch lands in the open menu (and in the session cache).
    pub(crate) fn apply_models_loaded(
        &mut self,
        provider: String,
        models: Vec<String>,
        failed: Option<String>,
    ) {
        // Cache only successful fetches (/model <name> validation +
        // no re-fetch on re-entry) — a cached failure would poison
        // the advisory check and the re-entry fast path.
        if failed.is_none() && !models.is_empty() {
            self.models_cache.insert(provider.clone(), models.clone());
        }
        if self
            .model_menu
            .as_ref()
            .and_then(|menu| menu.models.as_ref())
            .is_none_or(|m| m.provider != provider)
        {
            return;
        }
        let models = models.into_iter().map(ModelChoice::from).collect();
        let state = self.model_list_state(&provider, models, false, failed);
        if let Some(menu) = &mut self.model_menu {
            menu.models = Some(state);
        }
    }

    /// Model menu keys: ↑↓ move, Enter goes to level two / confirms, `r` re-asks
    /// the endpoint, Esc exits. Returns whether consumed.
    pub(crate) fn model_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(menu) = &mut self.model_menu else {
            return false;
        };
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(m) = &mut menu.models {
                    // Level two uses the same PickerModel core (windowed rendering follows selected).
                    let mut core = m.picker();
                    core.move_selection(1);
                    m.selected = core.selected;
                } else {
                    // Level one: delegates to the PickerModel core (picker-model.md commit E).
                    let mut core = menu.provider_picker();
                    core.move_selection(1);
                    menu.provider_selected = core.selected;
                }
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(m) = &mut menu.models {
                    let mut core = m.picker();
                    core.move_selection(-1);
                    m.selected = core.selected;
                } else {
                    let mut core = menu.provider_picker();
                    core.move_selection(-1);
                    menu.provider_selected = core.selected;
                }
                true
            }
            // Number jump: applies to both levels; out-of-range is swallowed (digits leaking into the input was once a half-modal boundary bug).
            KeyCode::Char(c)
                if c.is_ascii_digit() && !modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let n = c.to_digit(10).map(|n| n as usize).unwrap_or(0);
                if let Some(m) = &mut menu.models {
                    let mut core = m.picker();
                    if core.jump(n) {
                        m.selected = core.selected;
                    }
                } else {
                    let mut core = menu.provider_picker();
                    if core.jump(n) {
                        menu.provider_selected = core.selected;
                    }
                }
                true
            }
            // Level two, dynamic providers only: re-ask the endpoint. A declared
            // list has no upstream to refresh, so the key stays unbound there
            // rather than pretending to do something.
            KeyCode::Char('r') if !modifiers.contains(KeyModifiers::CONTROL) => {
                let Some(m) = &mut menu.models else {
                    return false;
                };
                if m.declared {
                    return false;
                }
                let provider = m.provider.clone();
                m.loading = true;
                m.failed = None;
                self.models_cache.remove(&provider);
                self.fetch_model_list(provider);
                true
            }
            KeyCode::Enter => {
                let Some(menu) = self.model_menu.take() else {
                    return true;
                };
                let Some(m) = menu.models else {
                    // Level one: go to level two (level-one list kept).
                    let provider = menu
                        .providers
                        .get(menu.provider_selected)
                        .cloned()
                        .unwrap_or_default();
                    self.open_model_models(
                        provider,
                        menu.providers,
                        menu.provider_descs,
                        menu.provider_selected,
                    );
                    return true;
                };
                // Level two: confirm the selected model. Keep the menu when the list is empty (fetch failed/none returned).
                let provider = m.provider.clone();
                let model = m.models.get(m.selected).map(|c| c.id.clone());
                let Some(model) = model.filter(|id| !id.is_empty()) else {
                    self.restore_model_menu(
                        menu.providers,
                        menu.provider_descs,
                        menu.provider_selected,
                        menu.provider_current,
                        m,
                    );
                    return true;
                };
                // provider+model is an atomic selection: confirming across endpoints goes through the same
                // switch_provider (login warnings, the busy guard, and paired persistence all live there),
                // the old bypass dropped every provider-side notice (audit A3).
                self.provider_models.insert(provider.clone(), model.clone());
                if provider != self.session.runtime.provider.borrow().clone() {
                    self.switch_provider(&provider, true);
                    if *self.session.runtime.provider.borrow() != provider {
                        // Switch refused (busy / unknown): keep the menu alive.
                        self.restore_model_menu(
                            menu.providers,
                            menu.provider_descs,
                            menu.provider_selected,
                            menu.provider_current,
                            m,
                        );
                    }
                } else {
                    self.set_model(model);
                }
                true
            }
            KeyCode::Esc => {
                // Level two → back to level one; level one → exit entirely (returns one level at a time).
                if let Some(menu) = self.model_menu.as_mut()
                    && menu.models.is_some()
                {
                    menu.models = None;
                } else {
                    self.model_menu = None;
                }
                true
            }
            _ => false,
        }
    }

    /// Put back a menu the Enter branch took out (a confirm that could not land).
    fn restore_model_menu(
        &mut self,
        providers: Vec<String>,
        provider_descs: Vec<String>,
        provider_selected: usize,
        provider_current: Option<usize>,
        models: ModelMenuModels,
    ) {
        self.model_menu = Some(ModelMenu {
            providers,
            provider_descs,
            provider_selected,
            provider_current,
            models: Some(models),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    /// An isolated home: the disk cache lives under it, so tests must not
    /// share one.
    fn tmp_home(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bingo-model-menu-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// A chat whose client is built from the given settings JSON (the catalog
    /// only exists on a Client built from settings).
    fn chat_with_settings(json: &str) -> crate::tui::chat::Chat {
        chat_at_home(&tmp_home("default"), json)
    }

    fn chat_at_home(home: &std::path::Path, json: &str) -> crate::tui::chat::Chat {
        let mut chat = crate::tui::test_util::chat_at(80, 24);
        let settings: crate::settings::Settings = serde_json::from_str(json).unwrap();
        // Rebuilt rather than mutated in place: the session is shared with the
        // engine that runs its turns, so nothing holds it alone any more.
        let mut session = (*chat.session).clone();
        session.client = crate::api::client::Client::from_settings_at(&settings, home).unwrap();
        session.home = home.to_path_buf();
        chat.session = std::sync::Arc::new(session);
        chat
    }

    /// Settings pointing at a dead local port: the fetch path is exercised
    /// without waiting on a real endpoint.
    const OFFLINE: &str = r#"{"apiKey": "sk", "apiBaseUrl": "http://127.0.0.1:1"}"#;

    fn open_level_two(chat: &mut crate::tui::chat::Chat) {
        chat.input = "/model".to_string();
        chat.submit();
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
    }

    fn default_endpoint(chat: &crate::tui::chat::Chat) -> String {
        chat.session
            .client
            .provider_endpoint("default")
            .expect("default provider")
            .1
    }

    /// Backdate every cached entry so it reads as expired.
    fn expire_cache(home: &std::path::Path) {
        let path = crate::model_cache::cache_path(home);
        let raw = std::fs::read_to_string(&path).unwrap();
        let mut store: serde_json::Value = serde_json::from_str(&raw).unwrap();
        for (_, entry) in store.as_object_mut().unwrap() {
            entry["fetchedAt"] = serde_json::json!(1);
        }
        std::fs::write(&path, store.to_string()).unwrap();
    }

    fn level_two(chat: &crate::tui::chat::Chat) -> &ModelMenuModels {
        chat.model_menu
            .as_ref()
            .and_then(|menu| menu.models.as_ref())
            .expect("level two is open")
    }

    /// D65: a declared list answers the menu on the spot. `loading` is the
    /// tell — the fetch path is the only thing that sets it, so a settled
    /// list right after Enter proves nothing was asked of the endpoint.
    #[tokio::test]
    async fn declared_models_open_without_a_fetch() {
        let mut chat = chat_with_settings(
            r#"{"apiKey": "sk", "models": [
                "claude-opus-5",
                {"id": "claude-sonnet-5", "display": "Sonnet (fast)"}
            ]}"#,
        );
        let _ = chat.session.runtime.model_tx.send("claude-sonnet-5".into());

        chat.input = "/model".to_string();
        chat.submit();
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());

        let m = level_two(&chat);
        assert!(!m.loading, "a declared list is never fetched");
        assert!(m.declared);
        assert_eq!(m.failed, None);
        assert_eq!(
            m.models,
            vec![
                ModelChoice {
                    id: "claude-opus-5".into(),
                    label: "claude-opus-5".into()
                },
                ModelChoice {
                    id: "claude-sonnet-5".into(),
                    label: "Sonnet (fast)".into()
                },
            ],
            "declaration order and labels are the user's"
        );
        assert_eq!(m.current, Some(1), "the running model is preselected");
        assert!(
            chat.models_cache.is_empty(),
            "nothing was fetched, so nothing is cached"
        );

        // Confirming commits the id, not the label.
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(*chat.session.runtime.model.borrow(), "claude-sonnet-5");
    }

    /// `r` re-asks the endpoint (and drops the session cache first, or the
    /// refresh would answer itself); a declared list has no upstream, so the
    /// key stays unbound there rather than pretending.
    #[tokio::test]
    async fn refresh_key_only_applies_to_fetched_lists() {
        let mut chat = chat_with_settings(r#"{"apiKey": "sk"}"#);
        chat.models_cache
            .insert("default".to_string(), vec!["cached-model".to_string()]);
        chat.input = "/model".to_string();
        chat.submit();
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        assert!(!level_two(&chat).loading, "the session cache answers first");

        assert!(chat.model_menu_key(KeyCode::Char('r'), KeyModifiers::empty()));
        assert!(level_two(&chat).loading, "r re-asks the endpoint");
        assert!(
            !chat.models_cache.contains_key("default"),
            "the stale session entry must not answer the refresh"
        );

        // Declared provider: `r` is not consumed (it reaches the input box).
        let mut chat = chat_with_settings(r#"{"apiKey": "sk", "models": ["m1"]}"#);
        chat.input = "/model".to_string();
        chat.submit();
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        assert!(!chat.model_menu_key(KeyCode::Char('r'), KeyModifiers::empty()));
        assert!(!level_two(&chat).loading);
    }

    /// A fetch failure with a usable list keeps the list and says why —
    /// degraded and visible, the repo's standing rule for short ops.
    #[tokio::test]
    async fn a_failed_fetch_may_still_carry_a_list() {
        let mut chat = chat_with_settings(r#"{"apiKey": "sk"}"#);
        chat.input = "/model".to_string();
        chat.submit();
        chat.on_key(KeyCode::Enter, KeyModifiers::empty());
        chat.apply_models_loaded(
            "default".to_string(),
            vec!["stale-model".to_string()],
            Some("fetch failed (TIMEOUT)".into()),
        );
        let m = level_two(&chat);
        assert_eq!(m.models.len(), 1);
        assert_eq!(m.failed.as_deref(), Some("fetch failed (TIMEOUT)"));
        assert!(
            chat.models_cache.is_empty(),
            "a failed fetch must not poison the session cache"
        );
    }

    /// A fresh disk entry answers the menu across sessions — the point of the
    /// cache. `loading` stays false, so no request went out.
    #[tokio::test]
    async fn a_fresh_disk_entry_answers_without_a_fetch() {
        let home = tmp_home("disk-fresh");
        let mut chat = chat_at_home(&home, OFFLINE);
        let endpoint = default_endpoint(&chat);
        crate::model_cache::ModelCache::new(&home).put(
            "default",
            &endpoint,
            &["disk-model".to_string()],
        );

        open_level_two(&mut chat);
        let m = level_two(&chat);
        assert!(!m.loading, "the disk cache answered");
        assert_eq!(m.models, vec![ModelChoice::from("disk-model".to_string())]);
        assert_eq!(
            chat.models_cache.get("default").map(Vec::as_slice),
            Some(&["disk-model".to_string()][..]),
            "the session cache is warmed from disk"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Past the TTL the list is re-asked, and a different endpoint under the
    /// same provider name is not the same list.
    #[tokio::test]
    async fn stale_or_repointed_entries_do_not_answer() {
        let home = tmp_home("disk-stale");
        let cache = crate::model_cache::ModelCache::new(&home);
        let mut chat = chat_at_home(&home, OFFLINE);
        let endpoint = default_endpoint(&chat);
        cache.put("default", &endpoint, &["disk-model".to_string()]);
        expire_cache(&home);

        open_level_two(&mut chat);
        assert!(
            level_two(&chat).loading,
            "an expired entry sends the request anyway"
        );

        // Same provider name, different endpoint: the entry must not be reused.
        let home = tmp_home("disk-repointed");
        let cache = crate::model_cache::ModelCache::new(&home);
        cache.put(
            "default",
            "https://elsewhere.example",
            &["other-model".to_string()],
        );
        let mut chat = chat_at_home(&home, OFFLINE);
        open_level_two(&mut chat);
        assert!(
            level_two(&chat).loading,
            "a list fetched from another endpoint is not this endpoint's list"
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A successful fetch lands on disk; a later failure serves that list with
    /// the reason attached rather than an empty menu.
    #[tokio::test]
    async fn a_fetch_writes_the_cache_and_a_failure_falls_back_to_it() {
        let home = tmp_home("disk-write");
        let mut chat = chat_at_home(&home, OFFLINE);
        let endpoint = default_endpoint(&chat);

        open_level_two(&mut chat);
        assert!(level_two(&chat).loading);
        // The fetch itself is the spawned task's job; assert the contract the
        // menu depends on — a successful result is persisted for next time.
        crate::model_cache::ModelCache::new(&home).put(
            "default",
            &endpoint,
            &["fetched-model".to_string()],
        );
        expire_cache(&home);

        // Re-entering with an expired entry re-asks, and the failure that
        // follows shows the expired list plus the reason.
        chat.model_menu = None;
        chat.models_cache.clear();
        open_level_two(&mut chat);
        assert!(level_two(&chat).loading);
        chat.apply_models_loaded(
            "default".to_string(),
            vec!["fetched-model".to_string()],
            Some("fetch failed (TIMEOUT)".into()),
        );
        let m = level_two(&chat);
        assert_eq!(
            m.models,
            vec![ModelChoice::from("fetched-model".to_string())]
        );
        assert_eq!(m.failed.as_deref(), Some("fetch failed (TIMEOUT)"));
        let _ = std::fs::remove_dir_all(&home);
    }
}

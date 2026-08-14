//! Tail half of [`Chat`]'s methods (split out of chat.rs, #8):
//! provider/think/skills menus, key handling, turn submission and the
//! input/editing surface. Owns no state; `impl super::Chat`.

use super::*;
use crossterm::event::{KeyCode, KeyModifiers};
use tokio::sync::mpsc;

use crate::query::Session;
use crate::tui::composer::KillDir;
use crate::ui::UiEvent;

/// Everything Esc can dismiss, in the order it dismisses them (D80). The key
/// handler walks [`EscLayer::ORDER`] top-down, acts on the first layer that is
/// open and stops: one press closes one level.
///
/// The busy interrupt sits *inside* the list rather than above it, and that
/// placement is the whole point. A dropdown, an info block or the `?` panel
/// opened over a running turn is what the user is looking at when they reach
/// for Esc; closing the turn instead was answering a question nobody asked.
/// Ctrl+C keeps the unconditional interrupt — the layers do not shield it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscLayer {
    /// Permission / question dialog: Esc denies it.
    AskDialog,
    /// `/model` `/think` `/theme` `/resume` `/provider` pickers (one level per press).
    Menu,
    /// The ctrl+b background-agent overlay (detail returns to the list, the list closes).
    AgentManager,
    /// Slash-command dropdown: closes, and takes a bare `/` query with it.
    SlashDropdown,
    /// The `@` mention dropdown: closes, leaving the typed token alone.
    /// Same stratum as [`EscLayer::SlashDropdown`] — both are transient
    /// completion surfaces over the composer, and the two are mutually
    /// exclusive, so their relative order is a formality.
    MentionDropdown,
    /// ctrl+r history search: cancels without adopting the hit.
    Search,
    /// A page/field-level error row above the prompt.
    ErrorRow,
    /// Info-tier lines (`/help` `/status` … output that persists until dismissed).
    InfoLines,
    /// The `?` shortcut panel.
    HelpPanel,
    /// The task panel, when the user opened it themselves with ctrl+t.
    TaskPanel,
    /// The running turn.
    Interrupt,
    /// Bash mode on an empty input. Below the interrupt, unlike every other
    /// layer, because the `!` prefix is sticky: a running bash command always
    /// sits under an empty bash-mode composer, and Esc there has to reach the
    /// command rather than the prompt prefix.
    BashMode,
    /// A non-empty input: esc-esc clears it into history.
    ClearInput,
}

impl EscLayer {
    /// The stack, top first. The single source for Esc's priority.
    pub const ORDER: [EscLayer; 13] = [
        EscLayer::AskDialog,
        EscLayer::Menu,
        EscLayer::AgentManager,
        EscLayer::SlashDropdown,
        EscLayer::MentionDropdown,
        EscLayer::Search,
        EscLayer::ErrorRow,
        EscLayer::InfoLines,
        EscLayer::HelpPanel,
        EscLayer::TaskPanel,
        EscLayer::Interrupt,
        EscLayer::BashMode,
        EscLayer::ClearInput,
    ];
}

impl super::Chat {
    /// Switch the provider: takes effect in the runtime immediately; persist=true writes settings (restored on restart).
    ///
    /// provider+model is an atomic selection: switching endpoints must resolve the model (this session's last-used →
    /// endpoint default → keep + warn) — the same rule as the subagent cross-provider check;
    /// the main session used to keep the old model silently, and the next message 404'd on the new endpoint.
    pub(crate) fn switch_provider(&mut self, name: &str, persist: bool) {
        // A mid-turn protocol swap would send this conversation's accumulated
        // thinking/reasoning blocks to the other protocol's endpoint — refuse
        // instead of corrupting the running turn.
        if self.busy {
            self.push_slash_error(
                "[error] code=BUSY msg=cannot switch providers mid-turn (press Esc to interrupt, then retry)".to_string(),
            );
            return;
        }
        let session = self.session.clone();
        let name = name.to_string();
        match session.client.set_provider(&name) {
            Ok(()) => {
                let (_, url) = session.client.current_endpoint();
                let prev_provider = session.runtime.provider.borrow().clone();
                let prev_model = session.runtime.model.borrow().clone();
                if prev_provider != name {
                    self.provider_models
                        .insert(prev_provider, prev_model.clone());
                }
                let _ = session.runtime.provider_tx.send(name.clone());
                let resolved = self
                    .provider_models
                    .get(&name)
                    .cloned()
                    .or_else(|| session.client.provider_default_model(&name));
                let model_note = match &resolved {
                    Some(model) if *model != prev_model => {
                        let _ = session.runtime.model_tx.send(model.clone());
                        format!(" · model {model}")
                    }
                    Some(_) => String::new(),
                    None => format!(
                        " · ⚠ model {prev_model} may not be available on this endpoint (pick with /model)"
                    ),
                };
                let model_now = session.runtime.model.borrow().clone();
                self.context_usage = crate::context_usage::ContextUsage::for_model(
                    self.context_usage.used,
                    &session.client.models(),
                    &model_now,
                );
                self.provider_models.insert(name.clone(), model_now.clone());
                self.provider_session_only = !persist;
                if persist {
                    // Same path as the /model menu: provider+model persist as a pair.
                    self.persist_selection(&model_now, &name);
                    self.push_slash_output(format!(
                        "✓ provider switched: {name} ({url}){model_note}"
                    ));
                } else {
                    self.push_slash_output(format!(
                        "✓ provider switched: {name} ({url}){model_note} (this session only)"
                    ));
                }
                // Credentials unavailable: the switch succeeds but the first request would fail — guide early (the criterion is
                // "are credentials available", not just the auth kind — a keyless apiKey-style
                // preset used to pass silently).
                match session.client.auth_status(&name) {
                    Some(crate::api::contract::AuthStatus::OAuth { account: None }) => {
                        self.push_slash_output(format!(
                            "⚠ {name} not logged in: /provider login {name}"
                        ));
                    }
                    Some(crate::api::contract::AuthStatus::StoredKey { configured: false }) => {
                        self.push_slash_output(format!(
                            "⚠ {name} has no API key configured: /provider login {name} --manual <key>"
                        ));
                    }
                    Some(crate::api::contract::AuthStatus::Unconfigured) => {
                        self.push_slash_output(
                            "⚠ default has no credentials: write apiKey in settings or /provider login codex"
                                .to_string(),
                        );
                    }
                    _ => {}
                }
            }
            Err(e) => self.push_slash_error(e),
        }
    }

    /// Option description: URL + redacted key (first 4 chars; short keys get no ellipsis — following the existing info column).
    pub(crate) fn provider_desc(&self, name: &str) -> String {
        let (key, url) = self
            .session
            .client
            .provider_endpoint(name)
            .unwrap_or_else(|| (None, "?".to_string()));
        let protocol = self
            .session
            .client
            .provider_protocol(name)
            .unwrap_or_default();
        let auth = match self.session.client.auth_status(name) {
            Some(crate::api::contract::AuthStatus::ApiKey) => {
                let key = key.unwrap_or_default();
                if key.is_empty() {
                    format!("○ not configured (/provider login {name})")
                } else {
                    let mut key_shown: String = key.chars().take(4).collect();
                    if key.chars().count() > 4 {
                        key_shown.push('…');
                    }
                    format!("key {key_shown}")
                }
            }
            // The key in auth.json (--manual): configured reads live — logging in during this
            // session immediately flips it from "not configured" to "configured".
            Some(crate::api::contract::AuthStatus::StoredKey { configured: true }) => {
                "✓ key (auth.json)".to_string()
            }
            Some(crate::api::contract::AuthStatus::StoredKey { configured: false }) => {
                format!("○ not configured (/provider login {name} --manual <key>)")
            }
            Some(crate::api::contract::AuthStatus::OAuth { account: Some(acc) }) => {
                format!("✓ {acc}")
            }
            Some(crate::api::contract::AuthStatus::OAuth { account: None }) => {
                format!("○ not logged in (/provider login {name})")
            }
            Some(crate::api::contract::AuthStatus::Unconfigured) => {
                "○ not configured (write apiKey in settings or /provider login codex)".to_string()
            }
            None => "?".to_string(),
        };
        let badge = if self.session.client.is_preset(name) {
            " · built-in"
        } else {
            ""
        };
        format!("{url} ({auth} · {protocol}{badge})")
    }

    /// Open the `/provider` selector: default first (the top-level endpoint), then named providers;
    /// ● marks the current provider, other menus close exclusively.
    pub(crate) fn open_provider_menu(&mut self) {
        let current = self.session.runtime.provider.borrow().clone();
        // Order shares a source with /model's level one (provider_order): the number jump means the same in both places.
        let names = self.provider_order();
        let mut options = Vec::with_capacity(names.len());
        for name in names {
            options.push((name.clone(), self.provider_desc(&name)));
        }
        let current = options.iter().position(|(n, _)| *n == current);
        let menu = ProviderMenu {
            selected: current.unwrap_or(0),
            current,
            options,
        };
        if menu.picker().is_empty() {
            return;
        }
        self.close_menus();
        self.provider_menu = Some(menu);
        self.clear_slash_suggestions();
    }

    /// Provider menu keys: ↑↓/1-N move (delegated to the PickerModel core),
    /// Enter switches + persists, s = session-only (no settings write), Esc exits.
    fn provider_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(menu) = &mut self.provider_menu else {
            return false;
        };
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(1);
                menu.selected = core.selected;
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(-1);
                menu.selected = core.selected;
                true
            }
            KeyCode::Char(c)
                if c.is_ascii_digit() && !modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let mut core = menu.picker();
                if let Some(n) = c.to_digit(10)
                    && core.jump(n as usize)
                {
                    menu.selected = core.selected;
                }
                // Swallow even out-of-range digits: a menu is a modal surface —
                // "4" on a 3-item picker used to type a literal 4 into the input.
                true
            }
            KeyCode::Char('s') if !modifiers.contains(KeyModifiers::CONTROL) => {
                let core = menu.picker();
                let value = core
                    .selected_item()
                    .map(|i| i.value.clone())
                    .unwrap_or_default();
                self.provider_menu = None;
                self.switch_provider(&value, false);
                true
            }
            KeyCode::Enter => {
                let core = menu.picker();
                let value = core
                    .selected_item()
                    .map(|i| i.value.clone())
                    .unwrap_or_default();
                self.provider_menu = None;
                self.switch_provider(&value, true);
                true
            }
            KeyCode::Esc => {
                self.provider_menu = None;
                true
            }
            _ => false,
        }
    }

    /// `/provider login <name> [--device-auth|--manual <token>]`: OAuth login
    /// for a provider with an `oauth` config (D33 §6). Default = loopback
    /// PKCE (opens the browser); `--device-auth` prints URL + code and polls
    /// (headless/SSH); `--manual` stores a pasted token (no refresh).
    pub(crate) fn slash_provider_login(&mut self, arg: &str) {
        let parts: Vec<&str> = arg.split_whitespace().collect();
        let Some(name) = parts.first() else {
            self.push_slash_output(
                "usage: /provider login <name> [--device-auth|--manual <token>]".to_string(),
            );
            return;
        };
        let manual = parts
            .iter()
            .position(|p| *p == "--manual")
            .and_then(|i| parts.get(i + 1).copied());
        let device_auth = parts.contains(&"--device-auth");

        let session = self.session.clone();
        // Effective config = user settings ⊕ built-in preset (D34 §6.5):
        // presets make official subscriptions loginable with zero config.
        let preset = crate::api::providers::presets::preset(name);
        let known = session.settings.providers.contains_key(*name) || preset.is_some();
        if !known {
            self.push_slash_error(format!(
                "provider \"{name}\" not found (see /provider for the list)"
            ));
            return;
        }
        let oauth_kind = session
            .settings
            .providers
            .get(*name)
            .and_then(|c| c.oauth.as_ref().map(|o| o.kind.clone()))
            .or_else(|| preset.and_then(|p| p.oauth_kind.map(str::to_string)));
        let name = name.to_string();
        let home = session.home.clone();
        let events = self.events.clone();
        let http = reqwest::Client::new();
        let config = crate::api::auth::OauthFlowConfig::codex();

        // --manual first: works for both apiKey presets (opencode-go, stores
        // auth.json `{type:"api"}`) and oauth presets (pasted access token).
        if let Some(token) = manual {
            let token = token.to_string();
            let api_preset = preset.map(|p| p.oauth_kind.is_none()).unwrap_or(false);
            // Share the session Client's TokenProvider: saving through the
            // same instance updates the adapter's cache + account mirror, so
            // the login takes effect in this session (no restart).
            let shared_tp = session.client.token_provider(&name);
            tokio::spawn(async move {
                if api_preset {
                    let store = crate::auth::AuthStore::new(&home);
                    match store.set(&name, crate::auth::AuthEntry::Api { key: token }) {
                        Ok(()) => {
                            let _ = events.send(UiEvent::SlashOutput(format!(
                                "✓ saved {name}'s API key (subscription key)"
                            )));
                        }
                        Err(e) => {
                            let _ = events.send(UiEvent::SlashError(format!("✗ save failed: {e}")));
                        }
                    }
                    return;
                }
                let tp = shared_tp.unwrap_or_else(|| {
                    std::sync::Arc::new(crate::api::auth::TokenProvider::new(&home, &name, config))
                });
                let tokens = crate::api::auth::TokenSet {
                    access_token: token,
                    refresh_token: String::new(),
                    id_token: None,
                    expires_at: None,
                    account_id: None,
                };
                match tp.save(&tokens).await {
                    Ok(()) => {
                        let _ = events.send(UiEvent::SlashOutput(format!(
                            "✓ saved {name}'s login info (a --manual token does not auto-refresh)"
                        )));
                    }
                    Err(e) => {
                        let _ = events.send(UiEvent::SlashError(format!("✗ save failed: {e}")));
                    }
                }
            });
            return;
        }

        // OAuth gate: codex only in v1; apiKey presets guide the key paste.
        let Some(oauth_kind) = oauth_kind else {
            self.push_slash_info(format!(
                "provider \"{name}\" requires an API key (subscription key):\n  1. get one at opencode.ai/auth\n  2. /provider login {name} --manual <key>"
            ));
            return;
        };
        if oauth_kind != "codex" {
            self.push_slash_error(format!(
                "unsupported oauth.kind \"{oauth_kind}\" (v1 supports only codex)"
            ));
            return;
        }

        if device_auth {
            // headless/SSH: print the URL + one-time code, poll for authorization.
            let shared_tp = session.client.token_provider(&name);
            tokio::spawn(async move {
                let flow = crate::api::auth::DeviceFlow::new(&http, &config);
                match flow.start().await {
                    Ok((prompt, device_auth_id, interval)) => {
                        // Pinned: the code is valid for 15 minutes — it must
                        // stay on screen for all of them (the 2s TTL burned
                        // it before anyone could type it).
                        let _ = events.send(UiEvent::PinPanel {
                            id: "login".to_string(),
                            lines: vec![
                                format!("sign in to {name} (device authorization)"),
                                format!("  1. open {}", prompt.verification_url),
                                format!("  2. enter code {} (valid for 15 minutes)", prompt.user_code),
                                "⏳ waiting for authorization… (Esc will not cancel; the panel disappears when done)".to_string(),
                            ],
                        });
                        let outcome = flow
                            .poll(&device_auth_id, &prompt.user_code, interval)
                            .await;
                        let _ = events.send(UiEvent::Unpin {
                            id: "login".to_string(),
                        });
                        match outcome {
                            Ok(tokens) => {
                                let tp = shared_tp.unwrap_or_else(|| {
                                    std::sync::Arc::new(crate::api::auth::TokenProvider::new(
                                        &home, &name, config,
                                    ))
                                });
                                match tp.save(&tokens).await {
                                    Ok(()) => {
                                        let _ = events.send(UiEvent::SlashOutput(format!(
                                            "✓ signed in to {name}"
                                        )));
                                    }
                                    Err(e) => {
                                        let _ = events.send(UiEvent::SlashOutput(format!(
                                            "✗ save failed: {e}"
                                        )));
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = events
                                    .send(UiEvent::SlashOutput(format!("✗ sign-in failed: {e}")));
                            }
                        }
                    }
                    Err(e) => {
                        let _ = events.send(UiEvent::SlashError(format!("✗ sign-in failed: {e}")));
                    }
                }
            });
            return;
        }

        // Default: loopback PKCE (local callback + opening the browser).
        let shared_tp = session.client.token_provider(&name);
        tokio::spawn(async move {
            let flow = crate::api::auth::LoopbackPkce::new(&http, &config);
            match flow.authorize_url().await {
                Ok((url, _redirect, _verifier, handle)) => {
                    // Pinned, with the URL itself: on SSH/no-GUI hosts the
                    // browser never opens and this line is the only way
                    // through (it used to say "tried to open" and show nothing).
                    let _ = events.send(UiEvent::PinPanel {
                        id: "login".to_string(),
                        lines: vec![
                            format!("sign in to {name}: complete the authorization in the browser (tried to open it automatically)"),
                            format!("  {url}"),
                            format!("  browser did not open? /provider login {name} --device-auth"),
                        ],
                    });
                    let _ = crate::share::open_in_browser(&url);
                    let outcome = handle.await;
                    let _ = events.send(UiEvent::Unpin {
                        id: "login".to_string(),
                    });
                    match outcome {
                        Ok(Ok(tokens)) => {
                            let tp = shared_tp.unwrap_or_else(|| {
                                std::sync::Arc::new(crate::api::auth::TokenProvider::new(
                                    &home, &name, config,
                                ))
                            });
                            match tp.save(&tokens).await {
                                Ok(()) => {
                                    let _ = events.send(UiEvent::SlashOutput(format!(
                                        "✓ signed in to {name}"
                                    )));
                                }
                                Err(e) => {
                                    let _ = events
                                        .send(UiEvent::SlashOutput(format!("✗ save failed: {e}")));
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            let _ =
                                events.send(UiEvent::SlashError(format!("✗ sign-in failed: {e}")));
                        }
                        Err(e) => {
                            let _ = events
                                .send(UiEvent::SlashError(format!("✗ sign-in interrupted: {e}")));
                        }
                    }
                }
                Err(e) => {
                    let _ = events.send(UiEvent::SlashError(format!("✗ sign-in failed: {e}")));
                }
            }
        });
    }

    pub(crate) fn slash_provider_logout(&mut self, arg: &str) {
        let name = arg.trim();
        if name.is_empty() {
            self.push_slash_error("usage: /provider logout <name>".to_string());
            return;
        }
        let session = self.session.clone();
        let preset = crate::api::providers::presets::preset(name);
        let known = session.settings.providers.contains_key(name) || preset.is_some();
        if !known {
            self.push_slash_error(format!(
                "provider \"{name}\" not found (see /provider for the list)"
            ));
            return;
        }
        let oauth_kind = session
            .settings
            .providers
            .get(name)
            .and_then(|c| c.oauth.as_ref().map(|o| o.kind.clone()))
            .or_else(|| preset.and_then(|p| p.oauth_kind.map(str::to_string)));
        let name = name.to_string();
        let home = session.home.clone();
        let events = self.events.clone();
        let shared_tp = session.client.token_provider(&name);
        tokio::spawn(async move {
            if oauth_kind.as_deref() != Some("codex") {
                // apiKey preset (opencode-go): only clears the auth.json entry.
                match crate::auth::AuthStore::new(&home).remove(&name) {
                    Ok(()) => {
                        let _ = events.send(UiEvent::SlashOutput(format!(
                            "✓ signed out of {name} (key cleared)"
                        )));
                    }
                    Err(e) => {
                        let _ = events.send(UiEvent::SlashError(format!("✗ sign-out failed: {e}")));
                    }
                }
                return;
            }
            // Same shared instance as login: the session's adapter loses its
            // cached token immediately (a stale cache kept requests working
            // after logout).
            let tp = shared_tp.unwrap_or_else(|| {
                std::sync::Arc::new(crate::api::auth::TokenProvider::new(
                    &home,
                    &name,
                    crate::api::auth::OauthFlowConfig::codex(),
                ))
            });
            match tp.logout().await {
                Ok(()) => {
                    let _ = events.send(UiEvent::SlashOutput(format!(
                        "✓ signed out of {name} (credentials cleared)"
                    )));
                }
                Err(e) => {
                    let _ = events.send(UiEvent::SlashError(format!("✗ sign-out failed: {e}")));
                }
            }
        });
    }

    pub(crate) fn slash_think(&mut self, arg: &str) {
        if arg.is_empty() {
            self.open_think_menu();
            return;
        }
        self.set_think_level(arg, true);
    }

    /// Sets the thinking level. Level table = off + THINKING_LEVELS: off sends no
    /// parameter; the rest send adaptive thinking + output_config.effort.
    /// `persist = false` applies to the current session only (no settings write).
    fn set_think_level(&mut self, arg: &str, persist: bool) {
        let level = if arg == "off" {
            None
        } else if crate::api::contract::THINKING_LEVELS.contains(&arg) {
            Some(arg.to_string())
        } else {
            self.push_slash_error(format!(
                "[error] code={} msg=usage: /think [off|low|medium|high|xhigh|max]",
                crate::error::SLASH_ERROR_BAD_ARGUMENT
            ));
            return;
        };
        let _ = self.session.runtime.thinking_tx.send(level.clone());
        let saved = level.as_deref().unwrap_or("off");
        // The wire gate (query.rs) skips thinking for models that reject it —
        // say so here, or the footer shows a level that never takes effect.
        let model = self.session.runtime.model.borrow().clone();
        let ignored = if level.is_some() && !self.session.client.models().supports_thinking(&model)
        {
            format!(" (⚠ {model} does not support thinking; the level will be ignored)")
        } else {
            String::new()
        };
        if persist {
            let cwd = std::path::PathBuf::from(&self.cwd);
            let _ = crate::settings::upsert_scoped_settings(
                &self.session.user_config_dir,
                &cwd,
                &serde_json::json!({ "thinkingLevel": saved }),
            );
            self.push_slash_output(format!("✓ thinking level set: {saved}{ignored}"));
        } else {
            self.push_slash_output(format!(
                "✓ thinking level set: {saved} (this session only){ignored}"
            ));
        }
    }

    /// Enters the `/think` level selector: preselects the current level (off when unset).
    fn open_think_menu(&mut self) {
        let current = self.session.runtime.thinking.borrow().clone();
        let current = current.as_deref().unwrap_or("off");
        let current = THINK_LEVELS
            .iter()
            .position(|(name, _)| *name == current)
            .unwrap_or(0);
        let menu = ThinkMenu {
            selected: current,
            current,
        };
        // Empty-table guard (THINK_LEVELS is a non-empty const; this defensive branch is unreachable): the menu stays closed.
        if menu.picker().is_empty() {
            return;
        }
        self.close_menus();
        self.think_menu = Some(menu);
        self.clear_slash_suggestions();
    }

    /// Think level menu keys: ↑↓/1-6 move (wraps, delegated to the PickerModel core),
    /// Enter confirms + persists, s = session-only (no settings write), Esc exits.
    /// Returns whether consumed.
    fn think_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(menu) = &mut self.think_menu else {
            return false;
        };
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(1);
                menu.selected = core.selected;
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                let mut core = menu.picker();
                core.move_selection(-1);
                menu.selected = core.selected;
                true
            }
            // Direct jump: 1 = off … 6 = max (fixed 6-item table, §G10).
            KeyCode::Char(c)
                if c.is_ascii_digit() && !modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let mut core = menu.picker();
                if let Some(n) = c.to_digit(10)
                    && core.jump(n as usize)
                {
                    menu.selected = core.selected;
                }
                // Swallow even out-of-range digits: a menu is a modal surface —
                // "4" on a 3-item picker used to type a literal 4 into the input.
                true
            }
            KeyCode::Char('s') if !modifiers.contains(KeyModifiers::CONTROL) => {
                let core = menu.picker();
                let value = core
                    .selected_item()
                    .map(|i| i.value.clone())
                    .unwrap_or_default();
                self.think_menu = None;
                self.set_think_level(&value, false);
                true
            }
            KeyCode::Enter => {
                let core = menu.picker();
                let value = core
                    .selected_item()
                    .map(|i| i.value.clone())
                    .unwrap_or_default();
                self.think_menu = None;
                self.set_think_level(&value, true);
                true
            }
            KeyCode::Esc => {
                self.think_menu = None;
                true
            }
            _ => false,
        }
    }

    pub(crate) fn slash_skills(&mut self) {
        let home = self.session.home.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        let skills = crate::skills::load_skills(&home, &cwd);
        if skills.is_empty() {
            self.push_slash_output(
                "no skills available.\nSkills live in .bingo/skills/<name>/SKILL.md or $XDG_CONFIG_HOME/bingo/skills/<name>/SKILL.md."
                    .to_string(),
            );
            return;
        }
        let listing = crate::skills::format_listing(&skills, crate::skills::DEFAULT_CHAR_BUDGET);
        self.push_slash_info(format!("available skills:\n{listing}"));
    }

    pub(crate) fn slash_tasks(&mut self) {
        self.refresh_tasks();
        // task_lines is gated by task-area visibility — /tasks explicitly asks for them, so bypass it temporarily.
        let was_visible = self.tasks_visible;
        self.tasks_visible = true;
        let lines = self.task_lines();
        self.tasks_visible = was_visible;
        if lines.is_empty() {
            self.push_slash_output("no background tasks right now.".to_string());
            return;
        }
        let text: Vec<String> = lines.iter().map(|l| l.plain_text()).collect();
        self.push_slash_info(text.join("\n"));
    }

    /// `/team <subcommand>` (D31 project-level formation): dispatched to team_cmd, multi-line output queued at once.
    pub(crate) fn slash_team(&mut self, arg: &str) {
        let lines = crate::team_cmd::run(&self.session, &std::path::PathBuf::from(&self.cwd), arg);
        self.push_slash_info(lines.join("\n"));
    }

    /// Rebuilds the composer's dropdown after an edit. One surface at a time,
    /// in the order the caret decides: an `@` token under the caret (D85), else
    /// the argument phase of a slash line (D85), else the command-name phase.
    pub(crate) fn update_slash_suggestions(&mut self) {
        self.clear_slash_suggestions();
        self.update_mention();
        if self.mention.is_some() {
            return;
        }
        if let Some(ctx) = crate::tui::complete::arg_context(&self.input) {
            // Past the command name, whatever happens next: a free-form
            // argument offers nothing rather than falling back to re-offering
            // the command the user has already finished typing.
            if let Some(candidates) = self.arg_candidates(&ctx) {
                let start = ctx.start;
                let items =
                    crate::tui::complete::fuzzy_rank(ctx.partial, candidates, |candidate| {
                        candidate.value.as_str()
                    });
                self.slash_arg_start = Some(start);
                self.slash_suggestions = items
                    .into_iter()
                    .map(|candidate| SlashSuggestion {
                        name: candidate.value,
                        hint: String::new(),
                        description: candidate.description,
                    })
                    .collect();
                self.slash_selected = self
                    .slash_selected
                    .min(self.slash_suggestions.len().saturating_sub(1));
            }
            // The name-phase "no matching commands" hint would be a lie here:
            // an unmatched argument is still a legal thing to type.
            self.slash_no_match = false;
            return;
        }
        let home = self.session.home.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        let skills = crate::skills::load_skills(&home, &cwd)
            .into_iter()
            .map(|skill| {
                let mut description = skill.description;
                if description.chars().count() > crate::skills::MAX_LISTING_DESC_CHARS {
                    let cut: String = description
                        .chars()
                        .take(crate::skills::MAX_LISTING_DESC_CHARS - 1)
                        .collect();
                    description = format!("{cut}…");
                }
                SlashSuggestion {
                    name: skill.name,
                    hint: String::new(),
                    description,
                }
            });
        let result = crate::tui::slash::suggestions(
            &self.input,
            SLASH_COMMANDS,
            skills,
            // Full list: rendering windows around the selection (the old
            // hard cap made commands 6+ unreachable from a bare `/`).
            usize::MAX,
        );
        self.slash_suggestions = result.items;
        self.slash_selected = self
            .slash_selected
            .min(self.slash_suggestions.len().saturating_sub(1));
        self.slash_no_match = result.no_match;
    }

    /// Dropdown key handling: ↑↓ move the selection, Tab completes (without running), Esc closes.
    /// No j/k navigation: while the menu is open, j/k would be typed as input chars (e.g. /thin → think),
    /// swallowing keys and truncating the command. Returns true = consumed.
    pub(crate) fn slash_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        if self.slash_suggestions.is_empty() {
            return false;
        }
        match code {
            KeyCode::Down if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.slash_selected = (self.slash_selected + 1) % self.slash_suggestions.len();
                true
            }
            KeyCode::Up if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.slash_selected = self
                    .slash_selected
                    .checked_sub(1)
                    .unwrap_or(self.slash_suggestions.len() - 1);
                true
            }
            KeyCode::Tab => {
                self.apply_slash_suggestion();
                true
            }
            KeyCode::Esc => {
                // The argument phase keeps its line: `/model dee` is a command
                // the user is halfway through typing, not a stray query.
                let name_phase = self.slash_arg_start.is_none();
                self.clear_slash_suggestions();
                // The name dropdown only exists for a pure `/`-query — dismissing
                // it dismisses the query too (the leftover "/th" used to turn the
                // next command into "//model").
                if name_phase && self.input.starts_with('/') {
                    self.set_input("");
                }
                true
            }
            _ => false,
        }
    }

    /// Applies the selected suggestion (applyCommandSuggestion). In the name
    /// phase that is `/name ` for the whole line; in the argument phase (D85)
    /// the value is spliced over the partial token, leaving the command and any
    /// earlier arguments alone. Both end with a space, so the next argument's
    /// dropdown opens straight away.
    fn apply_slash_suggestion(&mut self) {
        if let Some(s) = self.slash_suggestions.get(self.slash_selected) {
            match self.slash_arg_start {
                Some(start) if start <= self.input.len() && self.input.is_char_boundary(start) => {
                    self.input = format!("{}{} ", &self.input[..start], s.name);
                }
                _ => self.input = format!("/{} ", s.name),
            }
            // The caret follows the text it just completed; it used to stay
            // where the partial word ended, leaving it mid-line.
            self.cursor = self.input.len();
        }
        self.clear_slash_suggestions();
    }

    /// Submits the next queued item after a turn (one at a time: a plain message starts
    /// the next turn; queued slash commands drain synchronously until one does).
    pub(crate) fn submit_queued(&mut self) {
        if self.busy || self.queued.is_empty() {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        // Drain queued slash commands synchronously; stop at the first plain message
        // (it starts a turn, which re-triggers submit_queued on TurnEnd).
        loop {
            let Some(first) = self.queued.first() else {
                return;
            };
            if !first.is_slash {
                break;
            }
            let item = self.queued.remove(0);
            self.run_slash(item.text.strip_prefix('/').unwrap_or(&item.text));
            if self.busy {
                return; // a skill command started a turn; the rest waits for TurnEnd
            }
        }
        let item = self.queued.remove(0);
        self.start_turn(item.text, true);
    }

    /// Re-arms the steer channel from the queue: the longest prefix of plain messages
    /// the running turn may absorb (D83).
    ///
    /// It is a *prefix*, not a filter. A slash command runs on this side, so it cannot
    /// travel to the turn — and letting a plain message queued behind one jump into the
    /// turn would run the two in the opposite order from the one they were typed in.
    /// The same goes for a message carrying images: mounting attachments is `start_turn`'s
    /// path, so it waits for TurnEnd and everything after it waits with it.
    ///
    /// With no turn running there is nothing to steer, and the channel is emptied rather
    /// than left holding an offer for whichever turn starts next.
    pub(crate) fn rearm_steer(&mut self) {
        if !self.busy {
            self.steer.reset();
            return;
        }
        let mut items = Vec::new();
        for entry in &self.queued {
            if entry.is_slash || !self.resolve_images(&entry.text).is_empty() {
                break;
            }
            items.push(crate::steer::SteerItem {
                id: entry.id,
                text: entry.text.clone(),
            });
        }
        self.steer.rearm(items);
    }

    /// The running turn took these queued messages into its own context at a tool
    /// barrier: they are in the request already, so they leave the queue and enter the
    /// flow where the model read them.
    ///
    /// The reply block is split there. One turn renders as one assistant message, so a
    /// line merely pushed after it would sink below everything the turn still had to
    /// say; closing the block and opening a continuation — the same move an
    /// AskUserQuestion answer makes — puts the message between the reply written
    /// without it and the reply written with it, which is the order the history holds.
    pub(crate) fn absorb_steered(&mut self, items: &[crate::steer::SteerItem]) {
        if items.is_empty() {
            return;
        }
        self.queued
            .retain(|entry| !items.iter().any(|item| item.id == entry.id));
        for item in items {
            self.push_steered_line(&item.text);
        }
        self.open_continuation_message();
        self.rearm_steer();
        self.dirty = true;
    }

    /// The transcript line a steered message leaves: the user's own words under the
    /// `↪` marker, rendered as a single dim line rather than a `❯` bubble.
    fn push_steered_line(&mut self, text: &str) {
        self.messages.push(UiMessage {
            role: Role::User,
            text: format!("{}{text}", crate::steer::STEER_FLOW_PREFIX),
            at: crate::channels::now_unix(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
    }

    /// System-triggered turn: a watchable signal/terminal notification wakes the main agent.
    /// No user input (the notification is injected in run_query's first round); user state is irrelevant.
    pub(crate) fn submit_auto(&mut self) {
        if self.busy {
            return;
        }
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        self.start_turn(String::new(), false);
    }

    /// Multi-turn continuity: loads transcript history as this turn's context (each turn runs its own run_query).
    fn load_history(
        session: &Session,
        on_warning: &mut (dyn FnMut(String) + Send),
    ) -> Vec<crate::api::types::Message> {
        let Some(t) = session.runtime.transcript.borrow().clone() else {
            return Vec::new();
        };
        match t.load_messages() {
            Ok(msgs) => msgs,
            Err(crate::transcript::TranscriptError::Io(e))
                if e.kind() == std::io::ErrorKind::NotFound =>
            {
                Vec::new()
            }
            Err(e) => {
                on_warning(format!("transcript load failed: {e}"));
                Vec::new()
            }
        }
    }

    /// Post-turn handling: send TurnEnd first (busy resets / the completion row appears immediately),
    /// memory extraction is deferred — it is a non-streaming model call (seconds) and the wrap-up should not block
    /// the turn-end UI; extraction runs fine in parallel with the next turn (e.g. a watch wake-up).
    async fn finish_turn(
        events: &mpsc::UnboundedSender<UiEvent>,
        session: &Arc<Session>,
        outcome: &crate::query::QueryOutcome,
    ) {
        if let Some(marker) = outcome.interrupt_marker {
            let _ = events.send(UiEvent::Interrupted { marker });
        }
        if !outcome.aborted {
            match outcome.end_reason {
                crate::query::QueryEndReason::EmptyResponseRetried => {
                    let _ = events.send(UiEvent::Warning(
                        "model returned an empty response and was retried".to_string(),
                    ));
                }
                crate::query::QueryEndReason::Completed => {}
            }
        }
        let _ = events.send(UiEvent::TurnEnd);
        let cwd = session.cwd();
        crate::memory::extract_memory(session, &outcome.messages, &session.home, &cwd).await;
    }

    /// A turn task that dies without reporting an outcome (a panic inside the spawn) leaves
    /// `busy` latched, and every interrupt and quit route is gated on `busy` — the session
    /// then answers only to `kill`. Watching the handle turns a lost turn back into the
    /// ordinary long-turn error state, which releases `busy` and offers retry / go back.
    pub(crate) fn supervise_turn(
        events: mpsc::UnboundedSender<UiEvent>,
        handle: tokio::task::JoinHandle<()>,
    ) {
        tokio::spawn(async move {
            if handle.await.is_err() {
                let _ = events.send(UiEvent::Error {
                    code: crate::error::TURN_LOST,
                    msg: "The turn ended unexpectedly; retry or go back.".to_string(),
                    level: crate::error::ErrorLevel::Full,
                    context: crate::error::ErrorContext::LongTurn,
                });
            }
        });
    }

    pub(crate) fn start_turn(&mut self, text: String, show_user: bool) {
        if show_user {
            self.messages.push(UiMessage {
                role: Role::User,
                text: text.clone(),
                at: crate::channels::now_unix(),
                activities: Vec::new(),
                insert_points: Vec::new(),
                groups: Vec::new(),
                group_of: Vec::new(),
            });
        }
        self.busy = true;
        self.interrupted = false;
        // The steer channel belongs to one turn (D83): whatever the previous turn chose
        // not to take must not be folded into this one behind the user's back. The
        // caller re-arms it against the queue once this turn is the running one.
        self.steer.reset();
        let steer = self.steer.clone();
        let live = self.live.clone();
        let session = self.session_for_turn();
        let events = self.events.clone();
        let asks = self.asks.clone();
        let images = self.resolve_images(&text);
        // Subscribe first, then reset: tokio watch's send does not update the value with no receivers —
        // after the previous spawn ends, all receivers are dropped; sending false first would silently
        // fail (the value stays true) and the new turn would be misread as interrupted during connection.
        let cancel_rx = self.cancel_tx.subscribe();
        self.cancel_tx.send_replace(false);
        let handle = tokio::spawn(async move {
            let _ = events.send(UiEvent::TurnStart);
            let mut ui = crate::ui::tui_hooks(events.clone(), asks, steer, live);
            let history = Self::load_history(&session, &mut ui.on_warning);
            let result =
                run_query(&session, history, &text, &images, &mut ui, Some(cancel_rx)).await;
            match result {
                Ok(outcome) => {
                    Self::finish_turn(&events, &session, &outcome).await;
                }
                Err(e) => {
                    let code = crate::error::map_error(&e);
                    let _ = events.send(UiEvent::Error {
                        code,
                        msg: Self::auth_error_hint(&session, code, e.to_string()),
                        // Turn-level error = long-turn failure → full-flow full-screen state (AC-53).
                        level: crate::error::ErrorLevel::Full,
                        context: crate::error::ErrorContext::LongTurn,
                    });
                }
            }
        });
        Self::supervise_turn(self.events.clone(), handle);
    }

    /// Turn-level error message with auth guidance for the current provider:
    /// `AUTH_REQUIRED` on an oauth-configured provider appends a re-login
    /// hint (the raw API error body rarely tells the user what to do);
    /// `PERMISSION_DENIED` points at the model/subscription (D33 §6.4).
    fn auth_error_hint(session: &Session, code: &str, msg: String) -> String {
        let provider = session.runtime.provider.borrow().clone();
        // Merge built-in presets: zero-config codex (no settings entry) is the
        // main preset use case — without this, its expired token produced a
        // bare 401 with no re-login guidance.
        let oauth = session
            .settings
            .providers
            .get(&provider)
            .map(|c| c.oauth.is_some())
            .or_else(|| {
                crate::api::providers::presets::preset(&provider).map(|p| p.oauth_kind.is_some())
            })
            .unwrap_or(false);
        auth_hint_for(oauth, &provider, code, msg)
    }

    /// bash-mode turn (processBashCommand): `!` commands execute directly,
    /// output shown as a tool activity; with respondToBashCommands on, the model replies afterwards.
    pub(crate) fn start_bash_turn(&mut self, command: String) {
        self.messages.push(UiMessage {
            role: Role::User,
            text: format!("!{command}"),
            at: crate::channels::now_unix(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        self.busy = true;
        // Same as start_turn: a fresh turn clears interrupt suppression —
        // without this, one interrupt followed by only `!` commands kept
        // background wake-ups suppressed for the rest of the session.
        self.interrupted = false;
        // Same as start_turn: the channel is this turn's (D83).
        self.steer.reset();
        let steer = self.steer.clone();
        let live = self.live.clone();
        let session = self.session_for_turn();
        let events = self.events.clone();
        let asks = self.asks.clone();
        // Same as start_turn: subscribe first, then reset (send does not update with no receivers).
        let cancel_rx = self.cancel_tx.subscribe();
        self.cancel_tx.send_replace(false);
        let handle = tokio::spawn(async move {
            let _ = events.send(UiEvent::TurnStart);
            let mut ui = crate::ui::tui_hooks(events.clone(), asks, steer, live);
            let history = Self::load_history(&session, &mut ui.on_warning);
            let result = crate::query::run_bash_command(
                &session,
                &command,
                history,
                &mut ui,
                Some(cancel_rx),
            )
            .await;
            match result {
                Ok(outcome) => {
                    Self::finish_turn(&events, &session, &outcome).await;
                }
                Err(e) => {
                    let code = crate::error::map_error(&e);
                    let _ = events.send(UiEvent::Error {
                        code,
                        msg: Self::auth_error_hint(&session, code, e.to_string()),
                        // Turn-level error = long-turn failure → full-flow full-screen state (AC-53).
                        level: crate::error::ErrorLevel::Full,
                        context: crate::error::ErrorContext::LongTurn,
                    });
                }
            }
        });
        Self::supervise_turn(self.events.clone(), handle);
    }

    /// The answer lands mid-turn and the model keeps going. Without a message of its own, that
    /// continuation streams into the assistant message *above* the answer (`stream_msg` still
    /// points there), so everything the model does next renders above what the user just said
    /// and the answer stays pinned to the bottom until the turn ends. Close the old message and
    /// open a fresh one, the way a turn boundary would: the transcript then reads in clock order.
    pub(crate) fn open_continuation_message(&mut self) {
        let Some(prev) = self.stream_msg else { return };
        // Tool rows registered before the answer index into `prev`'s activities
        // (`pending_tools` holds those indices), so a call still in flight pins the stream here.
        if !self.pending_tools.is_empty() {
            return;
        }
        // AskUserQuestion is a hidden tool: `ToolStart` returns before closing the running
        // thinking block, and a block left running would keep `prev` from ever settling
        // (`message_static_settled`) — with it the whole flush prefix, for the rest of the session.
        self.close_running_thinking(prev);
        // The buffer belongs to the block just closed; carried over, the next reasoning delta
        // would try to merge into a block the new message does not have, and be dropped.
        self.thinking_buf.clear();
        self.thinking_seg_open = false;
        self.messages.push(UiMessage {
            role: Role::Assistant,
            text: String::new(),
            at: crate::channels::now_unix(),
            activities: Vec::new(),
            insert_points: Vec::new(),
            groups: Vec::new(),
            group_of: Vec::new(),
        });
        self.stream_msg = Some(self.messages.len() - 1);
        self.stream_attempt_checkpoint = self
            .stream_msg
            .and_then(|index| self.messages.get(index).cloned());
        self.continuation_msg = self.stream_msg;
    }

    /// A continuation message the turn never filled (the answer was the last thing that happened):
    /// an empty assistant block renders as a stray gap. Only ever drops the message
    /// [`Chat::open_continuation_message`] opened. Call before clearing `stream_msg`.
    pub(crate) fn drop_empty_stream_message(&mut self) {
        let Some(i) = self.continuation_msg.take() else {
            return;
        };
        if self.stream_msg == Some(i)
            && i + 1 == self.messages.len()
            && self.messages[i].text.is_empty()
            && self.messages[i].activities.is_empty()
        {
            self.messages.pop();
            self.stream_msg = None;
            self.stream_attempt_checkpoint = None;
        }
    }

    /// A tool call, message text, or a mid-turn answer all end the current reasoning segment.
    pub(crate) fn close_running_thinking(&mut self, i: usize) {
        let tick = self.tick;
        for hint in &mut self.messages[i].activities {
            if let ActivityKind::Thinking(t) = &mut hint.kind
                && t.state == ThinkingState::Running
            {
                t.state = ThinkingState::Done;
                t.duration_ms = tick.saturating_sub(t.start_tick).saturating_mul(33);
            }
        }
    }

    /// Keyboard events. Real-clock version; semantics in [`Chat::on_key_at`].
    pub fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        self.on_key_at(code, modifiers, std::time::Instant::now())
    }

    /// Resets the error state (one of AC-03's four resets: clears the error row / full-screen error state).
    fn dismiss_error(&mut self) {
        self.last_error = None;
    }

    /// #18 full-flow full-screen error-state keys (AC-26/53: the way back is not a dead end):
    /// Enter = retry (reruns the last input), Esc = back, Ctrl+C = quit, the rest ignored.
    fn error_screen_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        now: std::time::Instant,
    ) -> bool {
        match code {
            KeyCode::Enter => {
                self.dismiss_error();
                if !self.last_prompt.is_empty() {
                    self.start_turn(self.last_prompt.clone(), true);
                }
                true
            }
            KeyCode::Esc => {
                self.dismiss_error();
                true
            }
            KeyCode::Char('c') if modifiers.contains(KeyModifiers::CONTROL) => self.ctrl_c(now),
            _ => true,
        }
    }

    /// Keyboard events (`now` is injectable: the Ctrl+C double-press window and paste-burst detection both need a clock).
    ///
    /// Priority, top to bottom: dialog → `/model` menu → history search → interrupt/quit semantics
    /// → editing keys. Esc is the exception: it belongs to no single overlay and walks
    /// [`EscLayer::ORDER`] instead, so its priority can be read in one place rather than
    /// inferred from the order of the handlers below. Returns whether it was consumed.
    pub fn on_key_at(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        now: std::time::Instant,
    ) -> bool {
        let pasting = self.track_burst(now);
        self.pasting = pasting;
        // The two "immediately after" rules are counted in keys, not seconds
        // (D86): the tick is what makes a kill coalesce with the kill before
        // it and `alt+y` a yank-pop rather than a no-op.
        self.composer.tick();
        // `ctrl+x` arms its chord for exactly one key. Taking it here rather
        // than in `control_key` is what makes "anything else" mean anything
        // else — a plain character, Esc, a dialog key — and not just the
        // control keys that reach the same handler.
        let chord = self.composer.take_chord(now);
        // Update-banner breathing (P1): the first keypress in the window stops it immediately (the user's attention has moved to the input;
        // the banner itself stays, it just stops breathing).
        if self.update_anim_active() {
            self.update_banner_stopped = true;
        }
        // #18 full-flow full-screen error state: primary actions Enter=retry / Esc=back, the rest ignored.
        if let Some(err) = &self.last_error
            && err.level == crate::error::ErrorLevel::Full
        {
            return self.error_screen_key(code, modifiers, now);
        }
        // Esc dismisses the topmost layer and nothing else (D80): it is judged
        // before the handlers below because every one of them used to claim it,
        // and the resulting order — busy interrupt first — meant Esc killed the
        // turn instead of closing the dropdown the user was looking at.
        if code == KeyCode::Esc {
            return self.escape(now);
        }
        if self.ask_key_at(code, modifiers, now) {
            return true;
        }
        // A printable key that no menu claims (menus only take ↑↓/Enter/Esc/
        // digits/s) closes the menu first, then edits normally. Without this,
        // typing "/theme" over an open /think menu kept feeding a menu the
        // screen no longer showed — Enter landed on an invisible selection.
        if self.menu_open()
            && !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && matches!(code, KeyCode::Char(c) if !c.is_ascii_digit() && c != 's')
        {
            self.close_menus();
        }
        // `/model` `/think` selectors take priority over input (↑↓/Enter/Esc fully consumed).
        if self.model_menu_key(code, modifiers) {
            return true;
        }
        if self.think_menu_key(code, modifiers) {
            return true;
        }
        if self.theme_menu_key(code, modifiers) {
            return true;
        }
        if self.resume_menu_key(code, modifiers) {
            return true;
        }
        if self.provider_menu_key(code, modifiers) {
            return true;
        }
        if self.search.is_some() {
            return self.search_key(code, modifiers);
        }
        // Main-view agent management takes precedence over global editing keys.
        if self.agent_manager_key(code, modifiers) {
            return true;
        }
        // Interrupt (busy) and quit (idle) both live on Ctrl+C, judged before editing keys.
        // Unlike Esc, Ctrl+C skips the layer stack: it interrupts with anything open.
        if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
            return self.ctrl_c(now);
        }
        self.notice = None;
        // Composer completion keys take priority over input. The `@` dropdown
        // is judged first: it is anchored at the caret, and the two are
        // mutually exclusive anyway (D85).
        if !self.bash_mode && self.mention_menu_key(code, modifiers) {
            return true;
        }
        // Slash dropdown keys (Tab completes / Esc closes / ↑↓ navigate) take priority over input.
        if !self.bash_mode && self.slash_menu_key(code, modifiers) {
            return true;
        }
        if modifiers.contains(KeyModifiers::CONTROL)
            && let KeyCode::Char(c) = code
        {
            return self.control_key(c, chord, now);
        }
        if modifiers.contains(KeyModifiers::ALT)
            && let KeyCode::Char(c) = code
        {
            return self.alt_key(c);
        }
        match code {
            // Shift+Tab: cycle the permission mode (CC app:cyclePermissionMode).
            KeyCode::BackTab => {
                self.cycle_permission_mode();
                true
            }
            KeyCode::Left => {
                self.cursor = crate::tui::input::prev_char(&self.input, self.cursor);
                true
            }
            KeyCode::Right => {
                self.cursor = crate::tui::input::next_char(&self.input, self.cursor);
                true
            }
            KeyCode::Home => {
                self.cursor = crate::tui::input::line_start(&self.input, self.cursor);
                true
            }
            KeyCode::End => {
                self.cursor = crate::tui::input::line_end(&self.input, self.cursor);
                true
            }
            KeyCode::Up => self.vertical(false),
            KeyCode::Down => self.vertical(true),
            // Alt+Backspace is readline's `backward-kill-word`: the sub-word
            // kill, so it stops inside a path where ctrl+w takes the whole
            // token (D86).
            KeyCode::Backspace if modifiers.contains(KeyModifiers::ALT) => {
                self.snapshot(EditKind::Bulk);
                let end = self.cursor;
                let start = crate::tui::input::subword_left(&self.input, end);
                let cut =
                    crate::tui::input::kill_between(&mut self.input, &mut self.cursor, start, end);
                self.composer.kill(cut, KillDir::Back);
                self.after_edit();
                true
            }
            KeyCode::Backspace => {
                // Empty-input backspace in bash mode exits shell mode (CC).
                if self.bash_mode && self.input.is_empty() {
                    self.bash_mode = false;
                    return true;
                }
                self.snapshot(EditKind::Delete);
                crate::tui::input::backspace(&mut self.input, &mut self.cursor);
                self.after_edit();
                true
            }
            KeyCode::Delete => {
                self.snapshot(EditKind::Delete);
                crate::tui::input::delete(&mut self.input, &mut self.cursor);
                self.after_edit();
                true
            }
            KeyCode::Tab if self.bash_mode => {
                self.complete_bash_history();
                true
            }
            // Shift+Enter (available when the terminal reports enhanced keyboards) and pasted Enter are both newlines.
            KeyCode::Enter
                if pasting || modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.insert_newline();
                // Only pasted newlines can pile up large text → fold into a placeholder at the threshold.
                if pasting {
                    self.collapse_paste();
                }
                true
            }
            KeyCode::Enter => {
                // `\` + Enter: the newline every terminal can type (CC).
                if self.input.ends_with('\\') && self.cursor == self.input.len() {
                    self.snapshot(EditKind::Bulk);
                    self.input.pop();
                    self.cursor = self.input.len();
                    self.insert_newline();
                    return true;
                }
                self.submit();
                true
            }
            // `?` on empty input toggles the shortcut panel; with text it is an ordinary character.
            // Inside a paste it is always the character: a payload that opens
            // with `?` or `!` is text, not a command to this composer (D86).
            KeyCode::Char('?') if self.input.is_empty() && !self.bash_mode && !pasting => {
                self.help_visible = !self.help_visible;
                true
            }
            // `!` on empty input enters shell mode (`!` itself never enters the input).
            KeyCode::Char('!') if self.input.is_empty() && !self.bash_mode && !pasting => {
                self.bash_mode = true;
                true
            }
            KeyCode::Char(c) if !c.is_control() => {
                self.snapshot(EditKind::Insert);
                let mut buf = [0u8; 4];
                crate::tui::input::insert(
                    &mut self.input,
                    &mut self.cursor,
                    c.encode_utf8(&mut buf),
                );
                self.after_edit();
                true
            }
            KeyCode::PageDown => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_add(10);
                self.reconcile_scroll(self.viewport_height);
                true
            }
            KeyCode::PageUp => {
                self.auto_scroll = false;
                self.scroll = self.scroll.saturating_sub(10);
                self.reconcile_scroll(self.viewport_height);
                true
            }
            _ => false,
        }
    }

    /// Paste-burst detection: [`PASTE_BURST_KEYS`] consecutive key presses with intervals under
    /// [`PASTE_BURST_GAP`] count as a paste (limitations in that constant's comment).
    fn track_burst(&mut self, now: std::time::Instant) -> bool {
        let fast = self
            .last_key_at
            .is_some_and(|last| now.duration_since(last) < PASTE_BURST_GAP);
        self.burst_keys = if fast { self.burst_keys + 1 } else { 0 };
        self.last_key_at = Some(now);
        self.burst_keys >= PASTE_BURST_KEYS
    }

    /// Ctrl+C: interrupts when busy; with text while idle, clears it (into history, retrievable with ↑);
    /// first press on idle empty input shows a hint, a second press within [`CTRL_C_WINDOW`] quits.
    fn ctrl_c(&mut self, now: std::time::Instant) -> bool {
        if self.busy {
            // The quit path below is gated on `busy`, so a turn that never clears it (its
            // task died) used to leave `kill` as the only way out. An interrupt the turn
            // has ignored for [`INTERRUPT_GRACE`] hands Ctrl+C back its exit meaning.
            if self
                .interrupt_at
                .is_some_and(|at| now.duration_since(at) >= INTERRUPT_GRACE)
            {
                self.exit = true;
                return true;
            }
            self.interrupt(now);
            self.notice = Some("Interrupting… press ctrl-c again to force quit");
            self.notice_until = Some(now + CTRL_C_WINDOW);
            return true;
        }
        if !self.input.is_empty() {
            self.clear_input_into_history();
            self.notice = None;
            self.ctrl_c_at = None;
            return true;
        }
        let armed = self
            .ctrl_c_at
            .is_some_and(|at| now.duration_since(at) <= CTRL_C_WINDOW);
        if armed {
            self.exit = true;
            return true;
        }
        self.ctrl_c_at = Some(now);
        self.notice = Some("Press ctrl-c again to exit");
        self.notice_until = Some(now + CTRL_C_WINDOW);
        true
    }

    /// Esc: dismisses the topmost open [`EscLayer`] and stops there — one press,
    /// one level. With nothing open on an idle empty input it does nothing
    /// (reserved for rewind, D91).
    fn escape(&mut self, now: std::time::Instant) -> bool {
        match self.esc_layer() {
            Some(layer) => self.esc_dismiss(layer, now),
            None => {
                self.notice = None;
                false
            }
        }
    }

    /// The layer Esc acts on right now: the first entry of [`EscLayer::ORDER`]
    /// that is open. Key dispatch and the hint text both read this — they used
    /// to disagree, because the order lived in the shape of an if-chain.
    pub(crate) fn esc_layer(&self) -> Option<EscLayer> {
        EscLayer::ORDER
            .into_iter()
            .find(|layer| self.esc_layer_open(*layer))
    }

    fn esc_layer_open(&self, layer: EscLayer) -> bool {
        match layer {
            EscLayer::AskDialog => self.pending_ask.is_some(),
            EscLayer::Menu => self.menu_open(),
            EscLayer::AgentManager => self.agent_manager.is_some(),
            EscLayer::SlashDropdown => !self.slash_suggestions.is_empty(),
            EscLayer::MentionDropdown => self.mention.is_some(),
            EscLayer::Search => self.search.is_some(),
            // The full-screen error state is not a layer: it owns the whole
            // canvas and its own keys (`error_screen_key`).
            EscLayer::ErrorRow => self
                .last_error
                .as_ref()
                .is_some_and(|e| e.level != crate::error::ErrorLevel::Full),
            EscLayer::InfoLines => !self.slash_info_lines.is_empty(),
            EscLayer::HelpPanel => self.help_visible,
            EscLayer::TaskPanel => self.tasks_visible && !self.tasks_auto,
            EscLayer::Interrupt => self.busy,
            EscLayer::BashMode => self.bash_mode && self.input.is_empty(),
            EscLayer::ClearInput => !self.input.is_empty(),
        }
    }

    /// Closes one layer. A layer with a key handler of its own keeps its close
    /// semantics there (the model picker returns one level at a time, search
    /// must not adopt its hit); the stack only decides which layer hears the key.
    fn esc_dismiss(&mut self, layer: EscLayer, now: std::time::Instant) -> bool {
        const ESC: KeyCode = KeyCode::Esc;
        const NONE: KeyModifiers = KeyModifiers::NONE;
        match layer {
            EscLayer::AskDialog => self.ask_key_at(ESC, NONE, now),
            EscLayer::Menu => {
                self.model_menu_key(ESC, NONE)
                    || self.think_menu_key(ESC, NONE)
                    || self.theme_menu_key(ESC, NONE)
                    || self.resume_menu_key(ESC, NONE)
                    || self.provider_menu_key(ESC, NONE)
            }
            EscLayer::AgentManager => self.agent_manager_key(ESC, NONE),
            EscLayer::SlashDropdown => self.slash_menu_key(ESC, NONE),
            EscLayer::MentionDropdown => self.mention_menu_key(ESC, NONE),
            EscLayer::Search => self.search_key(ESC, NONE),
            // A Page/Field error row is dismissable like every other overlay —
            // it used to sit above the prompt until the next turn started.
            EscLayer::ErrorRow => {
                self.last_error = None;
                self.dirty = true;
                true
            }
            EscLayer::InfoLines => {
                self.slash_info_lines.clear();
                self.dirty = true;
                true
            }
            EscLayer::HelpPanel => {
                self.help_visible = false;
                true
            }
            // The tasks panel opened with ctrl+t closes with Esc (it used to have
            // no exit at all — the ? panel closed, this one squatted).
            EscLayer::TaskPanel => {
                self.tasks_visible = false;
                self.dirty = true;
                true
            }
            EscLayer::Interrupt => {
                self.interrupt(now);
                true
            }
            EscLayer::BashMode => {
                self.bash_mode = false;
                true
            }
            EscLayer::ClearInput => self.esc_clear_input(now),
        }
    }

    /// esc-esc on a non-empty input: the second press inside [`ESC_WINDOW`]
    /// clears the draft into history, retrievable with ↑.
    fn esc_clear_input(&mut self, now: std::time::Instant) -> bool {
        let armed = self
            .esc_at
            .is_some_and(|at| now.duration_since(at) <= ESC_WINDOW);
        if armed {
            self.clear_input_into_history();
            self.esc_at = None;
            self.notice = None;
            return true;
        }
        self.esc_at = Some(now);
        self.notice = Some("Press esc again to clear");
        self.notice_until = Some(now + ESC_WINDOW);
        true
    }

    /// What Esc does to a running turn right now. With a layer stacked above
    /// the interrupt, Esc closes that layer and the turn keeps running — the
    /// status row must not promise otherwise.
    pub(crate) fn esc_busy_hint(&self) -> &'static str {
        match self.esc_layer() {
            Some(EscLayer::Interrupt) | None => "esc to interrupt",
            Some(_) => "esc to close",
        }
    }

    /// The whole hint on the running-status row. Esc is always offered; ctrl+b
    /// joins it only while a foreground shell command is in flight, because that
    /// is the only time it means "background this" (D84).
    pub(crate) fn busy_hint(&self) -> String {
        let esc = self.esc_busy_hint();
        if self.live.running() {
            return format!("{esc} · ctrl+b to run in background");
        }
        esc.to_string()
    }

    /// Interrupts the current turn (Esc / Ctrl+C while busy). The first request is stamped
    /// so Ctrl+C can tell "the turn is stopping" from "the turn is never going to answer".
    fn interrupt(&mut self, now: std::time::Instant) {
        self.interrupted = true;
        self.interrupt_at.get_or_insert(now);
        self.cancel_tx.send_replace(true);
        // The dialog goes with the turn: the user asked for everything in flight
        // to stop, and a dialog is in flight.
        self.cancel_asks(false);
    }

    /// Ctrl+<char> editing commands (readline semantics).
    ///
    /// `chord` says the previous key was `ctrl+x`, which turns this key's
    /// `ctrl+e` into the readline chord for "compose in `$EDITOR`" (D86).
    fn control_key(&mut self, c: char, chord: bool, now: std::time::Instant) -> bool {
        if chord && c == 'e' {
            self.compose_in_editor();
            return true;
        }
        match c {
            'a' => {
                self.cursor = crate::tui::input::line_start(&self.input, self.cursor);
                true
            }
            'e' => {
                self.cursor = crate::tui::input::line_end(&self.input, self.cursor);
                true
            }
            'k' => {
                self.snapshot(EditKind::Bulk);
                let cut = crate::tui::input::kill_to_end(&mut self.input, &mut self.cursor);
                self.composer.kill(cut, KillDir::Forward);
                self.after_edit();
                true
            }
            'u' => {
                // Empty-input ctrl+u in bash mode exits shell mode (CC).
                if self.bash_mode && self.input.is_empty() {
                    self.bash_mode = false;
                    return true;
                }
                self.snapshot(EditKind::Bulk);
                let cut = crate::tui::input::kill_to_start(&mut self.input, &mut self.cursor);
                self.composer.kill(cut, KillDir::Back);
                self.after_edit();
                true
            }
            'w' => {
                self.snapshot(EditKind::Bulk);
                let cut = crate::tui::input::kill_word(&mut self.input, &mut self.cursor);
                self.composer.kill(cut, KillDir::Back);
                self.after_edit();
                true
            }
            'y' => {
                self.yank();
                true
            }
            // ctrl+x arms the `ctrl+x ctrl+e` chord and does nothing on its
            // own. The next key clears it wherever it lands.
            'x' => {
                self.composer.arm_chord(now);
                true
            }
            // ctrl+p/ctrl+n are ↑/↓ exactly — same function, so history
            // browsing and D83's queue pull-back behave identically.
            'p' => self.vertical(false),
            'n' => self.vertical(true),
            // ctrl+d deletes the char after the caret only when there is text (empty input never quits).
            'd' => {
                if self.input.is_empty() {
                    return true;
                }
                self.snapshot(EditKind::Delete);
                crate::tui::input::delete(&mut self.input, &mut self.cursor);
                self.after_edit();
                true
            }
            'j' => {
                self.insert_newline();
                true
            }
            // Ctrl+G composes the draft in `$EDITOR` (D86). It used to open
            // the agents/channels workspace, which D89 retires and which the
            // ctrl+b manager already reaches (Enter on an agent).
            'g' => {
                self.compose_in_editor();
                true
            }
            'l' => {
                self.force_redraw = true;
                self.dirty = true;
                true
            }
            // Ctrl+O opens the transcript view (D82). A question on screen keeps
            // priority: the pager would bury a dialog that is blocking a turn,
            // and the answer is one keystroke away.
            'o' => {
                if self.pending_ask.is_none() {
                    self.open_transcript = true;
                    self.dirty = true;
                }
                true
            }
            'r' => {
                self.open_search();
                true
            }
            's' => {
                self.toggle_stash();
                true
            }
            't' => {
                self.tasks_visible = !self.tasks_visible;
                if self.tasks_visible {
                    // Manually opened: keep the panel even when everything is done (the user explicitly wants to see it).
                    self.tasks_auto = false;
                    self.refresh_tasks();
                }
                self.dirty = true;
                true
            }
            // Ctrl+_ arrives as byte 0x1F, which crossterm reports as Ctrl+7; terminals with the enhanced
            // keyboard protocol report `_` or `/` — all three count as undo.
            '7' | '_' | '/' => {
                self.undo_edit();
                true
            }
            _ => false,
        }
    }

    /// Alt+<char>: sub-word movement and kills, yank-pop, and the thinking
    /// toggle. The motions here are readline's `backward-word` family, so they
    /// stop inside a path (D86); `ctrl+w` keeps the whitespace word a shell has.
    fn alt_key(&mut self, c: char) -> bool {
        match c {
            'b' => {
                self.cursor = crate::tui::input::subword_left(&self.input, self.cursor);
                true
            }
            'f' => {
                self.cursor = crate::tui::input::subword_right(&self.input, self.cursor);
                true
            }
            'd' => {
                self.snapshot(EditKind::Bulk);
                let start = self.cursor;
                let end = crate::tui::input::subword_right(&self.input, self.cursor);
                let cut =
                    crate::tui::input::kill_between(&mut self.input, &mut self.cursor, start, end);
                self.composer.kill(cut, KillDir::Forward);
                self.after_edit();
                true
            }
            'y' => self.yank_pop(),
            't' => {
                self.toggle_thinking();
                true
            }
            _ => false,
        }
    }

    /// Ctrl+Y: insert the top of the kill ring at the caret.
    fn yank(&mut self) {
        let Some(text) = self.composer.top().map(str::to_string) else {
            return;
        };
        self.snapshot(EditKind::Bulk);
        let start = self.cursor;
        crate::tui::input::insert(&mut self.input, &mut self.cursor, &text);
        self.composer.note_yank(start, text.len());
        self.after_edit();
    }

    /// Alt+Y: rotate the ring in place over the text the previous key yanked.
    /// Anywhere else it is not a binding at all — it does nothing and says
    /// nothing, which is what readline does with `yank-pop` out of context.
    fn yank_pop(&mut self) -> bool {
        let Some((start, len, text)) = self.composer.yank_pop() else {
            return false;
        };
        self.snapshot(EditKind::Bulk);
        let end = (start + len).min(self.input.len());
        let start = start.min(end);
        self.input.replace_range(start..end, &text);
        self.cursor = start + text.len();
        self.after_edit();
        true
    }

    /// Ctrl+G / `ctrl+x ctrl+e`: ask the host for the `$EDITOR` round trip.
    /// A pending question keeps priority for the same reason ctrl+o does — the
    /// dialog is blocking a turn and its keys are one keystroke from done.
    fn compose_in_editor(&mut self) {
        if self.pending_ask.is_some() {
            return;
        }
        self.open_editor = true;
        self.dirty = true;
    }

    /// ↑/↓: move within a multi-line input first, then switch history at the first/last row;
    /// ↑ while busy with a queue pulls back the last queued message.
    fn vertical(&mut self, down: bool) -> bool {
        // Pulling back a queued message only happens on empty input: what is being typed should not be clobbered.
        if !down && self.busy && self.input.is_empty() && !self.queued.is_empty() {
            if let Some(entry) = self.queued.last() {
                // The turn may have taken this one already (D83). It is in the request
                // by then, so pulling it into the composer would send it twice: the
                // turn wins, and the absorption event — already on its way — is what
                // takes it out of the queue. Doing nothing here is the whole fix.
                if self.steer.reclaim(entry.id) == crate::steer::Reclaim::Absorbed {
                    return true;
                }
            }
            if let Some(item) = self.queued.pop() {
                self.set_input(item.text);
            }
            self.rearm_steer();
            return true;
        }
        let width = self.input_width();
        if let Some(cursor) = crate::tui::input::move_row(&self.input, self.cursor, width, down) {
            self.cursor = cursor;
            return true;
        }
        let next = if down {
            self.history.newer()
        } else {
            self.history.older(&self.input)
        };
        match next {
            Some(text) => {
                self.snapshot(EditKind::Bulk);
                self.input = text;
                self.cursor = self.input.len();
                self.update_slash_suggestions();
                true
            }
            None => true,
        }
    }

    /// Available input width (terminal width - 2 prefix columns - right padding).
    pub fn input_width(&self) -> usize {
        self.width.saturating_sub(4).max(8)
    }

    /// Newline insertion (`\`+Enter / Ctrl+J / Shift+Enter / Enter inside a paste).
    fn insert_newline(&mut self) {
        self.snapshot(EditKind::Bulk);
        crate::tui::input::insert(&mut self.input, &mut self.cursor, "\n");
        self.after_edit();
    }

    /// Replaces the whole input and puts the caret at the end.
    pub fn set_input(&mut self, text: impl Into<String>) {
        self.input = text.into();
        self.cursor = self.input.len();
        self.update_slash_suggestions();
    }

    /// Clears the input and records it in history (Ctrl+C / double Esc: retrievable with ↑).
    fn clear_input_into_history(&mut self) {
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.undo.clear();
        self.record_history(&text);
        self.update_slash_suggestions();
    }

    /// Wrap-up after every edit: refresh the dropdown suggestions, leave history-browsing mode.
    ///
    /// While the edit is a paste rather than typing ([`Chat::pasting`], D86)
    /// the completion surfaces are closed instead of recomputed. A dropdown is
    /// an answer to typing, and pasted text that happens to contain `@` or `/`
    /// is not asking the question — worse, an `@` dropdown opened mid-burst
    /// claims the Enter that the rest of the paste needs as a newline, and the
    /// funnel's file walk would run once per pasted character. The first
    /// keystroke after the burst re-evaluates, which is the only moment the
    /// end of a burst is observable at all.
    pub(crate) fn after_edit(&mut self) {
        self.history.detach();
        if self.pasting {
            self.clear_slash_suggestions();
            self.mention = None;
        } else {
            self.update_slash_suggestions();
        }
        // G12: error/usage rows clear on the next input — the user has acted on them.
        if !self.slash_error_lines.is_empty() {
            self.slash_error_lines.clear();
            self.slash_error_at = None;
            self.dirty = true;
        }
        // Info output follows the same rule: reading time until the user acts.
        if !self.slash_info_lines.is_empty() {
            self.slash_info_lines.clear();
            self.dirty = true;
        }
    }

    /// Records and persists a prompt. A write failure only degrades to in-session history (once,
    /// no repeated retries).
    pub(crate) fn record_history(&mut self, text: &str) {
        if !self.history.record(text) || !self.history_writable {
            return;
        }
        let path = std::path::PathBuf::from(&self.cwd);
        if crate::tui::history::save(&self.session.home, &path, self.history.entries()).is_err() {
            self.history_writable = false;
        }
    }

    /// Undo stack: consecutive inserts merge into one step; deletes/whole replacements are their own steps.
    pub(crate) fn snapshot(&mut self, kind: EditKind) {
        let coalesce =
            kind != EditKind::Bulk && self.last_edit == Some(kind) && !self.undo.is_empty();
        self.last_edit = Some(kind);
        if coalesce {
            return;
        }
        self.undo.push((self.input.clone(), self.cursor));
        if self.undo.len() > UNDO_MAX {
            self.undo.remove(0);
        }
    }

    /// Ctrl+_: returns to the previous step's text and caret.
    pub(crate) fn undo_edit(&mut self) {
        let Some((text, cursor)) = self.undo.pop() else {
            return;
        };
        self.input = text;
        self.cursor = cursor.min(self.input.len());
        self.last_edit = None;
        self.update_slash_suggestions();
    }

    /// Ctrl+S: with text, stash and clear it; on empty input, restore (including the caret).
    fn toggle_stash(&mut self) {
        if self.input.is_empty() {
            if let Some((text, cursor)) = self.stash.take() {
                self.input = text;
                self.cursor = cursor.min(self.input.len());
                self.update_slash_suggestions();
                self.notice = Some("draft restored");
                self.notice_until = Some(std::time::Instant::now() + CTRL_C_WINDOW);
            }
            return;
        }
        let replaced = self.stash.is_some();
        self.stash = Some((std::mem::take(&mut self.input), self.cursor));
        self.cursor = 0;
        self.last_edit = None;
        self.update_slash_suggestions();
        self.notice = Some(if replaced {
            "draft saved (old draft overwritten) · ctrl+s on an empty input restores it"
        } else {
            "draft saved · ctrl+s on an empty input restores it"
        });
        self.notice_until = Some(std::time::Instant::now() + CTRL_C_WINDOW);
    }

    /// Shift+Tab: default → acceptEdits → plan → default.
    /// bypassPermissions / dontAsk stay in the cycle only when the session started in that mode
    /// (dangerous modes must not be reachable by one mispress).
    fn cycle_permission_mode(&mut self) {
        self.permission_mode = match self.permission_mode {
            PermissionMode::Default => PermissionMode::AcceptEdits,
            PermissionMode::AcceptEdits => PermissionMode::Plan,
            PermissionMode::Plan => PermissionMode::Default,
            // Started in bypass/dontAsk: toggle between it and default, never introducing a new dangerous mode.
            PermissionMode::BypassPermissions | PermissionMode::DontAsk => PermissionMode::Default,
        };
        // From default, switch back to the startup mode (an edge that only bypass/dontAsk sessions have).
        if self.permission_mode == PermissionMode::AcceptEdits
            && matches!(
                self.session.permission_mode,
                PermissionMode::BypassPermissions | PermissionMode::DontAsk
            )
        {
            self.permission_mode = self.session.permission_mode;
        }
        self.dirty = true;
    }

    /// Alt+T: thinking toggle (off ↔ the last non-off level, default medium).
    fn toggle_thinking(&mut self) {
        let current = self.session.runtime.thinking.borrow().clone();
        let next = match current.as_deref() {
            None | Some("off") => self
                .last_thinking
                .clone()
                .unwrap_or_else(|| "medium".into()),
            Some(level) => {
                self.last_thinking = Some(level.to_string());
                "off".to_string()
            }
        };
        self.slash_think(&next);
    }

    /// bash-mode Tab: prefix-completes from the `!` commands run in this session.
    fn complete_bash_history(&mut self) {
        let prefix = self.input.clone();
        let Some(hit) = self
            .bash_history
            .iter()
            .rev()
            .find(|cmd| cmd.starts_with(&prefix) && cmd.as_str() != prefix)
            .cloned()
        else {
            return;
        };
        self.set_input(hit);
    }

    /// Ctrl+R: enters reverse history search (an empty query hits the most recent entry first).
    fn open_search(&mut self) {
        self.close_menus();
        let mut search = HistorySearch::default();
        if let Some((index, hit)) = self.history.search("", None) {
            search.index = Some(index);
            search.hit = Some(hit);
        }
        self.search = Some(search);
        self.clear_slash_suggestions();
    }

    /// Search-mode keys: typing filters, Ctrl+R takes an older hit, Tab/Esc adopt and keep editing,
    /// Enter adopts and submits, Ctrl+C cancels and restores. Returns consumed (always true).
    fn search_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(mut search) = self.search.take() else {
            return false;
        };
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Char('r') if ctrl => {
                if let Some((index, hit)) = self.history.search(&search.query, search.index) {
                    search.index = Some(index);
                    search.hit = Some(hit);
                }
                self.search = Some(search);
            }
            KeyCode::Char('c') if ctrl => {}
            KeyCode::Char(c) if !c.is_control() && !ctrl => {
                search.query.push(c);
                match self.history.search(&search.query, None) {
                    Some((index, hit)) => {
                        search.index = Some(index);
                        search.hit = Some(hit);
                    }
                    None => {
                        search.index = None;
                        search.hit = None;
                    }
                }
                self.search = Some(search);
            }
            KeyCode::Backspace => {
                search.query.pop();
                match self.history.search(&search.query, None) {
                    Some((index, hit)) => {
                        search.index = Some(index);
                        search.hit = Some(hit);
                    }
                    None => {
                        search.index = None;
                        search.hit = None;
                    }
                }
                self.search = Some(search);
            }
            KeyCode::Enter => {
                match search.hit {
                    Some(hit) => {
                        self.set_input(hit);
                        self.submit();
                    }
                    // No match: keep the search layer open (it used to close
                    // silently, eating the Enter).
                    None => self.search = Some(search),
                }
            }
            KeyCode::Tab => {
                if let Some(hit) = search.hit {
                    self.set_input(hit);
                }
            }
            // Esc = cancel, like every other layer (it used to ADOPT the hit —
            // the only place in the app where Esc committed something).
            KeyCode::Esc => {}
            _ => self.search = Some(search),
        }
        true
    }

    /// tick: independent timing for spinner frames and running-state thinking.
    ///
    /// Only set dirty when some row changes with the tick: rebuilding the whole document on idle
    /// equals a 30fps full re-layout, wasting CPU and forcing the host to repaint the viewport every frame.
    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        if self.has_dynamic_rows() {
            self.dirty = true;
        }
        // The bottom entity area follows the registry (agent states/channel counts); dirty only on change.
        if self.tick.is_multiple_of(15) {
            self.refresh_entities();
        }
        // The bottom notice expires with the window it advertises.
        if let Some(until) = self.notice_until
            && std::time::Instant::now() >= until
        {
            self.notice = None;
            self.notice_until = None;
            self.dirty = true;
        }
        // Slash transient hints expire (operation confirmations leave no permanent placeholder);
        // error/usage rows live longer (G12) — they additionally clear on the next input.
        if let Some(at) = self.slash_at
            && at.elapsed() > SLASH_OUTPUT_TTL
        {
            self.slash_lines.clear();
            self.slash_at = None;
            self.dirty = true;
        }
        if let Some(at) = self.slash_error_at
            && at.elapsed() > SLASH_OUTPUT_ERROR_TTL
        {
            self.slash_error_lines.clear();
            self.slash_error_at = None;
            self.dirty = true;
        }
        for msg in &mut self.messages {
            for act in &mut msg.activities {
                if let ActivityKind::Thinking(t) = &mut act.kind
                    && t.state == ThinkingState::Running
                {
                    t.duration_ms = self.tick.saturating_sub(t.start_tick).saturating_mul(33);
                }
            }
        }
    }

    /// Frame number within the update-banner breathing window (animation running → Some; no banner / motion off /
    /// stopped by a keypress / window passed → None, resting). The 270-frame window = 9s = 3 breaths.
    fn update_banner_frame(&self) -> Option<u64> {
        if self.update_banner.is_none() || self.motion_off || self.update_banner_stopped {
            return None;
        }
        let frame = self.tick.saturating_sub(self.update_banner_start);
        (frame < UPDATE_BANNER_FRAMES).then_some(frame)
    }

    /// Whether the update-banner breathing is active (the frame loop keeps dirty set; outside the window it returns to idle).
    pub(crate) fn update_anim_active(&self) -> bool {
        self.update_banner_frame().is_some()
    }

    /// Whether any row changes with the tick (spinner frames / elapsed time / status rows).
    /// false when idle — the tick neither rebuilds the doc nor wakes the component.
    pub fn has_dynamic_rows(&self) -> bool {
        self.busy
            || self.messages.iter().any(|m| {
                m.groups.iter().any(|g| g.active) || m.activities.iter().any(|a| a.is_running())
            })
            || (self.tasks_visible
                && self
                    .tasks_cache
                    .iter()
                    .any(|t| t.status == TodoStatus::InProgress))
            || (self.agent_manager.is_some()
                && self
                    .session
                    .agents
                    .list()
                    .iter()
                    .any(|status| status.state == crate::agents::AgentState::Running))
            || self.update_anim_active()
    }

    /// Whether the host's tick loop has work to do. Returns false when idle so the host skips the whole frame —
    /// with no animation and no pending events, not a single byte is written.
    pub fn needs_tick(&self) -> bool {
        self.has_dynamic_rows()
            || self.slash_at.is_some()
            || self.slash_error_at.is_some()
            || self.notice_until.is_some()
            || !self.events_rx.is_empty()
            || !self.asks_rx.is_empty()
    }

    /// Task-area data source: live snapshot of the on-disk store.
    pub fn tasks(&self) -> Vec<TodoItem> {
        self.session
            .tasks
            .list_ui()
            .into_iter()
            .map(|t| {
                let status = match t.status {
                    crate::tasks::TaskStatus::Pending => TodoStatus::Pending,
                    crate::tasks::TaskStatus::InProgress => TodoStatus::InProgress,
                    crate::tasks::TaskStatus::Completed => TodoStatus::Done,
                };
                TodoItem {
                    text: t.subject,
                    status,
                }
            })
            .collect()
    }

    /// Refreshes the task cache (disk snapshot; called on the tick cadence and after draining events).
    /// Only set dirty when the snapshot changes — row-count changes alter the canvas height, and the render
    /// layer's shape detection triggers a full repaint.
    pub fn refresh_tasks(&mut self) {
        let next = self.tasks();
        if next != self.tasks_cache {
            self.tasks_cache = next;
            self.dirty = true;
        }
        // Auto-opened task area: hide once everything is done (work over, panel leaves),
        // push a 2s transient line for closure + a way back; manually opened panels stay.
        if self.tasks_auto
            && self.tasks_visible
            && !self.tasks_cache.is_empty()
            && self
                .tasks_cache
                .iter()
                .all(|t| t.status == TodoStatus::Done)
        {
            self.tasks_visible = false;
            self.tasks_auto = false;
            let total = self.tasks_cache.len();
            self.push_slash_output(format!("✓ {total}/{total} tasks done · ctrl+t to view"));
        }
    }

    /// Keep at most this many trailing done items; older ones fold into `… N done`.
    const DONE_SHOWN: usize = 3;
    /// Active-item window size; overflow folds into `… +N more`.
    const TODO_SHOWN: usize = 5;

    /// Task-area rows (CC TaskListV2 placement: above the input).
    /// Shown when the expand signal is set and tasks exist; auto-opened lists hide when everything is done
    /// (wrapped up in `refresh_tasks`); manually opened ones stay.
    pub fn task_lines(&self) -> Vec<Line> {
        if !self.tasks_visible {
            return Vec::new();
        }
        let t = &self.tasks_cache;
        if t.is_empty() {
            return Vec::new();
        }
        let theme = &self.theme;
        let mut out = Vec::new();
        // Header: `{spinner}todo · N/M tasks`
        let mut header = Line::empty();
        if t.iter().any(|i| i.status == TodoStatus::InProgress) {
            header.push_styled(
                format!("{} ", crate::tui::activities::spinner(self.tick)),
                SegStyle::fg(theme.claude),
            );
        }
        header.push_styled("todo".to_string(), theme.text());
        let done = t.iter().filter(|i| i.status == TodoStatus::Done).count();
        header.push_styled(
            format!(" · {done}/{} tasks", t.len()),
            SegStyle::fg(theme.inactive),
        );
        out.push(header);
        let done_indices: Vec<usize> = t
            .iter()
            .enumerate()
            .filter(|(_, i)| i.status == TodoStatus::Done)
            .map(|(i, _)| i)
            .collect();
        let shown_done = done_indices.len().min(Self::DONE_SHOWN);
        let hidden_done = done_indices.len() - shown_done;
        if hidden_done > 0 {
            out.push(Line::styled(
                format!("… {} done", hidden_done),
                SegStyle::fg(theme.inactive),
            ));
        }
        for &idx in done_indices.iter().skip(hidden_done) {
            // `☒` + struck-through text (real strikethrough + dim, see Theme::strikethrough).
            let mut line = Line::styled("☒ ", theme.task_done());
            line.push_styled(t[idx].text.clone(), theme.strikethrough());
            out.push(line);
        }
        let active: Vec<&TodoItem> = t.iter().filter(|i| i.status != TodoStatus::Done).collect();
        for item in active.iter().take(Self::TODO_SHOWN) {
            // `☐` not done; in-progress items use the primary accent color for the whole row (CC's active-item highlight).
            let style = match item.status {
                TodoStatus::Pending => theme.task_open(),
                TodoStatus::InProgress => SegStyle::fg(theme.claude).bold(),
                TodoStatus::Done => unreachable!("filtered"),
            };
            let mut line = Line::styled("☐ ", style);
            line.push_styled(item.text.clone(), style);
            out.push(line);
        }
        if active.len() > Self::TODO_SHOWN {
            out.push(Line::styled(
                format!("… +{} more", active.len() - Self::TODO_SHOWN),
                SegStyle::fg(theme.inactive),
            ));
        }
        out
    }

    /// Permission-mode label (footer badge).
    pub fn permission_mode_label(&self) -> &'static str {
        match self.permission_mode {
            PermissionMode::Default => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::BypassPermissions => "bypassPermissions",
            PermissionMode::DontAsk => "dontAsk",
            PermissionMode::Plan => "plan",
        }
    }

    /// Running status row (ActivityIndicator): when busy, returns the verb + elapsed time + tokens
    /// produced — preferring the running tool (summary/name), then the running
    /// thinking (whimsical word), falling back to "Working". Returns None when idle (row hidden).
    pub fn running_status(&self) -> Option<RunningStatus> {
        if !self.busy {
            return None;
        }
        let verb = self
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::Assistant)
            .and_then(|m| {
                m.activities.iter().find_map(|a| match &a.kind {
                    ActivityKind::Tool(t) if t.status == ToolStatus::Running => {
                        Some(if t.summary.is_empty() {
                            t.name.to_string()
                        } else {
                            t.summary.clone()
                        })
                    }
                    // Running background task/subagent (ActivityIndicator shows the agent activeForm):
                    // the label is `Agent: <description>`.
                    ActivityKind::Watch(w) if w.status == WatchState::Running => {
                        Some(w.label.clone())
                    }
                    ActivityKind::Thinking(t) if t.state == ThinkingState::Running => {
                        Some(t.stage.to_string())
                    }
                    _ => None,
                })
            })
            .unwrap_or_else(|| "Working".to_string());
        let elapsed = self
            .turn_started
            .map(|t| t.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        Some(RunningStatus {
            verb,
            elapsed,
            tokens: self.output_tokens,
        })
    }

    pub fn token_rate_label(&self) -> Option<String> {
        if !self.busy {
            return None;
        }
        self.token_rate
            .label(std::time::Instant::now(), self.motion_off)
    }

    pub fn context_usage(&self) -> crate::context_usage::ContextUsage {
        self.context_usage
    }

    /// Input-area rendered rows — the single source for the row-count model and rendering:
    /// chrome height is counted from it and assembly emits rows from it.
    ///
    /// No cursor glyph is drawn here: the terminal's own cursor is the only caret
    /// ([`crate::tui::chrome::prompt`] attaches it via `El::Caret`), so it overlays the cell it
    /// sits on and never pushes the text after it aside.
    ///
    /// Empty input gets a one-line dim placeholder; multi-line input beyond [`INPUT_ROWS_MAX`] shows only
    /// the screen around the caret (tail-aligned), so the row count always has an upper bound.
    pub fn prompt_lines(&self) -> Vec<Line> {
        let style = SegStyle::fg(self.theme.text);
        // Search mode: the input row shows the current hit; the query sits in the hint line below.
        if let Some(search) = &self.search {
            let hit = search.hit.clone().unwrap_or_default();
            return vec![Line::styled(one_line(&hit, self.input_width()), style)];
        }
        if self.input.is_empty() {
            // The real cursor rests on the placeholder's first cell; the hint keeps every
            // character (the terminal inverts the cell it covers).
            return vec![Line::styled(
                crate::tui::keys::INPUT_PLACEHOLDER.to_string(),
                self.theme.dim(),
            )];
        }
        let width = self.input_width();
        let lines = crate::tui::input::visual_lines(&self.input, width);
        let (row, _) = crate::tui::input::cursor_cell(&self.input, &lines, self.cursor);
        let start = row.saturating_sub(INPUT_ROWS_MAX - 1);
        lines
            .iter()
            .skip(start)
            .take(INPUT_ROWS_MAX)
            .map(|line| Line::styled(line.text.clone(), style))
            // Each row must occupy exactly one line: history-filled text may contain tabs (folded to spaces),
            // otherwise the column-width math and canvas height would both drift.
            .map(|mut line| {
                line.sanitize();
                line
            })
            .collect()
    }

    /// Refreshes the bottom entity-area snapshot (running agents + channels). Dirty only on change.
    pub fn refresh_entities(&mut self) {
        let mut fresh: Vec<EntityRow> = self
            .session
            .agents
            .list()
            .into_iter()
            .filter(|s| s.state == crate::agents::AgentState::Running)
            .map(|s| EntityRow::Agent {
                name: s.name,
                state: s.state.label(),
                model: s.model,
                thinking: s.thinking,
            })
            .collect();
        fresh.extend(
            self.session
                .channels
                .list()
                .into_iter()
                .map(|c| EntityRow::Channel {
                    name: c.name,
                    seq: c.seq,
                    frozen: c.frozen,
                }),
        );
        if fresh != self.entities {
            self.entities = fresh;
            self.dirty = true;
        }
    }

    /// Bottom entity area: a compact presence summary of what is running.
    ///
    /// ↑/↓ used to open an inline selector here, which cost the composer its
    /// history recall — the two keys a terminal user reaches for first. Running
    /// agents are reached with ctrl+b and ctrl+g instead (D80).
    pub fn entity_rows(&self, width: usize) -> Vec<Line> {
        if self.entities.is_empty() {
            return Vec::new();
        }
        let summary = self
            .entities
            .iter()
            .map(|e| match e {
                EntityRow::Agent {
                    name,
                    state,
                    model,
                    thinking,
                } => format!(
                    "◉ {name} · {model} · {} · {state}",
                    thinking.as_deref().unwrap_or("off")
                ),
                EntityRow::Channel { name, seq, frozen } => {
                    format!("◇ #{name}({seq}{})", if *frozen { "❄" } else { "" })
                }
            })
            .collect::<Vec<_>>()
            .join(" · ");
        vec![Line::styled(
            one_line(
                &format!("  {summary} — ctrl+g workspace · ctrl+b manage"),
                width,
            ),
            SegStyle::fg(self.theme.inactive),
        )]
    }

    /// The running command's output so far: dim, indented under the `⎿` row it
    /// belongs to (D84).
    ///
    /// These rows live in the redrawn tail region by construction — a running tool
    /// keeps its message unsettled, so nothing here can reach scrollback, and a
    /// finished command leaves nothing behind to unprint.
    pub(crate) fn bash_tail_rows(&self, width: usize) -> Vec<Line> {
        let Some(tail) = &self.bash_tail else {
            return Vec::new();
        };
        let indent = crate::tui::activities::RESULT_INDENT;
        let mut rows = Vec::new();
        // What is being left out, before what is kept: five lines of five reads
        // very differently from five lines of twelve hundred.
        if tail.total_lines > tail.lines.len() {
            rows.push(Line::styled(
                one_line(&format!("{indent}… {} lines", tail.total_lines), width),
                SegStyle::fg(self.theme.inactive),
            ));
        }
        for line in &tail.lines {
            rows.push(Line::styled(
                one_line(&format!("{indent}{line}"), width),
                SegStyle::fg(self.theme.inactive),
            ));
        }
        rows
    }

    /// Main-view entry for running background-agent management — and, while a
    /// shell command is running in the foreground, for moving that command to the
    /// background instead (D84).
    ///
    /// The running command wins: it is the thing the key is about right now, it is
    /// on screen with its tail under it, and it stops being promotable the moment
    /// it exits — after which ctrl+b means what it meant before (D80).
    pub fn agent_manager_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        if self.agent_manager.is_none() && code == KeyCode::Char('b') && ctrl && self.live.promote()
        {
            // The tail's rows go with the row they hung under; the command reappears
            // in the task panel as the background task it now is.
            self.bash_tail = None;
            self.dirty = true;
            return true;
        }
        if self.agent_manager.is_none() && code == KeyCode::Char('b') && ctrl {
            self.agent_manager = Some(AgentManager::List { selected: 0 });
            self.dirty = true;
            return true;
        }
        let Some(mut manager) = self.agent_manager.take() else {
            return false;
        };
        let running = self
            .session
            .agents
            .list()
            .into_iter()
            .filter(|status| status.state == crate::agents::AgentState::Running)
            .collect::<Vec<_>>();
        let keep = match &mut manager {
            AgentManager::List { selected } => {
                *selected = (*selected).min(running.len().saturating_sub(1));
                match code {
                    KeyCode::Up => {
                        *selected = selected.saturating_sub(1);
                        true
                    }
                    KeyCode::Down => {
                        *selected = (*selected + 1).min(running.len().saturating_sub(1));
                        true
                    }
                    KeyCode::Enter => {
                        if let Some(status) = running.get(*selected) {
                            manager = AgentManager::Detail {
                                name: status.name.clone(),
                            };
                        }
                        true
                    }
                    KeyCode::Char('x') => {
                        if let Some(status) = running.get(*selected) {
                            self.stop_agent_from_manager(&status.name);
                        }
                        true
                    }
                    KeyCode::Esc => false,
                    _ => {
                        self.agent_manager = Some(manager);
                        return true;
                    }
                }
            }
            AgentManager::Detail { name } => match code {
                KeyCode::Char('x') => {
                    self.stop_agent_from_manager(name);
                    false
                }
                KeyCode::Left | KeyCode::Esc => {
                    manager = AgentManager::List { selected: 0 };
                    true
                }
                // Enter opens this agent's DM. The entity strip used to be the
                // only way in, at the cost of the composer's ↑/↓; the manager
                // already has the agent in hand, so the way in moved here (D80).
                KeyCode::Enter => {
                    self.open_entity = Some(EntityOpen::Agent(name.clone()));
                    false
                }
                KeyCode::Char(' ') => false,
                _ => {
                    self.agent_manager = Some(manager);
                    return true;
                }
            },
        };
        if keep {
            self.agent_manager = Some(manager);
        }
        self.dirty = true;
        true
    }

    fn stop_agent_from_manager(&mut self, name: &str) {
        match self.session.agents.stop(name) {
            Ok((watch_id, dropped)) => {
                if let Some(id) = watch_id {
                    self.session.watch.set_state(
                        id,
                        WatchState::Cancelled,
                        Some("stopped".to_string()),
                        None,
                    );
                }
                self.push_warning(if dropped == 0 {
                    format!("stopped {name}")
                } else {
                    format!("stopped {name} · {dropped} queued instructions discarded")
                });
                self.notice_until = Some(std::time::Instant::now() + CTRL_C_WINDOW);
                self.refresh_entities();
            }
            Err(error) => self.push_warning(error),
        }
    }

    /// Rows for the main-view manager overlay.
    pub fn agent_manager_rows(&self, width: usize) -> Vec<Row> {
        let Some(manager) = &self.agent_manager else {
            return Vec::new();
        };
        let statuses = self.session.agents.list();
        let running = statuses
            .iter()
            .filter(|status| status.state == crate::agents::AgentState::Running)
            .collect::<Vec<_>>();
        match manager {
            AgentManager::List { selected } => {
                let mut rows = vec![Row::new(Line::styled(
                    format!("Background agents · {} running", running.len()),
                    SegStyle::fg(self.theme.text).bold(),
                ))];
                if running.is_empty() {
                    rows.push(Row::new(Line::styled(
                        "No agents currently running",
                        SegStyle::fg(self.theme.inactive),
                    )));
                } else {
                    let selected = (*selected).min(running.len() - 1);
                    let start = selected.saturating_sub(AGENT_MANAGER_ROWS_MAX - 1);
                    for (index, status) in running
                        .iter()
                        .enumerate()
                        .skip(start)
                        .take(AGENT_MANAGER_ROWS_MAX)
                    {
                        let activity = status
                            .recent_activity
                            .last()
                            .map(String::as_str)
                            .unwrap_or("initializing…");
                        let prefix = if index == selected { "❯ " } else { "  " };
                        let stats = format_agent_stats(status);
                        rows.push(Row::new(Line::styled(
                            one_line(
                                &format!(
                                    "{prefix}◉ {} · {} · {} · {activity}",
                                    status.name, status.description, stats
                                ),
                                width.saturating_sub(2),
                            ),
                            SegStyle::fg(if prefix == "❯ " {
                                self.theme.permission
                            } else {
                                self.theme.text
                            }),
                        )));
                    }
                    if running.len() > AGENT_MANAGER_ROWS_MAX {
                        rows.push(Row::new(Line::styled(
                            format!("  … {} running agents", running.len()),
                            SegStyle::fg(self.theme.inactive),
                        )));
                    }
                }
                rows.push(Row::new(Line::styled(
                    "↑/↓ select · Enter details · x stop · Esc close",
                    SegStyle::fg(self.theme.inactive),
                )));
                manager_box(rows, width, &self.theme)
            }
            AgentManager::Detail { name } => {
                let status = statuses.iter().find(|status| &status.name == name);
                let mut rows = vec![Row::new(Line::styled(
                    status.map_or_else(
                        || name.clone(),
                        |s| format!("{} › {}", s.name, s.description),
                    ),
                    SegStyle::fg(self.theme.text).bold(),
                ))];
                if let Some(status) = status {
                    rows.push(Row::new(Line::styled(
                        format!("{} · {}", status.state.label(), format_agent_stats(status)),
                        SegStyle::fg(self.theme.inactive),
                    )));
                    rows.push(Row::new(Line::empty()));
                    rows.push(Row::new(Line::styled(
                        "Progress",
                        SegStyle::fg(self.theme.inactive).bold(),
                    )));
                    if status.recent_activity.is_empty() {
                        rows.push(Row::new(Line::styled(
                            "› initializing…",
                            SegStyle::fg(self.theme.inactive),
                        )));
                    } else {
                        for (index, activity) in status.recent_activity.iter().enumerate() {
                            let prefix = if index + 1 == status.recent_activity.len() {
                                "› "
                            } else {
                                "  "
                            };
                            rows.push(Row::new(Line::styled(
                                one_line(&format!("{prefix}{activity}"), width.saturating_sub(2)),
                                SegStyle::fg(if prefix == "› " {
                                    self.theme.text
                                } else {
                                    self.theme.inactive
                                }),
                            )));
                        }
                    }
                    rows.push(Row::new(Line::empty()));
                    rows.push(Row::new(Line::styled(
                        "Prompt",
                        SegStyle::fg(self.theme.inactive).bold(),
                    )));
                    let prompt = if status.prompt.is_empty() {
                        "(prompt unavailable)".to_string()
                    } else {
                        truncate_chars(&status.prompt, AGENT_PROMPT_CHARS_MAX)
                    };
                    let prompt_rows = wrap_words(&prompt, width.saturating_sub(4).max(1));
                    for line in prompt_rows.iter().take(AGENT_PROMPT_ROWS_MAX) {
                        rows.push(Row::new(Line::plain(line.clone())));
                    }
                    if prompt_rows.len() > AGENT_PROMPT_ROWS_MAX {
                        rows.push(Row::new(Line::styled(
                            format!(
                                "… +{} prompt lines",
                                prompt_rows.len() - AGENT_PROMPT_ROWS_MAX
                            ),
                            SegStyle::fg(self.theme.inactive),
                        )));
                    }
                } else {
                    rows.push(Row::new(Line::styled(
                        "Agent is no longer available",
                        SegStyle::fg(self.theme.inactive),
                    )));
                }
                rows.push(Row::new(Line::styled(
                    "←/Esc back · Enter opens DM · x stop",
                    SegStyle::fg(self.theme.inactive),
                )));
                manager_box(rows, width, &self.theme)
            }
        }
    }

    /// `?` panel rows (single source for the shortcut table). The row budget comes from the terminal height:
    /// the panel must not push the viewport above the terminal height.
    pub fn help_lines(&self) -> Vec<String> {
        if !self.help_visible {
            return Vec::new();
        }
        // Reserve: input 3 rows + footer 1 + a 4-row margin for status/suggestions + 1 safety row.
        let budget = self.height.saturating_sub(9);
        crate::tui::keys::help_lines(self.width.saturating_sub(2), budget)
    }

    /// Queued-message rows (dim `> {text}` below the input); overflow folds into one row.
    pub fn queue_lines(&self) -> Vec<String> {
        if self.queued.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<String> = self
            .queued
            .iter()
            .take(QUEUE_ROWS_MAX)
            .map(|item| format!("> {}", one_line(&item.text, self.width.saturating_sub(4))))
            .collect();
        if self.queued.len() > QUEUE_ROWS_MAX {
            out.push(format!(
                "… +{} more queued",
                self.queued.len() - QUEUE_ROWS_MAX
            ));
        }
        out
    }

    /// The hint under the queued rows, in CC's wording. It is only true while a turn is
    /// running: with nothing in flight the queue is about to submit itself, and there is
    /// no window in which editing it would mean anything.
    pub fn queue_hint(&self) -> Option<&'static str> {
        (self.busy && !self.queued.is_empty()).then_some("Press up to edit queued messages")
    }

    /// ctrl+r search hint line (`(reverse-i-search)`query': hit`).
    pub fn search_line(&self) -> Option<String> {
        let search = self.search.as_ref()?;
        let (prefix, hit) = match search.hit.as_deref() {
            Some(hit) => ("(reverse-i-search)", hit),
            // bash shows failure explicitly; silence read as "found nothing? or broken?".
            None if !search.query.is_empty() => ("(failed reverse-i-search)", ""),
            None => ("(reverse-i-search)", ""),
        };
        Some(one_line(
            &format!(
                "{prefix}`{}': {hit}   — enter submits · tab accepts · ctrl+r older · esc cancels",
                search.query
            ),
            self.width.saturating_sub(2),
        ))
    }

    /// Scroll/doc consistency: clamp the scroll to the doc end; auto_scroll sticks to the bottom.
    pub fn reconcile_scroll(&mut self, viewport: usize) {
        self.viewport_height = viewport;
        let total = self.doc.rows.len();
        let max_scroll = total.saturating_sub(viewport);
        if self.auto_scroll {
            self.scroll = max_scroll;
        }
        let scroll = self.scroll.min(max_scroll);
        self.scroll = scroll;
        if scroll == max_scroll {
            self.auto_scroll = true;
        }
    }

    /// A message's own static settlement condition (independent of predecessors):
    /// streaming stopped, no running activities, no images loading.
    fn message_static_settled(&self, i: usize) -> bool {
        if Some(i) == self.stream_msg {
            return false;
        }
        let m = &self.messages[i];
        // Images load asynchronously. Settling (and therefore flushing) a
        // message whose images are still in flight would print the
        // `#[image]` fallback rows into the scrollback for good: the kitty
        // sequence is only emitted at flush time, and `build_rows` skips
        // flushed segments, so the picture could never appear. Loads that
        // fail drop out of `images_pending` and settle as the placeholder,
        // which is the intended failure display.
        if !self.images_pending.is_empty()
            && gfx::extract_image_urls(&m.text)
                .iter()
                .any(|url| self.images_pending.contains(url))
        {
            return false;
        }
        !m.groups.iter().any(|g| g.active) && !m.activities.iter().any(|a| a.is_running())
    }

    /// Whether a message is "settled": its rows no longer change (stream stopped, no running activities).
    /// REPL mode: settled messages print into scrollback in one go; unsettled ones stay in the
    /// dynamic tail for in-place redraws. Settling is one-way — once true, the rows never change.
    ///
    /// Sequential settlement: an answer message inserted mid-turn sits after the streaming
    /// assistant message; if a predecessor isn't settled (still streaming / tool running /
    /// image loading), this message must not settle either — flushing past a streaming row
    /// would print an intermediate state into scrollback as unchangeable residue (same
    /// invariant as `streaming_content_is_not_flushed_until_settled`; today's message model
    /// always has predecessors settled, this guard only constrains new scenarios).
    ///
    /// Prefix settlement is monotone (0..=i all settled ⟺ 0..i-1 all settled and i itself
    /// static), so recursing from the previous message is linear — do NOT recurse into every
    /// predecessor: with everything settled that is exponential (freezes the hot path on
    /// every build_rows).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn message_settled(&self, i: usize) -> bool {
        (i == 0 || self.message_settled(i - 1)) && self.message_static_settled(i)
    }

    /// Build the scrolling document: welcome card + messages (text and activities interleaved at their insert points) +
    /// permission-request blocks. The block list is laid out by [`crate::tui::statics::layout`]:
    /// `doc.settled` / checkpoints = the settled prefix (welcome card + all settled messages;
    /// permission-request blocks are never settled).
    ///
    /// In inline mode, segments already flushed ([`Chat::flushed_segments`]) are skipped wholesale:
    /// the doc only covers the dynamic tail, so more flushing means cheaper rebuilds.
    pub fn build_rows(&mut self, width: usize) -> &Doc {
        // The markdown render cache is not width-aware — clear it when the width changes,
        // otherwise message text keeps wrapping at the old width after a resize.
        if self.prev_build_width != width {
            self.prev_build_width = width;
            self.reply_cache.clear();
        }
        let theme = self.theme.clone();
        // Segment numbering: 0 = welcome card, i+1 = messages[i]. The clamp is defensive: if the message set
        // is replaced wholesale (/clear, /resume) without the cursor resetting, better to re-render
        // than leave a blank screen.
        let skip = self.flushed_segments.min(self.messages.len() + 1);
        self.tail_start = 0;
        self.mark_base = 0;

        // Prefix-monotone settlement, precomputed in one pass (recursing per
        // message inside the loop would be quadratic on the hot path).
        let mut settled_flags = Vec::with_capacity(self.messages.len());
        let mut prefix_settled = true;
        for i in 0..self.messages.len() {
            prefix_settled = prefix_settled && self.message_static_settled(i);
            settled_flags.push(prefix_settled);
        }

        let mut blocks: Vec<Block> = Vec::new();
        if skip == 0 {
            blocks.push(Block::settled(self.welcome_el(width, &theme), true));
        }
        let pal = crate::tui::slack::Palette::new(&theme);
        for (i, &settled) in settled_flags
            .iter()
            .enumerate()
            .skip(skip.saturating_sub(1))
        {
            let role = self.messages[i].role;
            // The band is the experimental face (`experimental.chatAvatars`): switched
            // off, a message opens on its body, exactly as it did before D50.
            let band = self.chat_avatars.then(|| self.sender_band_el(role, &pal));
            let body = match role {
                Role::User => {
                    let mut rows = user_message_rows(&self.messages[i].text, width, &theme);
                    // Send time under the bubble (issue #41), the same dim stamp
                    // every view trails its message bodies with. A state line gets
                    // none: nothing was sent, and the line is a state, not a message.
                    let time = if crate::tui::chat::is_state_line(&self.messages[i].text) {
                        String::new()
                    } else {
                        crate::tui::slack::stamp(self.messages[i].at)
                    };
                    if !time.is_empty() {
                        rows.push(Row::new(Line::styled(format!("  {time}"), theme.dim())));
                    }
                    El::Rows(rows)
                }
                Role::Assistant => self.assistant_el(i, width, &theme, settled, &pal),
            };
            // Message block spacing (CC marginTop=1): one blank row after the welcome card and before each message.
            let mut stack = vec![El::Blank];
            stack.extend(band);
            stack.push(body);
            blocks.push(Block::settled(El::col(stack), settled));
        }
        if let Some(ask) = self.ask_el(&theme) {
            blocks.push(Block::live(ask));
        }
        // Slash command output (/help /status /compact etc.): transient hints — rendered after messages and
        // above the input, **never settled or flushed**, auto-dismissed after the tick timeout (SLASH_OUTPUT_TTL).
        if !self.slash_lines.is_empty() {
            blocks.push(Block::transient(El::Lines(
                self.slash_lines
                    .iter()
                    .map(|line| Line::styled(one_line(line, width), SegStyle::fg(theme.text)))
                    .collect(),
            )));
        }
        // Error/usage rows (G12/G13): longer TTL, error color, clear on the next input.
        if !self.slash_error_lines.is_empty() {
            blocks.push(Block::transient(El::Lines(
                self.slash_error_lines
                    .iter()
                    .map(|line| Line::styled(one_line(line, width), SegStyle::fg(theme.error)))
                    .collect(),
            )));
        }
        // Informational output (/help /status …): persists until the next
        // input/Esc; never settles into scrollback.
        if !self.slash_info_lines.is_empty() {
            blocks.push(Block::transient(El::Lines(
                self.slash_info_lines
                    .iter()
                    .map(|line| Line::styled(one_line(line, width), SegStyle::fg(theme.text)))
                    .collect(),
            )));
        }

        self.doc = crate::tui::statics::layout(blocks);
        &self.doc
    }

    /// Welcome-card block. It settles at birth but stays in the live doc
    /// (banner breathing, re-wrap on resize) until it crosses the window top.
    fn welcome_el(&self, width: usize, theme: &Theme) -> El {
        // New-version banner (update-banner): breathing color inside the window; outside / no banner → resting rest or None.
        let banner = self.update_banner.as_deref().map(|v| {
            let frame = self.update_banner_frame().unwrap_or(UPDATE_BANNER_FRAMES);
            (v, update_color(theme, frame, self.motion_off))
        });
        let provider = self.session.runtime.provider.borrow().clone();
        El::Rows(welcome_card_rows(
            theme,
            &self.session.runtime.model.borrow(),
            self.permission_mode_label(),
            &self.cwd,
            width,
            banner,
            !self.session.client.is_configured(&provider),
        ))
    }

    /// The band above a message: who is speaking, as a portrait and a name.
    ///
    /// The names are the room's own — `main` for the hub, and the human's own
    /// messages read `You` exactly as the workspace already writes them
    /// ([`crate::tui::slack::message_rows`]). So the name on the band is the name
    /// that addresses the speaker, and the two views agree without a display-name
    /// table to keep honest in both.
    ///
    /// Neither speaker is a blueprint member, so both faces come from the same
    /// name hash the workspace falls back to — pinning is for the crew.
    fn sender_band_el(&mut self, role: Role, pal: &crate::tui::slack::Palette) -> El {
        let (name, shown) = match role {
            Role::User => (crate::channels::USER_NAME, "You"),
            Role::Assistant => (crate::channels::HUB_NAME, crate::channels::HUB_NAME),
        };
        let index = crate::tui::avatar::index_of(name);
        self.faces.insert(index);
        El::Rows(
            crate::tui::slack::sender_band(index, name, shown, self.image_cap.is_some(), pal)
                .into_iter()
                .map(Row::new)
                .collect(),
        )
    }

    /// Assistant message: markdown text and activities interleaved in model
    /// output order; collapse groups fold runs of read/search tools. `settled`
    /// mirrors the old `message_settled(i)` (prefix-monotone flag).
    /// The portrait each of this message's activities wears, resolved in one pass
    /// before the rows are built (the row loop holds a read borrow of `messages`,
    /// and recording a face needs a write).
    ///
    /// Only a subagent watch row gets one, and only where the terminal can place
    /// images: the face is what buys the `⎿` connector's place, so a chip skin —
    /// which has no face to spend — keeps `◉` and the connector exactly as before.
    /// With `experimental.chatAvatars` off the transcript wears no faces at all,
    /// which lands in the same place as a terminal that cannot draw them.
    fn watch_portraits(
        &mut self,
        i: usize,
        pal: &crate::tui::slack::Palette,
    ) -> Vec<Option<Portrait>> {
        if !self.chat_avatars || self.image_cap.is_none() {
            return Vec::new();
        }
        let named: Vec<Option<String>> = self.messages[i]
            .activities
            .iter()
            .map(|act| match &act.kind {
                ActivityKind::Watch(w) if w.kind == crate::watch::WatchKind::Agent => {
                    // `{instance} · {description}` — the address is the prefix.
                    let name = w.label.split(" · ").next().unwrap_or_default().trim();
                    (!name.is_empty()).then(|| name.to_string())
                }
                _ => None,
            })
            .collect();
        named
            .into_iter()
            .map(|name| {
                let name = name?;
                let index = self
                    .faces_pinned
                    .get(&name)
                    .copied()
                    .unwrap_or_else(|| avatar::index_of(&name));
                self.faces.insert(index);
                Some(Portrait {
                    top: crate::tui::slack::gutter_cell(index, &name, 0, true, pal),
                    bottom: crate::tui::slack::gutter_cell(index, &name, 1, true, pal),
                })
            })
            .collect()
    }

    fn assistant_el(
        &mut self,
        i: usize,
        width: usize,
        theme: &Theme,
        settled: bool,
        pal: &crate::tui::slack::Palette,
    ) -> El {
        let portraits = self.watch_portraits(i, pal);
        // Thinking completion row (CC SystemTextMessage `✻ Churned for 40s`):
        // rendered at the end of the message (after text and all tools), from the last completed
        // real thinking block (empty placeholder blocks produce no completion row).
        // Only rendered after the turn ends: while running, `✻ Baked for 0.4s` would appear
        // while tools are still running, contradicting the bottom running-status row.
        let show_done_line = i == self.messages.len() - 1 && self.stream_msg.is_none() || settled;
        // Built before the render closure takes its mutable borrows: the tail is the
        // same rows wherever the running command's row turns out to be inside this
        // message. Only the streaming message can hold one — the same rule the tool
        // events themselves follow — so every other message pays nothing.
        let bash_tail = if self.stream_msg == Some(i) {
            self.bash_tail_rows(width)
        } else {
            Vec::new()
        };
        // Markdown render closure: borrows only disjoint fields to avoid conflicting with
        // the shared read borrow of `self.messages`.
        let mut render = {
            let processor = &mut self.processor;
            let renderer = &mut self.renderer;
            let cache = &mut self.reply_cache;
            let images = &self.images;
            let images_failed = &self.images_failed;
            let image_cap = self.image_cap;
            let images_version = self.images_version;
            move |reply: &str| -> Vec<Line> {
                if reply.is_empty() {
                    return Vec::new();
                }
                if let Some(lines) = cache.get(reply) {
                    return lines.clone();
                }
                renderer.set_width(width.saturating_sub(2));
                // Image cache version changed → sync the renderer (clears its per-block cache).
                if renderer.images_version() != images_version {
                    renderer.set_images(image_cap, images, images_failed, images_version);
                }
                let doc = processor.process_streaming(reply);
                renderer.render(&doc);
                let lines = renderer.lines().to_vec();
                cache.insert(reply.to_string(), lines.clone());
                lines
            }
        };
        let msg = &self.messages[i];
        let text = &msg.text;
        let char_bounds: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
        let mut rendered_chars = 0usize;
        let mut rendered_bytes = 0usize;
        let mut parts: Vec<El> = Vec::new();
        for (idx, act) in msg.activities.iter().enumerate() {
            let pos_chars = msg
                .insert_points
                .get(idx)
                .copied()
                .unwrap_or(rendered_chars)
                .min(text.chars().count());
            if pos_chars > rendered_chars {
                let seg_end = char_bounds.get(pos_chars).copied().unwrap_or(text.len());
                let reply = render(&text[rendered_bytes..seg_end]);
                parts.push(text_el(theme, reply));
                rendered_chars = pos_chars;
                rendered_bytes = seg_end;
            }
            let group_idx = msg.group_of.get(idx).copied().flatten();
            let group_collapsed = group_idx.is_some_and(|g| !msg.groups[g].expanded);
            let is_group_head =
                group_idx.is_some_and(|g| msg.groups[g].activities.first() == Some(&idx));
            if group_collapsed && !is_group_head {
                continue;
            }
            if let Some(g) = group_idx
                && !msg.groups[g].expanded
            {
                // Collapse group: a one-line rule summary (`Read 3 files (ctrl+o to expand)`).
                let in_progress = msg.groups[g].active
                    && msg.groups[g].activities.iter().any(|&ai| {
                        matches!(
                            msg.activities.get(ai),
                            Some(a) if matches!(
                                &a.kind,
                                ActivityKind::Tool(t)
                                    if t.status == ToolStatus::Running
                            )
                        )
                    });
                let summary = collapse_summary(&msg.groups[g], in_progress);
                // A failure inside the fold is otherwise invisible: the summary counts the call
                // as if it had worked, and only ctrl+o shows the error row. Say so on the
                // summary line — it matters most for the calls that change something.
                let failed = msg.groups[g]
                    .activities
                    .iter()
                    .filter(|&&ai| {
                        matches!(
                            msg.activities.get(ai),
                            Some(a) if matches!(
                                &a.kind,
                                ActivityKind::Tool(t) if t.status == ToolStatus::Error
                            )
                        )
                    })
                    .count();
                // The group row is a static `⏺ …`: the spinner only lives in the bottom status row.
                let mut line = Line::styled(
                    "⏺ ",
                    if in_progress {
                        theme.dim()
                    } else {
                        theme.tool_done()
                    },
                );
                line.push_styled(summary, SegStyle::fg(theme.text));
                if failed > 0 {
                    line.push_styled(format!(" · {failed} failed"), SegStyle::fg(theme.error));
                }
                line.push_styled(
                    " (ctrl+o to expand)".to_string(),
                    SegStyle::fg(theme.inactive),
                );
                parts.push(El::click(
                    ClickTarget::Group {
                        message: i,
                        group: g,
                    },
                    El::Line(line),
                ));
                // Below a running collapse group, show the most recent tool's input (the CC ⎿ row).
                // The hint may be a multi-line bash command: single-line it and truncate by width,
                // otherwise the row balloons into multiple lines and the row model drifts from the canvas.
                // It sits outside the Click wrapper — only the summary row toggles.
                if in_progress && let Some(hint) = &msg.groups[g].last_hint {
                    parts.push(El::Line(Line::styled(
                        one_line(&format!("  ⎿  {hint}"), width),
                        SegStyle::fg(theme.inactive),
                    )));
                }
                // The folded command is the one running: its tail belongs under the
                // hint row, which is the only row the fold gives it (D84).
                if in_progress
                    && msg.groups[g]
                        .activities
                        .iter()
                        .any(|&ai| msg.activities.get(ai).is_some_and(is_running_bash))
                {
                    parts.extend(bash_tail.iter().cloned().map(El::Line));
                }
                continue;
            }
            let (lines, local) = layout_activity(
                act,
                &[idx],
                0,
                theme,
                portraits.get(idx).and_then(|p| p.as_ref()),
                &mut |reply: &str| render(reply),
            );
            let activity = El::Annotated {
                rows: lines.into_iter().map(Row::new).collect(),
                clicks: local
                    .into_iter()
                    .map(|range| LocalClick {
                        start: range.start as usize,
                        end: range.end as usize,
                        target: ClickTarget::Activity {
                            message: i,
                            path: range.path,
                        },
                    })
                    .collect(),
            };
            // Expanded group: the group-head tool row doubles as the group summary row — the
            // enclosing Click is emitted first, so clicking it collapses the group back.
            parts.push(if let Some(g) = group_idx {
                El::click(
                    ClickTarget::Group {
                        message: i,
                        group: g,
                    },
                    activity,
                )
            } else {
                activity
            });
            // The command's output so far, under its own row. Outside the Annotated
            // block: these rows are evidence, not a click target, and they are gone
            // by the time the row is worth clicking.
            if is_running_bash(act) {
                parts.extend(bash_tail.iter().cloned().map(El::Line));
            }
        }
        if rendered_bytes < text.len() {
            let reply = render(&text[rendered_bytes..]);
            parts.push(text_el(theme, reply));
        }
        if show_done_line
            && let Some(line) =
                self.messages[i]
                    .activities
                    .iter()
                    .rev()
                    .find_map(|a| match &a.kind {
                        ActivityKind::Thinking(t)
                            if t.state == ThinkingState::Done && !a.content.is_empty() =>
                        {
                            Some(crate::tui::activities::thinking_completion_line(t, theme))
                        }
                        _ => None,
                    })
        {
            parts.push(El::Line(line));
        }
        // Send time after the body (issue #41): only once the turn has finished —
        // a clock under still-streaming text would read as an ending. A markdown
        // body may end in a blank line; the stamp belongs to the message, so it
        // slots in before that spacing rather than floating under it.
        let time = crate::tui::slack::stamp(self.messages[i].at);
        if show_done_line && !time.is_empty() && !parts.is_empty() {
            let stamp_line = Line::styled(format!("  {time}"), theme.dim());
            match parts.last_mut() {
                Some(El::Rows(rows)) => {
                    let keep = rows
                        .iter()
                        .rposition(|r| {
                            r.line.image.is_some() || !r.line.plain_text().trim().is_empty()
                        })
                        .map_or(0, |p| p + 1);
                    rows.insert(keep, Row::new(stamp_line));
                }
                _ => parts.push(El::Line(stamp_line)),
            }
        }
        El::Col(parts)
    }

    /// Resets the flush cursor: after the message set is replaced wholesale (/clear, /resume), segment numbers
    /// are invalid, so the doc rebuilds from the welcome card (new content flushes into scrollback again).
    pub(crate) fn reset_flushed(&mut self) {
        self.flushed_segments = 0;
        self.tail_start = 0;
        self.mark_base = 0;
        self.dirty = true;
    }

    /// After flushing `doc.rows[tail_start..settled]`, advance the cursor: the next rebuild skips
    /// those segments and the current doc's tail start moves up (the canvas stops drawing them before a rebuild).
    // Production advances partially by checkpoints (lazy flush); full advance stays as a test-facing primitive.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn advance_flushed(&mut self) {
        if let Some(mark) = self.doc.settled_marks.last().copied() {
            self.advance_flushed_upto(mark);
        }
    }

    /// After flushing `doc.rows[tail_start..mark.row_end]`, advance the cursor to that checkpoint.
    /// Callable multiple times within one build (`mark_base` absorbs the build-internal accumulators,
    /// preventing double-counting); safe across width-change re-layouts — segment counts are row-number independent.
    pub fn advance_flushed_upto(&mut self, mark: SettledMark) {
        self.flushed_segments += mark.segments.saturating_sub(self.mark_base);
        self.mark_base = mark.segments;
        self.tail_start = mark.row_end;
    }

    /// After a resize the window can hold more: pull the most recently flushed content back into the live doc
    /// to refill it. Old copies in scrollback cannot be physically retracted — accept seeing a duplicate
    /// at the old width when scrolling up (an explicitly accepted trade-off, see research.md D27). Rehydration is
    /// purely bookkeeping (writes nothing to the terminal), bounded by "no more than `doc_budget` rows"; beyond that it rolls back,
    /// guaranteeing no conflict with lazy flushing (after rehydration no settled segment crosses the window top).
    pub fn rehydrate(&mut self, width: usize, doc_budget: usize) {
        loop {
            if self.flushed_segments == 0 {
                break;
            }
            if self.build_rows(width).rows.len() >= doc_budget {
                break;
            }
            self.flushed_segments -= 1;
            if self.build_rows(width).rows.len() > doc_budget {
                self.flushed_segments += 1;
                break;
            }
        }
        self.dirty = true;
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let mut out = chars.by_ref().take(max).collect::<String>();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

fn format_agent_stats(status: &crate::agents::AgentStatus) -> String {
    let elapsed = status.elapsed.unwrap_or_default().as_secs();
    let elapsed = if elapsed >= 60 {
        format!("{}m {:02}s", elapsed / 60, elapsed % 60)
    } else {
        format!("{elapsed}s")
    };
    let tools = if status.tool_uses == 1 {
        "tool"
    } else {
        "tools"
    };
    format!(
        "{elapsed} · {} tokens · {} {tools}",
        status.output_tokens, status.tool_uses
    )
}

fn manager_box(rows: Vec<Row>, width: usize, theme: &Theme) -> Vec<Row> {
    let inner = width.saturating_sub(4).max(1);
    let border = "─".repeat(inner);
    let mut out = Vec::with_capacity(rows.len() + 2);
    out.push(Row::new(Line::styled(
        format!("╭{border}╮"),
        SegStyle::fg(theme.inactive),
    )));
    for row in rows {
        let mut line = Line::styled("│ ", SegStyle::fg(theme.inactive));
        let mut used = 0usize;
        for seg in row.line.segs {
            if used >= inner.saturating_sub(2) {
                break;
            }
            let remaining = inner.saturating_sub(2 + used);
            let text = one_line(&seg.text, remaining.max(1));
            used += text_width(&text);
            line.push_styled(text, seg.style);
        }
        line.push_styled(
            format!("{} │", " ".repeat(inner.saturating_sub(used + 2))),
            SegStyle::fg(theme.inactive),
        );
        let mut boxed = Row::new(line);
        boxed.bg = row.bg;
        boxed.padding_right = row.padding_right;
        out.push(boxed);
    }
    out.push(Row::new(Line::styled(
        format!("╰{border}╯"),
        SegStyle::fg(theme.inactive),
    )));
    out
}

fn text_el(theme: &Theme, reply: Vec<Line>) -> El {
    El::Rows(text_rows(theme, reply))
}

/// Welcome card body (CC WelcomeBox): a starred greeting, the two commands
/// worth knowing, the cwd, and a dim identity line. `bingo` stays `bingo` —
/// this is homage, not impersonation.
///
/// The new-version banner row (update-banner spec v1.1): sits directly above the version-identity row, one blank
/// row from cwd; three segments (static inactive + version/command in breathing color, command bold),
/// breathing only affects the banner's two keyword segments; every other welcome-card element stays static.
fn welcome_rows(
    theme: &Theme,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
    banner: Option<(&str, Color)>,
    unconfigured: bool,
) -> Vec<Line> {
    let mut rows = Vec::new();
    let mut greeting = Line::styled(" ✻ ", SegStyle::fg(theme.claude));
    greeting.push_styled("Welcome back!", theme.text());
    rows.push(greeting);
    rows.push(Line::empty());
    rows.push(Line::styled(
        one_line("   /help for help · /status for your current setup", width),
        theme.dim(),
    ));
    rows.push(Line::empty());
    rows.push(Line::styled(
        one_line(&format!("   cwd: {cwd}"), width),
        theme.dim(),
    ));
    // Onboarding: with no usable credentials, the card says what to do next —
    // the login command lives in here, so the door must open before the key.
    if unconfigured {
        rows.push(Line::empty());
        rows.push(Line::styled(
            one_line(
                "   ⚠ no credentials configured: /provider login codex (ChatGPT subscription) or write apiKey in ~/.config/bingo/settings.json",
                width,
            ),
            SegStyle::fg(theme.warning),
        ));
    }
    // New-version banner row (update-banner spec §1.1): directly above the version-identity row, one blank row from cwd.
    if let Some((v, color)) = banner
        && let Some((pre, ver, mid, cmd)) = banner_segments(v, width)
    {
        rows.push(Line::empty());
        let no_color = std::env::var_os("NO_COLOR").is_some();
        let mut line = Line::styled(&pre, theme.dim());
        if no_color {
            // Monochrome / NO_COLOR fallback: a static bold row (spec §2.5).
            line.push_styled(ver, theme.dim());
            line.push_styled(mid, theme.dim());
            line.push_styled(cmd, theme.dim().bold());
        } else {
            line.push_styled(ver, SegStyle::fg(color));
            line.push_styled(mid, theme.dim());
            line.push_styled(cmd, SegStyle::fg(color).bold());
        }
        rows.push(line);
    }
    rows.push(Line::styled(
        one_line(
            &format!("   bingo v{} · {model} · {mode}", env!("CARGO_PKG_VERSION")),
            width,
        ),
        theme.dim(),
    ));
    rows
}

/// Welcome card rows (with the ╭╮ border), part of the scrollable content.
/// `banner` = the new-version hint (version + current breathing color); None = no banner row.
pub(crate) fn welcome_card_rows(
    theme: &Theme,
    model: &str,
    mode: &str,
    cwd: &str,
    width: usize,
    banner: Option<(&str, Color)>,
    unconfigured: bool,
) -> Vec<Row> {
    let gray = SegStyle::fg(theme.inactive);
    let inner_w = width.saturating_sub(2);
    let mut rows = vec![Row::new(Line::styled(
        format!("╭{}╮", "─".repeat(inner_w)),
        gray,
    ))];
    for line in welcome_rows(theme, model, mode, cwd, inner_w, banner, unconfigured) {
        let mut styled = Line::styled("│", gray);
        let pad = inner_w.saturating_sub(text_width(&line.plain_text()));
        styled.segs.extend(line.segs);
        styled.push_styled(" ".repeat(pad), gray);
        styled.push_styled("│", gray);
        rows.push(Row::new(styled));
    }
    rows.push(Row::new(Line::styled(
        format!("╰{}╯", "─".repeat(inner_w)),
        gray,
    )));
    rows
}

/// Update-banner breathing window: 270 frames = 9s (3 breaths; each cycle is 90 frames = 3.0s @30fps).
/// After the window it rests at the rest color and the banner stays (update-banner spec §2.3).
pub const UPDATE_BANNER_FRAMES: u64 = 270;
/// Breathing cycle in frames (one "in + out" every 3.0s at 30fps).
pub const UPDATE_BANNER_PERIOD: u64 = 90;

/// Banner truncation chain (update-banner spec §1.3, pure and testable): returns
/// (pre, ver, mid, cmd) — the static segment and the two breathing segments are separate so the render layer can color them.
///
/// | inner width | shown as |
/// |---|---|
/// | ≥50 (or the full line fits) | `   New version vX.Y.Z available — run bingo update` |
/// | ≥43 | `   New version vX.Y.Z — run bingo update` |
/// | ≥15 | `   bingo update` (the command alone, the minimal action entry) |
/// | <15 | None (banner hidden) |
///
/// At every tier `bingo update` stays visible, unwrapped, and inside the card.
pub fn banner_segments(v: &str, width: usize) -> Option<(String, String, String, String)> {
    const PRE: &str = "   New version ";
    const MID_FULL: &str = " available — run ";
    const MID_SHORT: &str = " — run ";
    const CMD: &str = "bingo update";
    let ver = format!("v{v}");
    let full_len = text_width(PRE) + text_width(&ver) + text_width(MID_FULL) + text_width(CMD);
    if width >= 50 || full_len <= width {
        return Some((PRE.to_string(), ver, MID_FULL.to_string(), CMD.to_string()));
    }
    if width >= 43 {
        return Some((PRE.to_string(), ver, MID_SHORT.to_string(), CMD.to_string()));
    }
    if width >= 15 {
        return Some((
            String::new(),
            String::new(),
            "   ".to_string(),
            CMD.to_string(),
        ));
    }
    None
}

/// The banner's full text (the string form of `banner_segments`; the pure function the spec names, used by test assertions).
#[cfg_attr(not(test), allow(dead_code))]
pub fn banner_line(v: &str, width: usize) -> Option<String> {
    banner_segments(v, width).map(|(pre, ver, mid, cmd)| format!("{pre}{ver}{mid}{cmd}"))
}

/// Update-banner breathing colors (update-banner spec §2, pure and testable):
/// - `motion_off` (settings `motion:"off"` or `BINGO_NO_MOTION`) → always rest (static, banner kept);
/// - no truecolor (theme downgraded to 256 colors) → discrete two-step: 60-frame cycle, peak for the first 12 frames (≥400ms);
/// - truecolor → sine breathing: `t = 0.5 − 0.5·cos(2π·phase/90)`, frame 0 = rest (trough), 45 = peak,
///   90 = back to rest; linear interpolation per sRGB channel.
///
/// Stops: dark `#D77757 ↔ #E8896B` (≥6.24:1 throughout); light `#B05227 ↔ #9A4A24` (≥4.72:1 throughout).
pub fn update_color(theme: &Theme, frame: u64, motion_off: bool) -> Color {
    let rest = if theme.is_dark {
        theme.claude
    } else {
        theme.claude_deep
    };
    if motion_off {
        return rest;
    }
    let peak = if theme.is_dark {
        theme.claude_strong
    } else {
        theme.claude_deep_strong
    };
    if !matches!(theme.claude_strong, Color::Rgb(..)) {
        // Discrete two-step (256-color terminal): 60-frame cycle, peak 400ms (12 frames) → rest 1600ms.
        return if frame % 60 < 12 { peak } else { rest };
    }
    let phase = (frame % UPDATE_BANNER_PERIOD) as f64;
    let t = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * phase / UPDATE_BANNER_PERIOD as f64).cos();
    lerp_color(rest, peak, t)
}

/// Per-channel sRGB linear interpolation (the two stops are close; no gamma correction — the spec notes it is out of scope).
fn lerp_color(a: Color, b: Color, t: f64) -> Color {
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) else {
        return b;
    };
    let l = |x: u8, y: u8| {
        (x as f64 + (y as f64 - x as f64) * t)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Color::Rgb(l(ar, br), l(ag, bg), l(ab, bb))
}

/// Whether this activity is the foreground shell command the live tail belongs
/// to. Bash is never concurrency-safe, so Phase 2 runs it alone: at most one
/// activity can answer yes at a time, which is why one tail slot is enough.
fn is_running_bash(act: &Activity) -> bool {
    matches!(&act.kind, ActivityKind::Tool(t) if t.status == ToolStatus::Running && t.name == "Bash")
}

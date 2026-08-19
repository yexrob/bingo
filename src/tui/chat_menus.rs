//! The provider and thinking pickers (split out of chat_tail.rs, D90): the
//! `/provider` and `/think` menus, their key handlers, and the sign-in /
//! sign-out flows behind them. Owns no state; `impl super::Chat`.

use super::*;
use crossterm::event::{KeyCode, KeyModifiers};

use crate::ui::UiEvent;

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
        if self.conv.busy {
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
                let used = self.main_conv().context_usage.used;
                self.main_conv().context_usage = crate::context_usage::ContextUsage::for_model(
                    used,
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
    pub(super) fn provider_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
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
    ///
    /// The two flags stay here and never become fields on the action (B5 ruling
    /// ③): they are login mechanics, and a pasted token is a credential. It
    /// travels from this input surface into [`crate::engine::actions`] as a
    /// function argument on this thread — nothing serialises it, so there is no
    /// frame it could leak into.
    pub(crate) fn slash_provider_login(&mut self, arg: &str) {
        use crate::engine::actions::{Flow, Waiting};
        let parts: Vec<&str> = arg.split_whitespace().collect();
        let Some(name) = parts.first() else {
            self.push_slash_output(
                "usage: /provider login <name> [--device-auth|--manual <token>]".to_string(),
            );
            return;
        };
        let flow = match parts
            .iter()
            .position(|part| *part == "--manual")
            .and_then(|at| parts.get(at + 1).copied())
        {
            Some(token) => Flow::Manual(token.to_string()),
            None if parts.contains(&"--device-auth") => Flow::DeviceAuth,
            None => Flow::Loopback,
        };
        let planned = match crate::engine::actions::plan_login(&self.session, name, flow) {
            Ok(planned) => planned,
            Err(said) => return self.say(said),
        };
        let session = self.session.clone();
        let events = self.events.clone();
        let pin = self.events.clone();
        // Pinned rather than printed: the URL and the one-time code have to stay
        // on screen for as long as the user needs to act on them (the 2s TTL
        // burned the code before anyone could type it).
        let waiting: Waiting = std::sync::Arc::new(move |lines| {
            pin.send(UiEvent::PinPanel {
                id: "login".to_string(),
                lines,
            });
        });
        tokio::spawn(async move {
            let said = crate::engine::actions::login_provider(&session, planned, waiting).await;
            events.send(UiEvent::Unpin {
                id: "login".to_string(),
            });
            events.send(crate::tui::chat::said_event(said));
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
                        events.send(UiEvent::SlashOutput(format!(
                            "✓ signed out of {name} (key cleared)"
                        )));
                    }
                    Err(e) => {
                        events.send(UiEvent::SlashError(format!("✗ sign-out failed: {e}")));
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
                    events.send(UiEvent::SlashOutput(format!(
                        "✓ signed out of {name} (credentials cleared)"
                    )));
                }
                Err(e) => {
                    events.send(UiEvent::SlashError(format!("✗ sign-out failed: {e}")));
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
    pub(crate) fn open_think_menu(&mut self) {
        let current = self.session.runtime.thinking.borrow().clone();
        let current = current.as_deref().unwrap_or("off");
        let levels = crate::tui::chat::think_levels();
        let current = levels
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
    pub(super) fn think_menu_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
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
}

//! What this session is set up as, reported and changed.
//!
//! `/status`, `/config`, `/context`, `/permissions`, `/mcp` and `/provider` are
//! one reading: they answer, and let the user move, the same handful of facts —
//! which endpoint and model, which thinking level, which permission rules, which
//! servers. On the wire that is `config/read` and the `config/changed` notice;
//! here it is a slash command and a printed block, and keeping them together is
//! what makes the two comparable.
//!
//! Lifted out of `chat.rs` whole (D149), method bodies unchanged.

use crate::ui::UiEvent;

/// What `/mcp` was asked for, as the action registry read it.
///
/// Three actions and one read reach one handler; this is the shape they arrive
/// in, so the handler never sees the line they came from.
pub(super) enum McpRequest {
    /// Every configured server and where it stands.
    List,
    /// `all` names every configured server; anything else names one.
    SetEnabled { target: String, enabled: bool },
    /// Absent is a usage error here rather than "every server": reconnecting is
    /// a per-server operation in this front end, and the console says so.
    Reconnect { server: Option<String> },
}

impl super::Chat {
    pub(super) fn slash_status(&mut self) {
        let session = self.session.clone();
        let model = self.model();
        let provider = self.provider();
        let thinking = self.thinking();
        let thinking_shown = thinking.unwrap_or_else(|| "off".to_string());
        let transcript = session.runtime.transcript.borrow().clone();
        let transcript_name = transcript
            .as_ref()
            .map(|t| t.name())
            .unwrap_or_else(|| "none".to_string());
        // The mode in effect, not the one the session started in: shift+tab
        // moves it and the core is where it moved to (D154).
        let mode = self.permission_mode_label().to_string();
        let models = session.client.models();
        self.slash_stats_async(move |msg_count, tokens| {
            // Window/percentage measured with the model actually in use — the
            // fixed 200k constant misread every non-Claude endpoint.
            let window = crate::budget::context_window_for(&models, &model).max(1);
            format!(
                "Model: {model}\nProvider: {provider}\nThinking: {thinking_shown}\nPermission mode: {mode}\nSession: {transcript_name}\nMessages: {msg_count}\nContext: {tokens} tokens / {window} ({}%)",
                tokens * 100 / window
            )
        });
    }

    /// `/config`: the interpreter the five config sources never had — for
    /// every effective value, WHICH layer (or env var) won; plus endpoint,
    /// credentials location and unknown-key warnings.
    pub(super) fn slash_config(&mut self) {
        let cwd = std::path::PathBuf::from(&self.cwd);
        let paths = crate::settings::layer_paths(&self.session.user_config_dir, &cwd);
        let layer_names = ["user", "project", "local"];
        let mut lines =
            vec!["config sources (user < project < local; later layers override):".to_string()];
        let mut layer_values: Vec<Option<serde_json::Value>> = Vec::new();
        for (path, name) in paths.iter().zip(layer_names) {
            let value = std::fs::read_to_string(path)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok());
            let state = if value.is_some() {
                "✓"
            } else if path.exists() {
                "✗ parse failed"
            } else {
                "(does not exist)"
            };
            lines.push(format!("  {name:8} {} {state}", path.display()));
            layer_values.push(value);
        }
        let lookup = |key: &str| -> Option<(String, &'static str)> {
            for (i, value) in layer_values.iter().enumerate().rev() {
                if let Some(v) = value.as_ref().and_then(|v| v.get(key)) {
                    let shown = match v {
                        serde_json::Value::String(s) if key == "apiKey" => {
                            let mut masked: String = s.chars().take(4).collect();
                            masked.push('…');
                            masked
                        }
                        serde_json::Value::String(s) => s.clone(),
                        other => other.to_string(),
                    };
                    return Some((shown, layer_names[i]));
                }
            }
            None
        };
        lines.push("effective values and their sources:".to_string());
        for key in [
            "provider",
            "model",
            "thinkingLevel",
            "theme",
            "permissionMode",
            "apiKey",
            "apiBaseUrl",
            "shell",
            "motion",
            "notifications",
        ] {
            let entry = match lookup(key) {
                Some((value, source)) => format!("  {key:18} = {value} ({source} layer)"),
                None => match key {
                    "apiKey" if std::env::var("ANTHROPIC_API_KEY").is_ok() => {
                        format!("  {key:18} = (env ANTHROPIC_API_KEY)")
                    }
                    "apiKey" if std::env::var("DEEPSEEK_API_KEY").is_ok() => {
                        format!("  {key:18} = (env DEEPSEEK_API_KEY)")
                    }
                    "apiBaseUrl" if std::env::var("ANTHROPIC_BASE_URL").is_ok() => {
                        format!("  {key:18} = (env ANTHROPIC_BASE_URL)")
                    }
                    _ => format!("  {key:18} = (default)"),
                },
            };
            lines.push(entry);
        }
        // Runtime identity: what this session is actually talking to.
        let provider = self.provider();
        let model = self.model();
        let (_, url) = self.session.client.current_endpoint();
        lines.push(format!(
            "current session: {provider} · {model} · {url}{}",
            if self.provider_session_only {
                " (provider is session-scoped)"
            } else {
                ""
            }
        ));
        lines.push(format!(
            "credential store: {} (/provider shows each endpoint's login state)",
            crate::auth::AuthStore::new(&self.session.home)
                .path()
                .display()
        ));
        // Unknown top-level keys: typos parse fine and silently do nothing.
        for (i, value) in layer_values.iter().enumerate() {
            if let Some(obj) = value.as_ref().and_then(|v| v.as_object()) {
                for key in obj.keys() {
                    if !crate::settings::KNOWN_KEYS.contains(&key.as_str()) {
                        lines.push(format!(
                            "⚠ unknown config key \"{key}\" in the {} layer (a typo? it will have no effect)",
                            layer_names[i]
                        ));
                    }
                }
            }
        }
        self.push_slash_info(lines.join("\n"));
    }

    pub(super) fn slash_context(&mut self) {
        let model = self.model();
        let models = self.session.client.models();
        self.slash_stats_async(move |_msg_count, tokens| {
            let window = crate::budget::context_window_for(&models, &model).max(1);
            let pct = tokens * 100 / window;
            let bar_len = 40usize;
            let filled = ((pct as usize * bar_len) / 100).min(bar_len);
            let bar = format!("{}·{}", "#".repeat(filled), "·".repeat(bar_len - filled));
            format!(
                "context: [{bar}] {pct}%\n{tokens} / {window} tokens used\nauto-compaction threshold: {}%",
                crate::budget::autocompact_threshold_for(&models, &model) * 100 / window
            )
        });
    }

    /// The permission rules as they stand — the reading half of
    /// `config/read`'s permissions section, rendered here.
    pub(super) fn slash_permissions(&mut self) {
        let rules = self
            .session
            .runtime
            .permissions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let lines = permission_lines(&rules, &self.session_grants());
        self.push_slash_info(lines.join("\n"));
    }

    /// Add or drop one permission rule. The registry read the line; this applies
    /// it to the live table and writes it back to the project layer.
    pub(crate) fn permission_rule(
        &mut self,
        decision: crate::app::snapshot::PermissionRuleDecision,
        rule: &str,
        add: bool,
    ) {
        use crate::app::snapshot::PermissionRuleDecision;
        let kind = match decision {
            PermissionRuleDecision::Allow => "allow",
            PermissionRuleDecision::Deny => "deny",
            PermissionRuleDecision::Ask => "ask",
        };
        let mut rules = self
            .session
            .runtime
            .permissions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let list = match decision {
            PermissionRuleDecision::Allow => &mut rules.allow,
            PermissionRuleDecision::Deny => &mut rules.deny,
            PermissionRuleDecision::Ask => &mut rules.ask,
        };
        let held = list.iter().any(|held| held == rule);
        match (add, held) {
            (true, false) => list.push(rule.to_string()),
            (false, true) => list.retain(|held| held != rule),
            (true, true) => {}
            (false, false) => {
                self.push_slash_error(format!(
                    "[error] code={} msg=no {kind} rule to remove: {rule}",
                    crate::error::SLASH_ERROR_BAD_ARGUMENT
                ));
                return;
            }
        }
        *self
            .session
            .runtime
            .permissions
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = rules.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        let patch = serde_json::json!({
            "permissions": {
                "allow": rules.allow,
                "deny": rules.deny,
                "ask": rules.ask,
            }
        });
        let verb = if add { "added" } else { "removed" };
        match crate::settings::upsert_project_settings(&cwd, &patch) {
            Ok(()) => self.push_slash_output(format!(
                "✓ {verb} {kind} rule: {rule} (active now + written to .bingo/settings.json)"
            )),
            Err(e) => self.push_slash_output(format!(
                "✓ {verb} {kind} rule: {rule} (active now); persistence failed: {e}"
            )),
        }
    }

    /// `/mcp`: what is configured, what is on, and what to reconnect.
    ///
    /// What was asked for arrives already read ([`McpRequest`]): the registry
    /// decides which handler runs, and it used to decide that by parsing a line
    /// this function then parsed again.
    pub(super) fn slash_mcp(&mut self, request: McpRequest) {
        use crate::mcp::McpStatus;
        let session = self.session.clone();
        let cwd = std::path::PathBuf::from(&self.cwd);
        let user_config_dir = self.session.user_config_dir.clone();
        let events = self.events.clone();
        match request {
            McpRequest::List => {
                self.pin_panel("mcp", vec!["⏳ checking MCP servers…".to_string()]);
                tokio::spawn(async move {
                    let unpin = || {
                        events.send(UiEvent::Unpin {
                            id: "mcp".to_string(),
                        });
                    };
                    let mgr = session.runtime.mcp.lock().await;
                    let names = mgr.configured();
                    if names.is_empty() {
                        unpin();
                        events.send(UiEvent::SlashInfo(
                            "no MCP servers configured.\nAdd them under mcpServers in .bingo/settings.json or \
                             ~/.config/bingo/settings.json."
                                .to_string(),
                        ));
                        return;
                    }
                    let mut lines = vec![format!("MCP servers ({}):", names.len())];
                    for name in names {
                        let line = match mgr.status(&name) {
                            McpStatus::Connected { tool_count } => {
                                format!("  ✓ {name}  connected · {tool_count} tools")
                            }
                            McpStatus::Failed { detail } => {
                                format!("  ✗ {name}  failed: {detail}")
                            }
                            McpStatus::Disabled => format!("  ○ {name}  disabled"),
                            McpStatus::NotConnected => format!("  · {name}  not connected"),
                        };
                        lines.push(line);
                    }
                    lines.push(
                        "usage: /mcp enable|disable [name|all] · /mcp reconnect [name]".into(),
                    );
                    unpin();
                    events.send(UiEvent::SlashInfo(lines.join("\n")));
                });
            }
            McpRequest::SetEnabled { target, enabled } => {
                self.push_slash_output(format!(
                    "⏳ {}{target}…",
                    if enabled { "enabling " } else { "disabling " }
                ));
                tokio::spawn(async move {
                    let mut mgr = session.runtime.mcp.lock().await;
                    let targets: Vec<String> = if target == "all" {
                        mgr.configured()
                    } else if mgr.configured().contains(&target.to_string()) {
                        vec![target.to_string()]
                    } else {
                        Vec::new()
                    };
                    if targets.is_empty() {
                        events.send(UiEvent::SlashError(format!("no MCP server \"{target}\".")));
                        return;
                    }
                    for name in &targets {
                        mgr.set_enabled(name, enabled);
                    }
                    if enabled {
                        // Union-merged key: the name must leave EVERY layer
                        // that lists it — writing only the project layer let
                        // a user-layer entry merge it right back next start.
                        for name in &targets {
                            let _ = crate::settings::remove_from_union_lists(
                                &user_config_dir,
                                &cwd,
                                "disabledMcpServers",
                                name,
                            );
                        }
                    } else {
                        let list = mgr.disabled();
                        let _ = crate::settings::upsert_project_settings(
                            &cwd,
                            &serde_json::json!({ "disabledMcpServers": list }),
                        );
                    }
                    let verb = if enabled { "enabled" } else { "disabled" };
                    events.send(UiEvent::SlashOutput(format!(
                        "{verb} {} MCP server(s): {}",
                        targets.len(),
                        targets.join(", ")
                    )));
                });
            }
            McpRequest::Reconnect { server } => {
                // No name reconnects every enabled server (D157) — the action
                // always could, and the console refusing what the wire allows
                // was the parity ledger's one admitted asymmetry.
                let label = server.as_deref().unwrap_or("all MCP servers");
                self.pin_panel("mcp", vec![format!("⏳ reconnecting {label}…")]);
                tokio::spawn(async move {
                    let said =
                        crate::engine::actions::reconnect_mcp(&session, server.as_deref()).await;
                    events.send(UiEvent::Unpin {
                        id: "mcp".to_string(),
                    });
                    events.send(crate::tui::chat::said_event(said));
                });
            }
        }
    }

    /// `/provider [name]`: no argument opens the selector (picker-model.md commit D); an argument takes the fast path.
    pub(super) fn slash_provider(&mut self, arg: &str) {
        if let Some(rest) = arg.strip_prefix("login ") {
            return self.slash_provider_login(rest.trim());
        }
        if let Some(rest) = arg.strip_prefix("logout ") {
            return self.slash_provider_logout(rest.trim());
        }
        if arg.is_empty() {
            self.open_provider_menu();
            return;
        }
        self.switch_provider(arg, true);
    }
}

/// `/permissions`, as a block of lines.
///
/// Two kinds of rule share one runtime table: the ones settings declares, and
/// the grants a "don't ask again this session" answer installed (D81). The
/// table is a flat list of strings and cannot tell them apart, so this block
/// used to print a session grant under a heading naming a file it had never
/// been written to. `granted` is the core's answer to which is which, and the
/// two lists are printed apart — a grant that vanishes when the process does
/// should not read as one somebody committed.
pub(super) fn permission_lines(
    rules: &crate::settings::PermissionRules,
    granted: &std::collections::BTreeSet<String>,
) -> Vec<String> {
    let mut lines = vec!["permission rules (.bingo/settings.json):".to_string()];
    let mut for_this_session = Vec::new();
    for (name, list) in [
        ("allow", &rules.allow),
        ("deny", &rules.deny),
        ("ask", &rules.ask),
    ] {
        let declared: Vec<&String> = list
            .iter()
            .filter(|rule| !granted.contains(*rule))
            .collect();
        if declared.is_empty() {
            lines.push(format!("  {name}: (none)"));
        } else {
            lines.push(format!("  {name}:"));
            for rule in declared {
                lines.push(format!("    {rule}"));
            }
        }
        for_this_session.extend(
            list.iter()
                .filter(|rule| granted.contains(*rule))
                .map(|rule| format!("    {rule}")),
        );
    }
    if !for_this_session.is_empty() {
        lines.push("allowed for this session only (written to no file):".to_string());
        lines.extend(for_this_session);
    }
    lines.push(
        "usage: /permissions [allow|deny|ask] <rule, e.g. Skill(review:*)] · remove <allow|deny|ask> <rule>"
            .into(),
    );
    lines
}

#[cfg(test)]
mod tests {
    use super::permission_lines;

    /// The heading names a file, so what is printed under it has to be in one.
    #[test]
    fn a_session_grant_is_not_printed_as_a_settings_rule() {
        let rules = crate::settings::PermissionRules {
            allow: vec!["Skill(review:*)".to_string(), "Bash(ls:*)".to_string()],
            deny: Vec::new(),
            ask: Vec::new(),
        };
        let granted = std::collections::BTreeSet::from(["Bash(ls:*)".to_string()]);
        let printed = permission_lines(&rules, &granted).join("\n");
        let settings = printed
            .split("allowed for this session only")
            .next()
            .unwrap_or_default();
        assert!(
            settings.contains("Skill(review:*)"),
            "a declared rule belongs under the settings heading: {printed}"
        );
        assert!(
            !settings.contains("Bash(ls:*)"),
            "a session grant was never written to settings: {printed}"
        );
        assert!(
            printed.contains("allowed for this session only (written to no file):\n    Bash(ls:*)"),
            "{printed}"
        );

        let none = permission_lines(&rules, &std::collections::BTreeSet::new()).join("\n");
        assert!(
            !none.contains("this session only"),
            "no grants, no second heading: {none}"
        );
    }
}

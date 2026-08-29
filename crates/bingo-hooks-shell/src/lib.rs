//! Shell hooks: a person's own commands, run at bingo's lifecycle points on
//! Claude Code's hook contract.
//!
//! # The dialect
//!
//! Verified against <https://code.claude.com/docs/en/hooks> and
//! <https://code.claude.com/docs/en/hooks-guide> on **2026-08-29**. A settings
//! `hooks` block is Claude Code's, an event arrives as JSON on stdin, a verdict
//! leaves as JSON on stdout, and the exit code decides: `0` is "here is what I
//! have to say", `2` blocks with the hook's own words as the reason, anything
//! else is a broken hook that never gets to decide.
//!
//! Where this plugin departs from the reference, and why:
//!
//! - **Timeouts are 60 s, 1.5 s for `SessionEnd`.** The reference now defaults a
//!   command hook to 600 s; `docs/plans/M7-hooks-skills-mcp.md` brick 4 fixes
//!   bingo's at 60 s, and a hook that holds a turn for ten minutes is a hang.
//!   A per-hook `timeout` overrides both, capped at 60 s for `SessionEnd`.
//! - **Ten events.** `PreToolUse`, `PostToolUse`, `PostToolUseFailure`,
//!   `PermissionRequest`, `UserPromptSubmit`, `Stop`, `PreCompact`,
//!   `SessionStart`, `SessionEnd`, `Notification`. The other twenty-odd events
//!   the reference lists have no bingo lifecycle point behind them yet, so an
//!   event name this plugin does not serve is a startup failure: a hook nobody
//!   will run is a rule its author believes is enforced.
//! - **`type: "command"` only.** `http`, `mcp_tool`, `prompt` and `agent` hooks
//!   are refused at startup rather than skipped in silence.
//! - **Both decision dialects are read.** The reference documents
//!   `hookSpecificOutput.permissionDecision`; the older top-level
//!   `decision`/`reason` pair is still what most scripts print, so both are
//!   accepted and `hookSpecificOutput` wins where they disagree.
//! - **`decision: "block"` ends a turn from `PostToolUse`.** The reference says
//!   a post-tool hook cannot block; brick 4 maps it onto `HookOutcome::Block`,
//!   which ends the turn after this round rather than undoing the call.
//! - **`permissionDecision: "allow"` does not skip the gate.** bingo has one
//!   permission path, the policy, and a hook is not it; `allow` reads as "no
//!   objection" and the call still goes to the gate.
//! - **Fields bingo cannot supply are omitted, never faked:** `transcript_path`
//!   (the journal is not a Claude Code transcript, and the hook context carries
//!   no path), `permission_mode` (it lives in the permissions plugin, and a
//!   plugin may not read another's state), `last_assistant_message` and
//!   `stop_hook_active` on `Stop`. `PreCompact` always reports `trigger: "auto"`,
//!   `SessionStart` always `source: "startup"` and `SessionEnd` always
//!   `end_reason: "other"`: `on_compact` and `on_session` carry a phase and
//!   nothing else, so any other value would be a guess.
//! - **Plain text on stdout is not context.** The reference feeds a
//!   `UserPromptSubmit` hook's plain stdout to the model; here only JSON is read,
//!   and `hookSpecificOutput.additionalContext` is the way to add to a prompt.
//! - **`BINGO_ENV_FILE`** is `CLAUDE_ENV_FILE`: a `SessionStart` hook appends
//!   `KEY=value` (or `export KEY=value`) lines to the path in it, and every later
//!   hook in that session runs with them. The file is read as assignments, not
//!   sourced as shell.

pub mod config;
pub mod dispatch;
pub mod env_file;
pub mod events;
pub mod hook;
pub mod matcher;
pub mod program;
pub mod run;
pub mod session;
pub mod verdict;

use std::sync::Arc;

use bingo_sdk::{ConfigClaim, Contribution, Hook, Plugin, PluginError, PluginManifest, Registrar};

pub use config::{HookEvent, Hooks, Settings};
pub use hook::ShellHooks;

static MANIFEST: PluginManifest = PluginManifest {
    id: "bingo.hooks.shell",
    version: env!("CARGO_PKG_VERSION"),
    sdk: "^0.1",
    provides: &["hook:shell"],
    requires: &[],
    config: Some(ConfigClaim {
        keys: config::CLAIMED,
        schema: config::schema,
    }),
};

/// Registers the one hook that runs a person's shell commands.
#[derive(Debug, Default, Clone, Copy)]
pub struct ShellHooksPlugin;

#[async_trait::async_trait]
impl Plugin for ShellHooksPlugin {
    fn manifest(&self) -> &'static PluginManifest {
        &MANIFEST
    }

    fn register(&self, registrar: &mut Registrar) -> Result<(), PluginError> {
        let settings: Settings = registrar.config()?;
        let hooks = Arc::new(ShellHooks::new(&settings.hooks, &registrar.env().data_dir));
        registrar.add(Contribution::Hook(hooks as Arc<dyn Hook>));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bingo_sdk::{Env, Merge};
    use serde_json::json;

    fn registrar(config: serde_json::Value) -> Registrar {
        Registrar::new(MANIFEST.id, config, Env::rooted("/tmp/bingo-hooks-test"))
    }

    #[test]
    fn the_plugin_registers_one_hook() {
        let mut registrar = registrar(json!({
            "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "true"}]}]}
        }));
        ShellHooksPlugin
            .register(&mut registrar)
            .expect("registers");
        let contributions = registrar.into_contributions();
        assert_eq!(contributions.len(), 1);
        assert!(matches!(contributions[0], Contribution::Hook(_)));
    }

    #[test]
    fn a_process_with_no_hooks_configured_still_registers() {
        let mut registrar = registrar(json!({}));
        ShellHooksPlugin
            .register(&mut registrar)
            .expect("registers");
        assert_eq!(registrar.into_contributions().len(), 1);
    }

    #[test]
    fn a_settings_mistake_stops_the_boot_instead_of_being_ignored() {
        let mut registrar = registrar(json!({"hooks": {"Stop": "not a list"}}));
        let error = ShellHooksPlugin
            .register(&mut registrar)
            .expect_err("refused");
        assert!(matches!(error, PluginError::Config(_)), "{error:?}");
    }

    #[test]
    fn the_manifest_claims_only_the_hooks_key() {
        let claim = MANIFEST.config.expect("the plugin claims settings");
        assert!(claim.keys.iter().all(|(key, _)| key.starts_with("hooks.")));
        // Accumulate, so a project's hooks add to the user's (ADR-0003 §2).
        assert!(claim.keys.iter().all(|(_, m)| *m == Merge::Accumulate));
    }
}

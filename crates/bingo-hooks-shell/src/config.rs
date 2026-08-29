//! The `hooks` settings key: event name → rules → commands.
//!
//! The shape is Claude Code's, so a `.claude/settings.json` `hooks` block can be
//! pasted verbatim. One list of event names is the single fact here: it types the
//! map's keys, names the events on the wire, and is what the plugin claims from
//! the settings loader.

use std::collections::BTreeMap;

use bingo_sdk::Merge;
use schemars::JsonSchema;
use serde::Deserialize;

/// The events this plugin serves. Every one is claimed with [`Merge::Accumulate`],
/// so a project's list adds to the user's instead of hiding it: with the default
/// `Replace`, `.bingo/settings.json` naming `PreToolUse` would silently drop every
/// `PreToolUse` hook the user configured in their home directory (ADR-0003 §2 —
/// `ByName` is not an option, since a rule has neither a `name` nor an `id`).
macro_rules! events {
    ($($variant:ident),* $(,)?) => {
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, JsonSchema,
        )]
        pub enum HookEvent { $($variant),* }

        impl HookEvent {
            /// The name in `settings.json` and in `hook_event_name` on stdin.
            pub const fn name(self) -> &'static str {
                match self { $(Self::$variant => stringify!($variant)),* }
            }
        }

        /// The dotted key paths this plugin claims, with how each merges.
        pub const CLAIMED: &[(&str, Merge)] = &[
            $((concat!("hooks.", stringify!($variant)), Merge::Accumulate)),*
        ];
    };
}

events! {
    PreToolUse,
    PostToolUse,
    PostToolUseFailure,
    PermissionRequest,
    UserPromptSubmit,
    Stop,
    PreCompact,
    SessionStart,
    SessionEnd,
    Notification,
}

/// The claimed slice, as the kernel hands it over.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
pub struct Settings {
    #[serde(default)]
    pub hooks: Hooks,
}

/// Event → the rules configured for it, in layer order.
///
/// An event name this plugin does not serve is a startup failure rather than a
/// silence: a hook nobody will ever run is a rule the person believes is enforced.
#[derive(Clone, Debug, Default, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Hooks(pub BTreeMap<HookEvent, Vec<HookRule>>);

impl Hooks {
    pub fn rules(&self, event: HookEvent) -> &[HookRule] {
        self.0.get(&event).map_or(&[], Vec::as_slice)
    }
}

/// One matcher and the commands it selects.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookRule {
    /// Whole-string-anchored regex over the event's subject. Absent matches every
    /// subject, as an empty one does.
    #[serde(default)]
    pub matcher: Option<String>,
    #[serde(default)]
    pub hooks: Vec<HookEntry>,
}

/// One command to run. A field this plugin does not understand is a startup
/// failure, so an `http` or `mcp_tool` hook is refused loudly instead of skipped.
#[derive(Clone, Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HookEntry {
    #[serde(rename = "type")]
    pub kind: HookKind,
    pub command: String,
    /// Seconds. Absent takes the event's default (60 s, 1.5 s for `SessionEnd`).
    #[serde(default)]
    pub timeout: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum HookKind {
    Command,
}

pub fn schema() -> schemars::Schema {
    schemars::schema_for!(Settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn settings(value: serde_json::Value) -> Result<Settings, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn a_claude_code_hooks_block_parses_as_written() {
        let parsed = settings(json!({
            "hooks": {
                "PreToolUse": [{
                    "matcher": "Edit|Write",
                    "hooks": [{"type": "command", "command": "./check.sh", "timeout": 5}]
                }]
            }
        }))
        .expect("the block parses");
        let rules = parsed.hooks.rules(HookEvent::PreToolUse);
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].matcher.as_deref(), Some("Edit|Write"));
        assert_eq!(rules[0].hooks[0].command, "./check.sh");
        assert_eq!(rules[0].hooks[0].timeout, Some(5));
    }

    #[test]
    fn a_rule_without_a_matcher_is_a_rule() {
        let parsed = settings(json!({
            "hooks": {"Stop": [{"hooks": [{"type": "command", "command": "true"}]}]}
        }))
        .expect("the block parses");
        assert!(parsed.hooks.rules(HookEvent::Stop)[0].matcher.is_none());
    }

    #[test]
    fn an_event_this_plugin_does_not_serve_is_refused() {
        let error = settings(json!({"hooks": {"PostToolBatch": []}})).expect_err("refused");
        assert!(error.to_string().contains("PostToolBatch"), "{error}");
    }

    #[test]
    fn a_hook_type_this_plugin_cannot_run_is_refused() {
        let error = settings(json!({
            "hooks": {"Stop": [{"hooks": [{"type": "http", "url": "http://x"}]}]}
        }))
        .expect_err("refused");
        assert!(error.to_string().contains("http"), "{error}");
    }

    #[test]
    fn a_misspelt_field_is_refused() {
        let error = settings(json!({
            "hooks": {"Stop": [{"mather": "x", "hooks": []}]}
        }))
        .expect_err("refused");
        assert!(error.to_string().contains("mather"), "{error}");
    }

    #[test]
    fn no_event_is_claimed_twice_and_every_one_is_claimed() {
        let mut keys: Vec<&str> = CLAIMED.iter().map(|(k, _)| *k).collect();
        let before = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), before, "a key is claimed twice");
        assert!(CLAIMED.iter().all(|(_, m)| *m == Merge::Accumulate));
        assert!(keys.contains(&"hooks.PreToolUse"));
    }

    #[test]
    fn the_schema_is_generated_from_the_claimed_types() {
        let schema = serde_json::to_value(schema()).expect("the schema serialises");
        assert!(
            schema.to_string().contains("PreToolUse"),
            "the events are missing from the schema"
        );
    }
}

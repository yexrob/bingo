//! The configuration, compiled: every matcher becomes a regex once, at
//! registration, so selecting a hook for an event is a scan of the rules that
//! were written for it and nothing else.

use std::collections::BTreeMap;

use crate::config::{HookEntry, HookEvent, HookRule, Hooks};
use crate::matcher::Matcher;

#[derive(Debug)]
pub struct Program(BTreeMap<HookEvent, Vec<Rule>>);

#[derive(Debug)]
struct Rule {
    matcher: Matcher,
    entries: Vec<HookEntry>,
}

impl Program {
    pub fn compile(hooks: &Hooks) -> Self {
        let events = [
            HookEvent::PreToolUse,
            HookEvent::PostToolUse,
            HookEvent::PostToolUseFailure,
            HookEvent::PermissionRequest,
            HookEvent::UserPromptSubmit,
            HookEvent::Stop,
            HookEvent::PreCompact,
            HookEvent::SessionStart,
            HookEvent::SessionEnd,
            HookEvent::Notification,
        ];
        Self(
            events
                .into_iter()
                .map(|event| (event, rules(hooks.rules(event))))
                .filter(|(_, rules)| !rules.is_empty())
                .collect(),
        )
    }

    /// The commands configured for this event whose matcher accepts the subject,
    /// in the order they were written. Empty is the common answer and costs one
    /// map lookup.
    pub fn select(&self, event: HookEvent, subject: &str) -> Vec<&HookEntry> {
        let Some(rules) = self.0.get(&event) else {
            return Vec::new();
        };
        rules
            .iter()
            .filter(|rule| rule.matcher.matches(subject))
            .flat_map(|rule| rule.entries.iter())
            .collect()
    }
}

fn rules(configured: &[HookRule]) -> Vec<Rule> {
    configured
        .iter()
        .filter(|rule| !rule.hooks.is_empty())
        .map(|rule| Rule {
            matcher: Matcher::compile(rule.matcher.as_deref()),
            entries: rule.hooks.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn program(value: serde_json::Value) -> Program {
        let hooks: Hooks = serde_json::from_value(value).expect("the hooks parse");
        Program::compile(&hooks)
    }

    fn commands(program: &Program, event: HookEvent, subject: &str) -> Vec<String> {
        program
            .select(event, subject)
            .into_iter()
            .map(|entry| entry.command.clone())
            .collect()
    }

    #[test]
    fn the_rules_that_match_contribute_their_commands_in_order() {
        let program = program(json!({
            "PreToolUse": [
                {"matcher": "Bash", "hooks": [
                    {"type": "command", "command": "one"},
                    {"type": "command", "command": "two"}
                ]},
                {"matcher": "Edit", "hooks": [{"type": "command", "command": "three"}]},
                {"hooks": [{"type": "command", "command": "four"}]}
            ]
        }));
        assert_eq!(
            commands(&program, HookEvent::PreToolUse, "Bash"),
            ["one", "two", "four"]
        );
        assert_eq!(
            commands(&program, HookEvent::PreToolUse, "Edit"),
            ["three", "four"]
        );
        assert_eq!(commands(&program, HookEvent::PreToolUse, "Read"), ["four"]);
    }

    #[test]
    fn an_event_nobody_configured_selects_nothing() {
        let program = program(json!({
            "Stop": [{"hooks": [{"type": "command", "command": "one"}]}]
        }));
        assert!(program.select(HookEvent::PreToolUse, "Bash").is_empty());
        assert_eq!(commands(&program, HookEvent::Stop, ""), ["one"]);
    }

    #[test]
    fn a_rule_with_no_commands_is_not_a_rule() {
        let program = program(json!({"Stop": [{"matcher": "", "hooks": []}]}));
        assert!(program.select(HookEvent::Stop, "").is_empty());
    }
}

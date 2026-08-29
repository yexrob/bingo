//! Rules a person accepted for one session, and for no longer than that.
//!
//! "Allow for the session" is an answer, not a setting: it lives in memory,
//! keyed by the session that gave it, and reaches no file. A sub-agent is a
//! session of its own, so an approval given in one never silences a prompt in
//! another.

use std::collections::HashMap;
use std::sync::Mutex;

use bingo_sdk::SessionId;

use crate::rule::Rule;

#[derive(Debug, Default)]
pub struct SessionRules {
    by_session: Mutex<HashMap<SessionId, Vec<Rule>>>,
}

impl SessionRules {
    /// Install what the person accepted. `false` for a line this grammar
    /// cannot read, or for a lock a panic already broke: neither installs a
    /// guess, and both only mean more prompts.
    pub fn install(&self, session: &SessionId, raw: &str) -> bool {
        let Some(rule) = Rule::parse(raw) else {
            return false;
        };
        let Ok(mut by_session) = self.by_session.lock() else {
            return false;
        };
        let rules = by_session.entry(session.clone()).or_default();
        if !rules.contains(&rule) {
            rules.push(rule);
        }
        true
    }

    pub fn of(&self, session: &SessionId) -> Vec<Rule> {
        self.by_session
            .lock()
            .ok()
            .and_then(|by_session| by_session.get(session).cloned())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> SessionId {
        SessionId::from_raw(id)
    }

    #[test]
    fn an_accepted_rule_is_there_for_the_next_call() {
        let rules = SessionRules::default();
        assert!(rules.install(&session("ses_a"), "Bash(cargo:*)"));
        assert_eq!(
            rules
                .of(&session("ses_a"))
                .iter()
                .map(Rule::raw)
                .collect::<Vec<_>>(),
            ["Bash(cargo:*)"]
        );
    }

    #[test]
    fn one_sessions_answer_does_not_speak_for_another() {
        let rules = SessionRules::default();
        rules.install(&session("ses_a"), "Bash(cargo:*)");
        assert!(rules.of(&session("ses_b")).is_empty());
    }

    #[test]
    fn the_same_answer_twice_is_still_one_rule() {
        let rules = SessionRules::default();
        rules.install(&session("ses_a"), "Bash(cargo:*)");
        rules.install(&session("ses_a"), "Bash(cargo:*)");
        assert_eq!(rules.of(&session("ses_a")).len(), 1);
    }

    #[test]
    fn a_line_the_grammar_cannot_read_installs_nothing() {
        let rules = SessionRules::default();
        assert!(!rules.install(&session("ses_a"), "Bash(unclosed"));
        assert!(rules.of(&session("ses_a")).is_empty());
    }
}

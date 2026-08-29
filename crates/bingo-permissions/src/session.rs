//! What a person decided for one session, and for no longer than that.
//!
//! "Allow for the session" and `/permission <mode>` are answers, not settings:
//! they live in memory, keyed by the session that gave them, and reach no file.
//! A sub-agent is a session of its own, so neither an approval nor a mode given
//! in one is ever heard in another.

use std::collections::HashMap;
use std::sync::Mutex;

use bingo_sdk::SessionId;

use crate::mode::Mode;
use crate::rule::Rule;

/// One session's answers: the rules its person accepted, and the mode they
/// chose over the configured one.
#[derive(Debug, Default)]
struct Decided {
    rules: Vec<Rule>,
    mode: Option<Mode>,
}

#[derive(Debug, Default)]
pub struct Sessions {
    by_session: Mutex<HashMap<SessionId, Decided>>,
}

impl Sessions {
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
        let rules = &mut by_session.entry(session.clone()).or_default().rules;
        if !rules.contains(&rule) {
            rules.push(rule);
        }
        true
    }

    pub fn rules(&self, session: &SessionId) -> Vec<Rule> {
        self.read(session, |decided| decided.rules.clone())
            .unwrap_or_default()
    }

    /// The mode this session was told to run in, if it was told one.
    pub fn mode(&self, session: &SessionId) -> Option<Mode> {
        self.read(session, |decided| decided.mode).flatten()
    }

    /// Run this session in `mode` from now on. A lock a panic already broke
    /// leaves the configured mode in place, which is the safe half.
    pub fn choose_mode(&self, session: &SessionId, mode: Mode) {
        if let Ok(mut by_session) = self.by_session.lock() {
            by_session.entry(session.clone()).or_default().mode = Some(mode);
        }
    }

    fn read<T>(&self, session: &SessionId, of: impl FnOnce(&Decided) -> T) -> Option<T> {
        self.by_session
            .lock()
            .ok()
            .and_then(|by_session| by_session.get(session).map(of))
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
        let sessions = Sessions::default();
        assert!(sessions.install(&session("ses_a"), "Bash(cargo:*)"));
        assert_eq!(
            sessions
                .rules(&session("ses_a"))
                .iter()
                .map(Rule::raw)
                .collect::<Vec<_>>(),
            ["Bash(cargo:*)"]
        );
    }

    #[test]
    fn one_sessions_answer_does_not_speak_for_another() {
        let sessions = Sessions::default();
        sessions.install(&session("ses_a"), "Bash(cargo:*)");
        sessions.choose_mode(&session("ses_a"), Mode::AcceptEdits);
        assert!(sessions.rules(&session("ses_b")).is_empty());
        assert_eq!(sessions.mode(&session("ses_b")), None);
    }

    #[test]
    fn the_same_answer_twice_is_still_one_rule() {
        let sessions = Sessions::default();
        sessions.install(&session("ses_a"), "Bash(cargo:*)");
        sessions.install(&session("ses_a"), "Bash(cargo:*)");
        assert_eq!(sessions.rules(&session("ses_a")).len(), 1);
    }

    #[test]
    fn a_line_the_grammar_cannot_read_installs_nothing() {
        let sessions = Sessions::default();
        assert!(!sessions.install(&session("ses_a"), "Bash(unclosed"));
        assert!(sessions.rules(&session("ses_a")).is_empty());
    }

    #[test]
    fn a_session_runs_in_the_last_mode_it_was_given() {
        let sessions = Sessions::default();
        assert_eq!(sessions.mode(&session("ses_a")), None);
        sessions.choose_mode(&session("ses_a"), Mode::Plan);
        sessions.choose_mode(&session("ses_a"), Mode::AcceptEdits);
        assert_eq!(sessions.mode(&session("ses_a")), Some(Mode::AcceptEdits));
    }

    #[test]
    fn a_mode_and_an_accepted_rule_live_side_by_side() {
        let sessions = Sessions::default();
        sessions.choose_mode(&session("ses_a"), Mode::Plan);
        sessions.install(&session("ses_a"), "Bash(cargo:*)");
        assert_eq!(sessions.mode(&session("ses_a")), Some(Mode::Plan));
        assert_eq!(sessions.rules(&session("ses_a")).len(), 1);
    }
}

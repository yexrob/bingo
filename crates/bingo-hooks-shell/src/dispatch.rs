//! Running the hooks one event selected: the deadline, the environment, and
//! reading an exit code as an answer.
//!
//! Exit codes are the reference's: `0` is "here is what I have to say", `2` is a
//! hard block whose reason is the hook's own words, and anything else is a broken
//! hook — logged, and never allowed to decide.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use bingo_sdk::{HookContext, SessionId};
use serde_json::Value;

use crate::config::{HookEntry, HookEvent, Hooks};
use crate::program::Program;
use crate::run::{self, Request};
use crate::session::Sessions;
use crate::verdict::Verdict;

/// What a hook gets unless it asked for something else.
const TIMEOUT: Duration = Duration::from_secs(60);
/// Teardown is not the place to wait on somebody's script.
const SESSION_END_TIMEOUT: Duration = Duration::from_millis(1500);
/// The longest a `SessionEnd` hook may ask for.
const SESSION_END_CEILING: Duration = Duration::from_secs(60);

/// What one hook said, once its exit code has been read.
#[derive(Debug)]
pub enum Said {
    /// Exit 2: blocked, for this reason.
    Blocked(String),
    /// Exit 0, and whatever it printed.
    Spoke(Verdict),
    /// It could not run, ran past its deadline, or exited non-zero: no effect.
    Nothing,
}

#[derive(Debug)]
pub struct Dispatch {
    program: Program,
    sessions: Sessions,
}

impl Dispatch {
    pub fn new(hooks: &Hooks, data_dir: &Path) -> Self {
        Self {
            program: Program::compile(hooks),
            sessions: Sessions::new(data_dir.join("hooks")),
        }
    }

    /// The hooks configured for this event whose matcher accepts the subject.
    pub fn select(&self, event: HookEvent, subject: &str) -> Vec<&HookEntry> {
        self.program.select(event, subject)
    }

    pub fn sessions(&self) -> &Sessions {
        &self.sessions
    }

    /// Run one hook and read what it said.
    pub async fn speak(
        &self,
        event: HookEvent,
        entry: &HookEntry,
        input: &Value,
        cx: &HookContext,
    ) -> Said {
        let request = Request {
            command: &entry.command,
            input,
            cwd: &cx.cwd,
            timeout: deadline(event, entry),
            env: self.env(event, &cx.session),
        };
        match run::run(request).await {
            Ok(completed) => read(event, entry, completed),
            Err(error) => {
                tracing::warn!(
                    event = event.name(),
                    command = %entry.command,
                    %error,
                    "hook did not run"
                );
                Said::Nothing
            }
        }
    }

    /// The session's exports, plus — for `SessionStart` alone, which is where the
    /// reference defines it — the file a hook writes the next ones into.
    fn env(&self, event: HookEvent, session: &SessionId) -> BTreeMap<String, String> {
        let mut env = self.sessions.env(session);
        if event == HookEvent::SessionStart {
            let file = self.sessions.file(session);
            env.insert(
                "BINGO_ENV_FILE".to_string(),
                file.to_string_lossy().into_owned(),
            );
        }
        env
    }
}

fn deadline(event: HookEvent, entry: &HookEntry) -> Duration {
    let Some(seconds) = entry.timeout else {
        return match event {
            HookEvent::SessionEnd => SESSION_END_TIMEOUT,
            _ => TIMEOUT,
        };
    };
    let asked = Duration::from_secs(seconds);
    match event {
        HookEvent::SessionEnd => asked.min(SESSION_END_CEILING),
        _ => asked,
    }
}

fn read(event: HookEvent, entry: &HookEntry, completed: run::Completed) -> Said {
    let verdict = Verdict::read(&completed.stdout).unwrap_or_default();
    match completed.code {
        0 => Said::Spoke(verdict),
        2 => Said::Blocked(blocking_reason(&verdict, &completed.stderr)),
        code => {
            tracing::warn!(
                event = event.name(),
                command = %entry.command,
                code,
                stderr = %completed.stderr,
                "hook exited non-zero; continuing"
            );
            Said::Nothing
        }
    }
}

/// Exit 2 says what it blocked for in its JSON if it printed any, in its stderr
/// otherwise. A hook that blocked silently leaves this empty; the one sentence
/// that stands in for silence is minted where an outcome is (`hook::because`).
fn blocking_reason(verdict: &Verdict, stderr: &str) -> String {
    verdict
        .decision()
        .map(|(_, reason)| reason)
        .filter(|reason| !reason.is_empty())
        .unwrap_or_else(|| stderr.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HookKind;

    fn entry(command: &str, timeout: Option<u64>) -> HookEntry {
        HookEntry {
            kind: HookKind::Command,
            command: command.into(),
            timeout,
        }
    }

    #[test]
    fn a_hook_gets_a_minute_unless_it_asked_for_something_else() {
        assert_eq!(
            deadline(HookEvent::PreToolUse, &entry("true", None)),
            TIMEOUT
        );
        assert_eq!(
            deadline(HookEvent::PreToolUse, &entry("true", Some(5))),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn session_end_is_quick_by_default_and_capped_when_it_is_not() {
        assert_eq!(
            deadline(HookEvent::SessionEnd, &entry("true", None)),
            SESSION_END_TIMEOUT
        );
        assert_eq!(
            deadline(HookEvent::SessionEnd, &entry("true", Some(5))),
            Duration::from_secs(5)
        );
        assert_eq!(
            deadline(HookEvent::SessionEnd, &entry("true", Some(600))),
            SESSION_END_CEILING
        );
    }

    #[test]
    fn a_block_says_why_in_the_hook_s_own_words() {
        let json = Verdict::read(r#"{"decision": "deny", "reason": "not that file"}"#)
            .expect("the hook spoke");
        assert_eq!(blocking_reason(&json, "on stderr"), "not that file");
        assert_eq!(
            blocking_reason(&Verdict::default(), "on stderr"),
            "on stderr"
        );
        // Silence is left empty here and given words where the outcome is made.
        assert_eq!(blocking_reason(&Verdict::default(), ""), "");
    }
}

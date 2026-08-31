//! When a job is worth waking a session for, and what it says.
//!
//! Finishing always is, and so is a line the call asked to be told about.
//! Output merely growing never is: that is what `BashOutput` is for
//! (ADR-0018 §4). The message goes through `deliver(…, Wake)`, the same door
//! a message to an agent uses, so `--print`, RPC, the channels and the TUI all
//! hear it without any of them knowing this plugin exists.

use bingo_sdk::{Delivery, HostHandle, Input, IntentId, KernelError, Origin, SessionId};
use regex::Regex;

use crate::jobs::{Job, State};

/// The surface a job's notification comes from. It is not a peer, so it signs
/// nothing: the text says what happened and the kernel adds no `[from …]`.
const SURFACE: &str = "bash";

/// What a call asked to be told about while its job runs.
#[derive(Debug, Default)]
pub struct Conditions {
    substrings: Vec<String>,
    pattern: Option<Regex>,
}

impl Conditions {
    /// A pattern that does not compile is the caller's mistake, and is worth
    /// refusing the call over: a job that silently never notifies is worse.
    pub fn new(on: Vec<String>, regex: Option<String>) -> Result<Self, String> {
        let pattern = match regex {
            Some(source) => Some(
                Regex::new(&source).map_err(|e| format!("notify_regex is not a pattern: {e}"))?,
            ),
            None => None,
        };
        Ok(Self {
            substrings: on.into_iter().filter(|s| !s.is_empty()).collect(),
            pattern,
        })
    }

    pub fn watched(&self) -> bool {
        !self.substrings.is_empty() || self.pattern.is_some()
    }

    /// The first line of `text` that answers one of the conditions.
    pub fn hit<'a>(&self, text: &'a str) -> Option<&'a str> {
        text.lines().find(|line| self.matches(line))
    }

    fn matches(&self, line: &str) -> bool {
        self.substrings.iter().any(|s| line.contains(s.as_str()))
            || self.pattern.as_ref().is_some_and(|p| p.is_match(line))
    }
}

/// What a job that has ended says to the session that started it. A condition
/// that only matched in its last breath is carried here rather than sent on
/// its own: one ending, one message.
pub fn finished(job: &Job, state: State, hit: Option<&str>) -> String {
    let matched = hit
        .map(|line| format!("\nIt matched: {}", line.trim_end()))
        .unwrap_or_default();
    format!(
        "Background job {} {} after {}.{matched}\n`BashOutput` with id \"{}\" reads what it wrote; its log is {}.",
        job.named(),
        state.said(),
        job.age(),
        job.id,
        job.log.display(),
    )
}

/// What a job says the first time its output answers a condition. It says the
/// job is still going, so nothing reads this as an ending.
pub fn matched(job: &Job, line: &str) -> String {
    format!(
        "Background job {} is still running and wrote a line you asked to be told about:\n{}\n`BashOutput` with id \"{}\" reads on from there.",
        job.named(),
        line.trim_end(),
        job.id,
    )
}

/// Open a turn on the session that started the job. A session that has gone
/// takes the message nowhere, and the error is the caller's to record — a
/// reader task must never fail loudly over one.
pub async fn wake(host: &HostHandle, to: &SessionId, text: String) -> Result<(), KernelError> {
    host.deliver(
        to,
        IntentId::mint(),
        Input::text(text, Origin::surface(SURFACE)),
        Delivery::Wake,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn job() -> Job {
        Job::new(
            "ab12cd34".into(),
            "cargo test --workspace".into(),
            PathBuf::from("/tmp/bash/ab12cd34.log"),
            SessionId::from_raw("ses_test"),
        )
    }

    fn conditions(on: &[&str], regex: Option<&str>) -> Conditions {
        Conditions::new(
            on.iter().map(|s| (*s).to_string()).collect(),
            regex.map(str::to_string),
        )
        .expect("the conditions compile")
    }

    #[test]
    fn a_call_that_asked_for_nothing_watches_nothing() {
        let none = conditions(&[], None);
        assert!(!none.watched());
        assert_eq!(none.hit("anything at all\n"), None);
        assert!(!conditions(&[""], None).watched(), "an empty word is none");
    }

    #[test]
    fn a_substring_is_matched_on_the_line_that_carries_it() {
        let watch = conditions(&["Compiling", "error["], None);
        assert_eq!(
            watch.hit("Finished\nerror[E0308]: mismatched types\nmore\n"),
            Some("error[E0308]: mismatched types")
        );
        assert_eq!(watch.hit("nothing to see\n"), None);
    }

    #[test]
    fn a_pattern_is_matched_too_and_a_bad_one_is_refused() {
        let watch = conditions(&[], Some(r"^test result: FAILED"));
        assert_eq!(
            watch.hit("running 3 tests\ntest result: FAILED. 1 failed\n"),
            Some("test result: FAILED. 1 failed")
        );
        let bad = Conditions::new(Vec::new(), Some("(unclosed".into()));
        assert!(bad.is_err(), "a pattern that cannot compile is refused");
    }

    #[test]
    fn a_completion_names_the_job_its_state_and_where_to_read_it() {
        let job = job();
        let text = finished(&job, State::Exited { code: 1 }, None);
        assert!(text.contains(&job.id), "{text}");
        assert!(text.contains("cargo test --workspace"), "{text}");
        assert!(text.contains("exited with code 1"), "{text}");
        assert!(text.contains("BashOutput"), "{text}");
        assert!(text.contains("/tmp/bash/ab12cd34.log"), "{text}");
        assert!(!text.contains("It matched"), "{text}");
    }

    /// A condition that only matched as the job ended is one message, not two.
    #[test]
    fn a_condition_matched_at_the_end_rides_the_completion() {
        let text = finished(&job(), State::Killed, Some("error[E0308]\n"));
        assert!(text.contains("It matched: error[E0308]\n"), "{text}");
        assert!(text.contains("killed"), "{text}");
    }

    #[test]
    fn a_condition_hit_says_the_job_is_still_going() {
        let job = job();
        let text = matched(&job, "error[E0308]: mismatched types\n");
        assert!(text.contains("still running"), "{text}");
        assert!(text.contains("error[E0308]"), "{text}");
        assert!(!text.contains("exited"), "{text}");
    }
}

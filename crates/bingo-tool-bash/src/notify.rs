//! When a job is worth waking a session for, and what it says.
//!
//! Finishing always is, and so is a line the call asked to be told about.
//! Output merely growing never is: that is what `BashOutput` is for
//! (ADR-0018 §4). The message goes through `deliver(…, Wake)`, the same door
//! an agent's followup uses, so `--print`, RPC, the channels and the TUI all
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
    /// The conditions outlive their first hit — `notify_all` (ADR-0018 §8).
    ongoing: bool,
}

impl Conditions {
    /// A pattern that does not compile is the caller's mistake, and is worth
    /// refusing the call over: a job that silently never notifies is worse.
    /// An ongoing watch with nothing to watch for is the same mistake.
    pub fn new(on: Vec<String>, regex: Option<String>, ongoing: bool) -> Result<Self, String> {
        let pattern = match regex {
            Some(source) => Some(
                Regex::new(&source).map_err(|e| format!("notify_regex is not a pattern: {e}"))?,
            ),
            None => None,
        };
        let watch = Self {
            substrings: on.into_iter().filter(|s| !s.is_empty()).collect(),
            pattern,
            ongoing,
        };
        if ongoing && !watch.watched() {
            return Err(
                "notify_all watches nothing on its own: give notify_on a word or \
                        notify_regex a pattern for it to keep watching for."
                    .into(),
            );
        }
        Ok(watch)
    }

    pub fn watched(&self) -> bool {
        !self.substrings.is_empty() || self.pattern.is_some()
    }

    /// Whether the conditions go on watching past their first hit.
    pub fn ongoing(&self) -> bool {
        self.ongoing
    }

    /// How many lines of `text` answer a condition, and the last that did.
    pub fn tally<'t>(&'t self, text: &'t str) -> Tally<'t> {
        Tally::of(self.matching(text))
    }

    /// The first line of `text` that answers one of the conditions: the tally
    /// of its first match and nothing past it.
    pub fn hit<'t>(&'t self, text: &'t str) -> Option<&'t str> {
        Tally::of(self.matching(text).take(1)).last
    }

    fn matching<'t>(&'t self, text: &'t str) -> impl Iterator<Item = &'t str> {
        text.lines().filter(|line| self.matches(line))
    }

    fn matches(&self, line: &str) -> bool {
        self.substrings.iter().any(|s| line.contains(s.as_str()))
            || self.pattern.as_ref().is_some_and(|p| p.is_match(line))
    }
}

/// What a window of a job's output held for the conditions watching it: how
/// many lines answered them, and the last that did. Two facts are all a notice
/// ever carries — the log holds the lines, and `BashOutput` reads them
/// (ADR-0018 §8).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally<'a> {
    pub count: usize,
    pub last: Option<&'a str>,
}

impl<'a> Tally<'a> {
    fn of(lines: impl Iterator<Item = &'a str>) -> Self {
        lines.fold(Self::default(), |seen, line| Self {
            count: seen.count + 1,
            last: Some(line),
        })
    }
}

/// One line worth waking a session over, and how many more matched since the
/// last notice went out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub line: String,
    pub more: usize,
}

impl Notice {
    /// A line that is news on its own, with nothing behind it.
    pub fn of(line: &str) -> Self {
        Self {
            line: line.to_string(),
            more: 0,
        }
    }

    /// Fold a window's matches into whatever a quiet window is still holding.
    /// The newest matching line is the one shown and every older one is the
    /// count; a window that matched nothing changes nothing.
    pub fn folded(held: Option<Self>, fresh: &Tally<'_>) -> Option<Self> {
        let Some(line) = fresh.last else {
            return held;
        };
        Some(Self {
            line: line.to_string(),
            more: held.map_or(0, |held| held.more + 1) + fresh.count - 1,
        })
    }

    /// What a notice says about the lines a quiet window swallowed.
    fn and_more(&self) -> String {
        match self.more {
            0 => String::new(),
            1 => "\n…and 1 more line matched since the last notice.".into(),
            more => format!("\n…and {more} more lines matched since the last notice."),
        }
    }
}

/// What a job that has ended says to the session that started it. A condition
/// that only matched in its last breath is carried here rather than sent on
/// its own, and so is a tally a quiet window was still holding: one ending,
/// one message.
pub fn finished(job: &Job, state: State, pending: Option<&Notice>) -> String {
    let matched = pending
        .map(|notice| {
            format!(
                "\nIt matched: {}{}",
                notice.line.trim_end(),
                notice.and_more()
            )
        })
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

/// What a job says when its output answers a condition. It says the job is
/// still going, so nothing reads this as an ending.
pub fn matched(job: &Job, notice: &Notice) -> String {
    format!(
        "Background job {} is still running and wrote a line you asked to be told about:\n{}{}\n`BashOutput` with id \"{}\" reads on from there.",
        job.named(),
        notice.line.trim_end(),
        notice.and_more(),
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
            false,
        )
        .expect("the conditions compile")
    }

    fn tally<'a>(count: usize, last: impl Into<Option<&'a str>>) -> Tally<'a> {
        Tally {
            count,
            last: last.into(),
        }
    }

    #[test]
    fn a_call_that_asked_for_nothing_watches_nothing() {
        let none = conditions(&[], None);
        assert!(!none.watched());
        assert!(!none.ongoing());
        assert_eq!(none.hit("anything at all\n"), None);
        assert_eq!(none.tally("anything at all\n"), tally(0, None));
        assert!(!conditions(&[""], None).watched(), "an empty word is none");
    }

    /// The brick under both readings: a window of text as a count and the last
    /// line that answered.
    #[test]
    fn a_window_is_tallied_as_a_count_and_the_last_line_that_answered() {
        let watch = conditions(&["HIT"], Some(r"^boom"));
        for (text, want) in [
            ("", tally(0, None)),
            ("nothing to see\n", tally(0, None)),
            ("HIT one\n", tally(1, "HIT one")),
            (
                "HIT one\nquiet\nHIT two\nHIT three\n",
                tally(3, "HIT three"),
            ),
            ("HIT one\nboom over there\n", tally(2, "boom over there")),
            ("no newline, but a HIT", tally(1, "no newline, but a HIT")),
        ] {
            assert_eq!(watch.tally(text), want, "{text:?}");
        }
    }

    /// The first-hit read is that same tally stopped at one match, so the two
    /// readings can never disagree about what answers a condition.
    #[test]
    fn the_first_hit_read_is_the_tally_stopped_at_its_first_match() {
        let watch = conditions(&["HIT"], None);
        let text = "HIT one\nHIT two\nHIT three\n";
        assert_eq!(watch.hit(text), Some("HIT one"));
        assert_eq!(
            watch.tally(text).count,
            3,
            "the tally reads on; the hit does not"
        );
        assert_eq!(watch.hit("nothing here\n"), None);
    }

    /// `notify_all` with nothing to watch for is refused the way a pattern
    /// that cannot compile is: a job that can never notify is worse than a
    /// call that comes back corrected (ADR-0018 §8).
    #[test]
    fn an_ongoing_watch_with_nothing_to_watch_for_is_refused() {
        let refused = Conditions::new(Vec::new(), None, true).expect_err("nothing to watch");
        assert!(refused.contains("notify_all"), "{refused}");
        assert!(refused.contains("notify_on"), "{refused}");
        assert!(refused.contains("notify_regex"), "{refused}");
        let empty_words =
            Conditions::new(vec![String::new()], None, true).expect_err("an empty word is none");
        assert!(empty_words.contains("notify_all"), "{empty_words}");

        let watching = Conditions::new(vec!["HIT".into()], None, true).expect("a word to watch");
        assert!(watching.ongoing());
        assert!(Conditions::new(Vec::new(), Some("boom".into()), true).is_ok());
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
        let bad = Conditions::new(Vec::new(), Some("(unclosed".into()), false);
        assert!(bad.is_err(), "a pattern that cannot compile is refused");
    }

    /// What a quiet window holds is one line and a count, folded window by
    /// window: the newest line shows, the older ones are the number.
    #[test]
    fn a_held_notice_folds_the_next_window_into_a_line_and_a_count() {
        let watch = conditions(&["HIT"], None);
        let held = Notice::folded(None, &watch.tally("HIT one\n"));
        assert_eq!(held, Some(Notice::of("HIT one")));

        let nothing_new = Notice::folded(held.clone(), &watch.tally("quiet\n"));
        assert_eq!(
            nothing_new, held,
            "a window that matched nothing changes nothing"
        );

        let folded = Notice::folded(held, &watch.tally("HIT two\nHIT three\n"));
        assert_eq!(
            folded,
            Some(Notice {
                line: "HIT three".into(),
                more: 2
            })
        );
        assert_eq!(
            Notice::folded(None, &watch.tally("nothing at all\n")),
            None,
            "nothing matched and nothing was held"
        );
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
        let text = finished(&job(), State::Killed, Some(&Notice::of("error[E0308]\n")));
        assert!(text.contains("It matched: error[E0308]\n"), "{text}");
        assert!(text.contains("killed"), "{text}");
        assert!(!text.contains("since the last notice"), "{text}");
    }

    /// A tally the quiet window was still holding rides the completion too:
    /// one line, one count, and the log holds the rest (ADR-0018 §8).
    #[test]
    fn a_tally_the_window_held_rides_the_completion_as_a_count() {
        let pending = Notice {
            line: "error[E0433]: no `Foo`\n".into(),
            more: 12,
        };
        let text = finished(&job(), State::Exited { code: 101 }, Some(&pending));
        assert!(
            text.contains("It matched: error[E0433]: no `Foo`"),
            "{text}"
        );
        assert!(
            text.contains("…and 12 more lines matched since the last notice."),
            "{text}"
        );
    }

    #[test]
    fn a_condition_hit_says_the_job_is_still_going() {
        let job = job();
        let text = matched(&job, &Notice::of("error[E0308]: mismatched types\n"));
        assert!(text.contains("still running"), "{text}");
        assert!(text.contains("error[E0308]"), "{text}");
        assert!(!text.contains("exited"), "{text}");
        assert!(!text.contains("since the last notice"), "{text}");
    }

    /// The clause an ongoing watch adds, counted rather than listed.
    #[test]
    fn a_hit_that_follows_a_quiet_window_counts_what_the_window_swallowed() {
        let job = job();
        let one = matched(
            &job,
            &Notice {
                line: "HIT again".into(),
                more: 1,
            },
        );
        assert!(
            one.contains("…and 1 more line matched since the last notice."),
            "{one}"
        );
        let many = matched(
            &job,
            &Notice {
                line: "HIT again".into(),
                more: 7,
            },
        );
        assert!(
            many.contains("…and 7 more lines matched since the last notice."),
            "{many}"
        );
        assert!(many.contains("HIT again"), "{many}");
    }
}

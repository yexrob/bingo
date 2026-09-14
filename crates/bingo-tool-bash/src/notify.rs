//! When a job is worth waking a session for, and what it says.
//!
//! Finishing always is, and so is a line the call asked to be told about.
//! Output merely growing never is: that is what `BashOutput` is for
//! (ADR-0018 §4). The message goes through `deliver(…, Wake)`, the same door
//! a message to an agent uses, so `--print`, RPC, the channels and the TUI all
//! hear it without any of them knowing this plugin exists.

use std::time::Duration;

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
    /// The quiet window the call pinned — `notify_quiet`; none lets the
    /// window follow the stream.
    quiet: Option<Duration>,
}

impl Conditions {
    /// A pattern that does not compile is the caller's mistake, and is worth
    /// refusing the call over: a job that silently never notifies is worse.
    /// An ongoing watch with nothing to watch for is the same mistake, and so
    /// is a pinned window on a watch that fires once.
    pub fn new(
        on: Vec<String>,
        regex: Option<String>,
        ongoing: bool,
        quiet_ms: Option<u64>,
    ) -> Result<Self, String> {
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
            quiet: quiet_ms.map(Duration::from_millis),
        };
        if ongoing && !watch.watched() {
            return Err(
                "notify_all watches nothing on its own: give notify_on a word or \
                        notify_regex a pattern for it to keep watching for."
                    .into(),
            );
        }
        if watch.quiet.is_some() && !ongoing {
            return Err(
                "notify_quiet paces a watch that keeps going, and this one stops at \
                        its first hit: set notify_all true for the window to apply, or \
                        drop notify_quiet."
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

    /// The quiet window the call pinned, if it chose one.
    pub fn quiet(&self) -> Option<Duration> {
        self.quiet
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

/// One wake of a session over a job's output: the notice, and for an ongoing
/// watch the pace it came at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wake {
    pub notice: Notice,
    /// None for the default watch, which fires once and has no next.
    pub cadence: Option<Cadence>,
}

impl Wake {
    /// The default watch's one notice.
    pub fn once(notice: Notice) -> Self {
        Self {
            notice,
            cadence: None,
        }
    }

    /// An ongoing watch's notice, with its pace.
    pub fn paced(notice: Notice, cadence: Cadence) -> Self {
        Self {
            notice,
            cadence: Some(cadence),
        }
    }
}

/// How an ongoing watch is pacing its wakes: how long since the last one, and
/// the quiet the next has to wait out (ADR-0018 §8). The model reads the
/// stream's rate off this instead of guessing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cadence {
    /// None on the first notice, which has no last one.
    pub since: Option<Duration>,
    pub window: Duration,
}

impl Cadence {
    /// The clause after the count: when the last notice was.
    fn since_said(&self) -> String {
        self.since
            .map_or_else(String::new, |since| format!(", {} ago", said(since)))
    }

    /// The sentence that says how soon the next notice can come.
    fn next_said(&self) -> String {
        if self.window.is_zero() {
            "\nThe next matching line wakes you at once.".into()
        } else {
            format!(
                "\nThe next notice comes no sooner than {} after this one.",
                said(self.window)
            )
        }
    }
}

/// A duration as a notice says it: milliseconds under a second, whole seconds
/// under a minute, minutes and seconds past that.
fn said(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs {
        0 => format!("{} ms", duration.as_millis()),
        1..=59 => format!("{secs} s"),
        _ => format!("{}m {}s", secs / 60, secs % 60),
    }
}

/// The clock time a notice stamps itself with, so the model knows when it
/// came and not only that it did.
pub fn clock() -> String {
    jiff::Zoned::now().strftime("%H:%M:%S").to_string()
}

/// What a job that has ended says to the session that started it. A condition
/// that only matched in its last breath is carried here rather than sent on
/// its own, and so is a tally a quiet window was still holding: one ending,
/// one message.
pub fn finished(job: &Job, state: State, pending: Option<&Notice>, at: &str) -> String {
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
        "Background job {} {} at {at}, after {}.{matched}\n`BashOutput` with id \"{}\" reads what it wrote; its log is {}.",
        job.named(),
        state.said(),
        job.age(),
        job.id,
        job.log.display(),
    )
}

/// What a job says when its output answers a condition. It says the job is
/// still going, so nothing reads this as an ending; an ongoing watch adds when
/// the last notice was and how soon the next can come.
pub fn matched(job: &Job, wake: &Wake, at: &str) -> String {
    let notice = &wake.notice;
    let pace = wake.cadence.as_ref();
    let more = if notice.more == 0 {
        pace.and_then(|c| c.since)
            .map(|since| format!("\nThe last notice was {} ago.", said(since)))
            .unwrap_or_default()
    } else {
        format!(
            "{}{}.",
            notice.and_more().trim_end_matches('.'),
            pace.map(Cadence::since_said).unwrap_or_default()
        )
    };
    let next = pace.map(Cadence::next_said).unwrap_or_default();
    format!(
        "Background job {} is still running and at {at} wrote a line you asked to be told about:\n{}{more}{next}\n`BashOutput` with id \"{}\" reads on from there.",
        job.named(),
        notice.line.trim_end(),
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
            None,
        )
        .expect("the conditions compile")
    }

    fn paced(more: usize, since: Option<u64>, window: u64) -> Wake {
        Wake::paced(
            Notice {
                line: "HIT again".into(),
                more,
            },
            Cadence {
                since: since.map(Duration::from_secs),
                window: Duration::from_secs(window),
            },
        )
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
        let refused = Conditions::new(Vec::new(), None, true, None).expect_err("nothing to watch");
        assert!(refused.contains("notify_all"), "{refused}");
        assert!(refused.contains("notify_on"), "{refused}");
        assert!(refused.contains("notify_regex"), "{refused}");
        let empty_words = Conditions::new(vec![String::new()], None, true, None)
            .expect_err("an empty word is none");
        assert!(empty_words.contains("notify_all"), "{empty_words}");

        let watching =
            Conditions::new(vec!["HIT".into()], None, true, None).expect("a word to watch");
        assert!(watching.ongoing());
        assert_eq!(
            watching.quiet(),
            None,
            "unpinned, the window follows the stream"
        );
        assert!(Conditions::new(Vec::new(), Some("boom".into()), true, None).is_ok());
    }

    /// `notify_quiet` paces an ongoing watch and nothing else: on the default
    /// watch it is refused in words that name the way round (ADR-0018 §8).
    #[test]
    fn a_pinned_window_needs_an_ongoing_watch() {
        let pinned =
            Conditions::new(vec!["HIT".into()], None, true, Some(5_000)).expect("a pinned watch");
        assert_eq!(pinned.quiet(), Some(Duration::from_secs(5)));

        let refused = Conditions::new(vec!["HIT".into()], None, false, Some(5_000))
            .expect_err("a window on a watch that fires once");
        assert!(refused.contains("notify_quiet"), "{refused}");
        assert!(refused.contains("notify_all"), "{refused}");
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
        let bad = Conditions::new(Vec::new(), Some("(unclosed".into()), false, None);
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
        let text = finished(&job, State::Exited { code: 1 }, None, "16:33:51");
        assert!(text.contains(&job.id), "{text}");
        assert!(text.contains("at 16:33:51"), "{text}");
        assert!(text.contains("cargo test --workspace"), "{text}");
        assert!(text.contains("exited with code 1"), "{text}");
        assert!(text.contains("BashOutput"), "{text}");
        assert!(text.contains("/tmp/bash/ab12cd34.log"), "{text}");
        assert!(!text.contains("It matched"), "{text}");
    }

    /// A condition that only matched as the job ended is one message, not two.
    #[test]
    fn a_condition_matched_at_the_end_rides_the_completion() {
        let text = finished(
            &job(),
            State::Killed,
            Some(&Notice::of("error[E0308]\n")),
            "now",
        );
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
        let text = finished(&job(), State::Exited { code: 101 }, Some(&pending), "now");
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
        let wake = Wake::once(Notice::of("error[E0308]: mismatched types\n"));
        let text = matched(&job, &wake, "16:33:51");
        assert!(text.contains("still running"), "{text}");
        assert!(text.contains("at 16:33:51 wrote"), "{text}");
        assert!(text.contains("error[E0308]"), "{text}");
        assert!(!text.contains("exited"), "{text}");
        assert!(!text.contains("since the last notice"), "{text}");
        assert!(
            !text.contains("The next"),
            "a watch that fires once has no next: {text}"
        );
    }

    /// The clauses an ongoing watch adds: the count, when the last notice
    /// was, and how soon the next can come — the pace, never a list.
    #[test]
    fn an_ongoing_notice_says_its_count_its_span_and_its_next() {
        let job = job();
        let first = matched(&job, &paced(0, None, 0), "now");
        assert!(
            !first.contains("last notice"),
            "a first notice has no last: {first}"
        );
        assert!(
            first.contains("The next matching line wakes you at once."),
            "{first}"
        );

        let one = matched(&job, &paced(1, Some(12), 8), "now");
        assert!(
            one.contains("…and 1 more line matched since the last notice, 12 s ago."),
            "{one}"
        );
        assert!(
            one.contains("The next notice comes no sooner than 8 s after this one."),
            "{one}"
        );

        let many = matched(&job, &paced(7, None, 30), "now");
        assert!(
            many.contains("…and 7 more lines matched since the last notice."),
            "{many}"
        );
        assert!(many.contains("HIT again"), "{many}");
        assert!(many.contains("no sooner than 30 s"), "{many}");

        let quiet = matched(&job, &paced(0, Some(90), 0), "now");
        assert!(quiet.contains("The last notice was 1m 30s ago."), "{quiet}");
        assert!(!quiet.contains("more line"), "{quiet}");
    }

    #[test]
    fn a_duration_is_said_the_way_a_person_reads_it() {
        assert_eq!(said(Duration::from_millis(250)), "250 ms");
        assert_eq!(said(Duration::from_secs(1)), "1 s");
        assert_eq!(said(Duration::from_millis(16_400)), "16 s");
        assert_eq!(said(Duration::from_secs(125)), "2m 5s");
    }
}

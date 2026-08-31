//! One job's life, from the moment nobody is waiting for it.
//!
//! The task started here is the only owner of the process, which is why
//! `KillShell` asks rather than kills: it flips the job's token and this loop
//! does the signalling. On the way it looks at what the job has written for a
//! line the call asked to be told about; at the end it settles the state,
//! republishes the rail and wakes the session that started it. A session that
//! has gone takes the message nowhere, and the log says so — a reader task
//! never fails loudly.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bingo_sdk::HostHandle;
use process_wrap::tokio::ChildWrapper;
use tokio::sync::Mutex;
// The tokio clock, so a test can drive the quiet window instead of waiting it
// out: `std::time::Instant` would not move under `tokio::time::pause`.
use tokio::time::Instant;

use crate::jobs::{Job, Jobs, State};
use crate::notify::{self, Conditions, Notice};
use crate::run::{self, Running};
use crate::sink::Sink;

/// How long a `SIGTERM`ed group has to leave on its own terms.
const GRACE: Duration = Duration::from_secs(2);

/// How often a watched job's new output is read for a condition.
const SCAN: Duration = Duration::from_millis(250);

/// How much quiet one notice of an ongoing watch buys. A pattern that answers
/// on every line of a busy log must not wake a session on every line, so what
/// the window swallows becomes a count on the next notice (ADR-0018 §8).
const QUIET: Duration = Duration::from_secs(30);

/// Everything one job's task needs.
pub struct Watch {
    pub jobs: Arc<Jobs>,
    pub job: Arc<Job>,
    pub running: Running,
    pub conditions: Conditions,
    pub host: HostHandle,
}

/// Take the job over. The task lives as long as the process does, and the
/// process dies with this one (ADR-0018): no daemon, no queue.
pub fn take(watch: Watch) {
    tokio::spawn(supervise(watch));
}

async fn supervise(watch: Watch) {
    let Watch {
        jobs,
        job,
        mut running,
        conditions,
        host,
    } = watch;
    let mut scan = Scan::new(&conditions);
    let state = wait_out(&mut running.child, &job, &mut scan, &host, &running.sink).await;
    run::drain(running.readers).await;
    // The last of the output only reached the log once the readers were done.
    let pending = scan.last_look(&job.log).await;
    job.finished(state);
    jobs.publish(&host, &job.session).await;
    announce(
        &host,
        &job,
        &running.sink,
        notify::finished(&job, state, pending.as_ref()),
    )
    .await;
}

/// Wait for the job to end, or for someone to ask it to, reading what it
/// writes on a slow clock while it works.
async fn wait_out(
    child: &mut Box<dyn ChildWrapper>,
    job: &Job,
    scan: &mut Scan<'_>,
    host: &HostHandle,
    sink: &Mutex<Sink>,
) -> State {
    let asked = job.killed();
    loop {
        tokio::select! {
            status = child.wait() => {
                return status.map(state_of).unwrap_or(State::Killed);
            }
            () = asked.cancelled() => return end_it(child).await,
            () = tokio::time::sleep(SCAN) => {
                if let Some(notice) = scan.look(&job.log).await {
                    announce(host, job, sink, notify::matched(job, &notice)).await;
                }
            }
        }
    }
}

/// `SIGTERM` first, so a program that cleans up after itself gets to; the
/// signal it cannot answer only once the grace is spent.
async fn end_it(child: &mut Box<dyn ChildWrapper>) -> State {
    let _ = child.signal(run::TERM);
    if let Ok(Ok(status)) = tokio::time::timeout(GRACE, child.wait()).await {
        return state_of(status);
    }
    run::kill(child).await;
    State::Killed
}

/// A status as a job's state. A process that took a signal has no code of its
/// own to report, whoever sent it.
fn state_of(status: std::process::ExitStatus) -> State {
    match status.code() {
        Some(code) => State::Exited { code },
        None => State::Killed,
    }
}

/// Wake the session that started the job, or leave the reason in its log.
async fn announce(host: &HostHandle, job: &Job, sink: &Mutex<Sink>, text: String) {
    let Err(error) = notify::wake(host, &job.session, text).await else {
        return;
    };
    let note = format!(
        "nobody was told this job had news: the session that started it is gone ({})",
        error.message
    );
    tracing::debug!(job = %job.id, %error, "a job's session could not be woken");
    if let Some(log) = sink.lock().await.log() {
        let _ = log.note(&note).await;
    }
}

/// The reading of a job's log that looks for a condition, and what it makes of
/// what it finds.
struct Scan<'a> {
    conditions: &'a Conditions,
    cursor: u64,
    mode: Mode,
}

/// What a scan does with a hit after the first one.
enum Mode {
    /// The default: one notice, and silence after it. A pattern that matches
    /// every line must not wake a session every line.
    Once { fired: bool },
    /// `notify_all`: every hit is news, but no more than one notice a quiet
    /// window; what the window swallows is held as a count for the next one.
    All {
        last_wake: Option<Instant>,
        held: Option<Notice>,
    },
}

impl<'a> Scan<'a> {
    fn new(conditions: &'a Conditions) -> Self {
        let mode = if conditions.ongoing() {
            Mode::All {
                last_wake: None,
                held: None,
            }
        } else {
            Mode::Once { fired: false }
        };
        Self {
            conditions,
            cursor: 0,
            mode,
        }
    }

    /// What the output written since the last look has earned, if anything.
    async fn look(&mut self, log: &Path) -> Option<Notice> {
        let text = self.read(log).await?;
        let conditions = self.conditions;
        match &mut self.mode {
            Mode::Once { fired } => first(conditions, &text, fired),
            Mode::All { last_wake, held } => throttled(conditions, &text, last_wake, held),
        }
    }

    /// What is left to say now the job has ended. The completion is going out
    /// regardless, so the quiet window holds nothing back — this is the one
    /// thing a count with no hit behind it ever rides (ADR-0018 §8).
    async fn last_look(&mut self, log: &Path) -> Option<Notice> {
        let last = self.look(log).await;
        match &mut self.mode {
            Mode::Once { .. } => last,
            Mode::All { held, .. } => last.or_else(|| held.take()),
        }
    }

    /// The output written since the last look, or `None` when there is nothing
    /// left to look for: an unwatched job, and a job whose one notice has
    /// already gone, cost no read at all.
    async fn read(&mut self, log: &Path) -> Option<String> {
        if !self.watching() {
            return None;
        }
        let window = crate::log::window(log, self.cursor, WINDOW).await.ok()?;
        self.cursor = window.cursor;
        Some(window.text)
    }

    fn watching(&self) -> bool {
        self.conditions.watched() && !matches!(self.mode, Mode::Once { fired: true })
    }
}

/// The default reading: the first line that answers a condition, once.
fn first(conditions: &Conditions, text: &str, fired: &mut bool) -> Option<Notice> {
    let hit = conditions.hit(text)?;
    *fired = true;
    Some(Notice::of(hit))
}

/// The ongoing reading: a hit wakes when the quiet window has passed since the
/// last notice, and is only counted inside it. Nothing but a hit ever wakes
/// anything — the window ending on its own flushes no count, because a
/// suppressed hit is the same pattern matching again (ADR-0018 §8).
fn throttled(
    conditions: &Conditions,
    text: &str,
    last_wake: &mut Option<Instant>,
    held: &mut Option<Notice>,
) -> Option<Notice> {
    let fresh = conditions.tally(text);
    let folded = Notice::folded(held.take(), &fresh);
    if fresh.count == 0 || last_wake.is_some_and(|wake| wake.elapsed() < QUIET) {
        *held = folded;
        return None;
    }
    *last_wake = Some(Instant::now());
    folded
}

/// Bytes of new output one scan reads. A condition on a line further than this
/// behind waits for the next tick.
const WINDOW: usize = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::Log;

    async fn logged(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut log = Log::create(dir.path(), "job").await.expect("a log");
        log.write(text).await.expect("written");
        let path = log.path().to_path_buf();
        (dir, path)
    }

    fn conditions(on: &[&str]) -> Conditions {
        Conditions::new(on.iter().map(|s| (*s).to_string()).collect(), None, false)
            .expect("the conditions compile")
    }

    /// The same words, watched for the whole of the job (`notify_all`).
    fn ongoing(on: &[&str]) -> Conditions {
        Conditions::new(on.iter().map(|s| (*s).to_string()).collect(), None, true)
            .expect("the conditions compile")
    }

    /// A log to write into, and the path a scan reads it back from.
    async fn writing() -> (tempfile::TempDir, Log, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let log = Log::create(dir.path(), "job").await.expect("a log");
        let path = log.path().to_path_buf();
        (dir, log, path)
    }

    #[tokio::test]
    async fn a_scan_reads_on_from_where_it_stopped_and_fires_once() {
        let (_dir, mut log, path) = writing().await;
        let watched = conditions(&["FAILED"]);
        let mut scan = Scan::new(&watched);

        log.write("running\n").await.expect("written");
        assert_eq!(scan.look(&path).await, None, "nothing has matched yet");
        log.write("test result: FAILED\n").await.expect("written");
        assert_eq!(
            scan.look(&path).await,
            Some(Notice::of("test result: FAILED"))
        );
        log.write("test result: FAILED again\n").await.expect("w");
        assert_eq!(
            scan.look(&path).await,
            None,
            "one notification, not a storm"
        );
        assert_eq!(
            scan.last_look(&path).await,
            None,
            "the default says nothing more at the end either"
        );
    }

    // ---- the ongoing watch, on a clock the test drives (ADR-0018 §8) ------

    /// The leading edge: the first hit is news the moment it is read, and the
    /// hits behind it inside the window are only counted.
    #[tokio::test(start_paused = true)]
    async fn an_ongoing_watch_wakes_at_once_and_then_holds_the_burst() {
        let (_dir, mut log, path) = writing().await;
        let watched = ongoing(&["HIT"]);
        let mut scan = Scan::new(&watched);

        log.write("warming\nHIT one\n").await.expect("written");
        assert_eq!(scan.look(&path).await, Some(Notice::of("HIT one")));

        log.write("HIT two\n").await.expect("written");
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(scan.look(&path).await, None, "inside the window, counted");
        log.write("HIT three\nHIT four\n").await.expect("written");
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(scan.look(&path).await, None, "a burst is still one wake");
    }

    /// No trailing timer: the window running out flushes nothing on its own,
    /// because a held count is the same pattern that will match again.
    #[tokio::test(start_paused = true)]
    async fn the_window_ending_on_its_own_flushes_nothing() {
        let (_dir, mut log, path) = writing().await;
        let watched = ongoing(&["HIT"]);
        let mut scan = Scan::new(&watched);

        log.write("HIT one\n").await.expect("written");
        assert_eq!(scan.look(&path).await, Some(Notice::of("HIT one")));
        log.write("HIT two\n").await.expect("written");
        assert_eq!(scan.look(&path).await, None, "held by the window");

        tokio::time::advance(QUIET * 3).await;
        assert_eq!(
            scan.look(&path).await,
            None,
            "the quiet window ending is not news; only a hit is"
        );
    }

    /// The next hit past the window carries what the window swallowed, and
    /// leaves the count at nothing behind it.
    #[tokio::test(start_paused = true)]
    async fn the_first_hit_past_the_window_carries_the_count_and_resets_it() {
        let (_dir, mut log, path) = writing().await;
        let watched = ongoing(&["HIT"]);
        let mut scan = Scan::new(&watched);

        log.write("HIT one\nHIT two\n").await.expect("written");
        assert_eq!(
            scan.look(&path).await,
            Some(Notice {
                line: "HIT two".into(),
                more: 1
            }),
            "the newest line shows and the older one is the count"
        );
        log.write("HIT three\nHIT four\n").await.expect("written");
        assert_eq!(scan.look(&path).await, None, "held by the window");

        tokio::time::advance(QUIET).await;
        log.write("HIT five\n").await.expect("written");
        assert_eq!(
            scan.look(&path).await,
            Some(Notice {
                line: "HIT five".into(),
                more: 2
            })
        );

        tokio::time::advance(QUIET).await;
        log.write("HIT six\n").await.expect("written");
        assert_eq!(
            scan.look(&path).await,
            Some(Notice::of("HIT six")),
            "the count went with the notice that carried it"
        );
    }

    /// What no hit came back for rides the end of the job, and only that.
    #[tokio::test(start_paused = true)]
    async fn what_the_window_held_rides_the_end_of_the_job() {
        let (_dir, mut log, path) = writing().await;
        let watched = ongoing(&["HIT"]);
        let mut scan = Scan::new(&watched);

        log.write("HIT one\n").await.expect("written");
        assert_eq!(scan.look(&path).await, Some(Notice::of("HIT one")));
        log.write("HIT two\nHIT three\n").await.expect("written");
        assert_eq!(scan.look(&path).await, None, "held by the window");

        log.write("HIT four\n").await.expect("written");
        assert_eq!(
            scan.last_look(&path).await,
            Some(Notice {
                line: "HIT four".into(),
                more: 2
            }),
            "the last read and the held count are one message"
        );
        assert_eq!(
            scan.last_look(&path).await,
            None,
            "and the ending says it once"
        );
    }

    #[tokio::test]
    async fn a_job_nobody_asked_about_is_never_read() {
        let (_dir, path) = logged("error everywhere\n").await;
        let none = conditions(&[]);
        let mut scan = Scan::new(&none);
        assert_eq!(scan.look(&path).await, None);
        assert_eq!(scan.cursor, 0, "an unwatched job costs no read");
    }

    #[tokio::test]
    async fn a_log_that_is_not_there_is_not_a_failure() {
        let watched = conditions(&["boom"]);
        let mut scan = Scan::new(&watched);
        assert_eq!(scan.look(Path::new("/no/such/job.log")).await, None);
    }

    #[test]
    fn a_signalled_process_has_no_code_of_its_own() {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            state_of(std::process::ExitStatus::from_raw(0)),
            State::Exited { code: 0 }
        );
        // Raw 9 is "killed by SIGKILL": no exit code at all.
        assert_eq!(
            state_of(std::process::ExitStatus::from_raw(9)),
            State::Killed
        );
    }
}

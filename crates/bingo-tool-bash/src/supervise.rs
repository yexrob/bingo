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
use crate::notify::{self, Cadence, Conditions, Notice, Tally, Wake};
use crate::run::{self, Running};
use crate::sink::Sink;

/// How long a `SIGTERM`ed group has to leave on its own terms. Unix only:
/// nothing is asked on Windows, so nothing there waits to be answered.
#[cfg(unix)]
const GRACE: Duration = Duration::from_secs(2);

/// How often a watched job's new output is read for a condition.
const SCAN: Duration = Duration::from_millis(250);

/// How much quiet each line per second buys, once lines arrive faster than
/// the wakes go out: a stream at one line a second waits two, at fifteen it
/// waits the cap, and a single line waits nothing (ADR-0018 §8).
const PACE: Duration = Duration::from_secs(2);

/// The most quiet a stream can buy, however fast it runs.
const CAP: Duration = Duration::from_secs(30);

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
        notify::finished(&job, state, pending.as_ref(), &notify::clock()),
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
                if let Some(wake) = scan.look(&job.log).await {
                    let text = notify::matched(job, &wake, &notify::clock());
                    announce(host, job, sink, text).await;
                }
            }
        }
    }
}

/// `SIGTERM` first, so a program that cleans up after itself gets to; the
/// signal it cannot answer only once the grace is spent.
#[cfg(unix)]
async fn end_it(child: &mut Box<dyn ChildWrapper>) -> State {
    let _ = child.signal(run::TERM);
    if let Ok(Ok(status)) = tokio::time::timeout(GRACE, child.wait()).await {
        return state_of(status);
    }
    run::kill(child).await;
    State::Killed
}

/// The same end, on a platform with no signal to ask with.
///
/// Windows has nothing a process may answer and decline: a job object is
/// ended, not asked. Waiting a grace first would buy the program no chance to
/// clean up, only a slower kill, so the kill is the whole of it.
#[cfg(windows)]
async fn end_it(child: &mut Box<dyn ChildWrapper>) -> State {
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
    /// `notify_all`: every hit is news, paced by a quiet window that follows
    /// the stream (ADR-0018 §8).
    All(Ongoing),
}

/// How long an ongoing watch keeps quiet after a wake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Quiet {
    /// Follows the stream: what the last wake observed of its rate.
    Adaptive(Duration),
    /// `notify_quiet`: what the call asked for, whatever the stream does.
    Pinned(Duration),
}

impl Quiet {
    fn window(self) -> Duration {
        match self {
            Self::Adaptive(window) | Self::Pinned(window) => window,
        }
    }

    /// What a wake that carried `lines` after `since` of quiet teaches.
    fn observe(&mut self, lines: usize, since: Duration) {
        if let Self::Adaptive(window) = self {
            *window = next_window(lines, since);
        }
    }
}

/// The quiet a wake buys from the lines it had to fold. One line is not a
/// rate and buys nothing; past that, each line per second that arrived while
/// the watch was quiet buys `PACE`, up to `CAP`. A span shorter than a scan
/// tick is a tick: the lines could not have been read any sooner.
fn next_window(lines: usize, since: Duration) -> Duration {
    let extra = u32::try_from(lines.saturating_sub(1)).unwrap_or(u32::MAX);
    if extra == 0 {
        return Duration::ZERO;
    }
    let seconds = PACE.as_secs_f64() * f64::from(extra) / since.max(SCAN).as_secs_f64();
    if seconds >= CAP.as_secs_f64() {
        CAP
    } else {
        Duration::from_secs_f64(seconds)
    }
}

/// The state of an ongoing watch between two of its wakes.
struct Ongoing {
    /// When the watch began, which is what the first wake's span is since.
    started: Instant,
    last_wake: Option<Instant>,
    /// What the quiet window has swallowed so far, folded to a line and a
    /// count.
    held: Option<Notice>,
    quiet: Quiet,
}

impl Ongoing {
    fn new(quiet: Quiet) -> Self {
        Self {
            started: Instant::now(),
            last_wake: None,
            held: None,
            quiet,
        }
    }

    /// What a window of output earns. Anything to say — a fresh hit, or what
    /// an earlier tick held — goes out once the quiet since the last wake has
    /// passed, and the first wake waits for nothing; inside the window it is
    /// held, folded onto what was held before. A wake teaches the window the
    /// stream's pace.
    fn tick(&mut self, fresh: &Tally<'_>) -> Option<Wake> {
        let notice = Notice::folded(self.held.take(), fresh)?;
        let since = self.last_wake.unwrap_or(self.started).elapsed();
        if self.last_wake.is_some() && since < self.quiet.window() {
            self.held = Some(notice);
            return None;
        }
        let since_last = self.last_wake.map(|_| since);
        self.last_wake = Some(Instant::now());
        self.quiet.observe(notice.more + 1, since);
        let cadence = Cadence {
            since: since_last,
            window: self.quiet.window(),
        };
        Some(Wake::paced(notice, cadence))
    }

    /// What the job's end still has to say: a count no wake carried.
    fn remainder(&mut self) -> Option<Notice> {
        self.held.take()
    }
}

impl<'a> Scan<'a> {
    fn new(conditions: &'a Conditions) -> Self {
        let mode = if conditions.ongoing() {
            let quiet = conditions
                .quiet()
                .map_or(Quiet::Adaptive(Duration::ZERO), Quiet::Pinned);
            Mode::All(Ongoing::new(quiet))
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
    async fn look(&mut self, log: &Path) -> Option<Wake> {
        let text = self.read(log).await?;
        let conditions = self.conditions;
        match &mut self.mode {
            Mode::Once { fired } => first(conditions, &text, fired).map(Wake::once),
            Mode::All(ongoing) => ongoing.tick(&conditions.tally(&text)),
        }
    }

    /// What is left to say now the job has ended. The completion is going out
    /// regardless, so the quiet window holds nothing back — this is the one
    /// thing a count with no hit behind it ever rides (ADR-0018 §8).
    async fn last_look(&mut self, log: &Path) -> Option<Notice> {
        let last = self.look(log).await.map(|wake| wake.notice);
        match &mut self.mode {
            Mode::Once { .. } => last,
            Mode::All(ongoing) => last.or_else(|| ongoing.remainder()),
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
        Conditions::new(
            on.iter().map(|s| (*s).to_string()).collect(),
            None,
            false,
            None,
        )
        .expect("the conditions compile")
    }

    /// The same words, watched for the whole of the job (`notify_all`).
    fn ongoing(on: &[&str]) -> Conditions {
        Conditions::new(
            on.iter().map(|s| (*s).to_string()).collect(),
            None,
            true,
            None,
        )
        .expect("the conditions compile")
    }

    /// The same, with the window pinned by the call (`notify_quiet`).
    fn pinned(on: &[&str], quiet: Duration) -> Conditions {
        let millis = u64::try_from(quiet.as_millis()).expect("a short window");
        Conditions::new(
            on.iter().map(|s| (*s).to_string()).collect(),
            None,
            true,
            Some(millis),
        )
        .expect("the conditions compile")
    }

    /// A log to write into, and the path a scan reads it back from.
    async fn writing() -> (tempfile::TempDir, Log, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let log = Log::create(dir.path(), "job").await.expect("a log");
        let path = log.path().to_path_buf();
        (dir, log, path)
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
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
            Some(Wake::once(Notice::of("test result: FAILED")))
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

    // ---- the window a stream buys (ADR-0018 §8) ---------------------------

    /// The brick: one line is not a rate; past that, each line per second
    /// that arrived while the watch was quiet buys two seconds, to a cap.
    #[test]
    fn a_wake_buys_quiet_in_proportion_to_the_lines_it_folded() {
        for (lines, since, want) in [
            (0, secs(10), Duration::ZERO),
            (1, Duration::ZERO, Duration::ZERO),
            (1, secs(600), Duration::ZERO),
            (2, secs(2), secs(1)),
            (3, secs(1), secs(4)),
            (11, secs(10), secs(2)),
            (2, Duration::ZERO, secs(8)),
            (2, Duration::from_millis(10), secs(8)),
            (4, SCAN, secs(24)),
            (1000, secs(1), CAP),
            (usize::MAX, secs(1), CAP),
        ] {
            assert_eq!(
                next_window(lines, since),
                want,
                "{lines} lines in {since:?}"
            );
        }
    }

    /// A stream that is slower than the window it earns is real time: every
    /// line wakes the moment it is read, however long the job runs.
    #[tokio::test(start_paused = true)]
    async fn a_slow_stream_wakes_on_every_line() {
        let (_dir, mut log, path) = writing().await;
        let watched = ongoing(&["HIT"]);
        let mut scan = Scan::new(&watched);

        log.write("warming\nHIT one\n").await.expect("written");
        let first = scan.look(&path).await.expect("the first hit wakes");
        assert_eq!(first.notice, Notice::of("HIT one"));
        assert_eq!(
            first.cadence,
            Some(Cadence {
                since: None,
                window: Duration::ZERO
            }),
            "a first notice has no last one, and one line buys no quiet"
        );

        for (n, line) in ["HIT two", "HIT three", "HIT four"].iter().enumerate() {
            tokio::time::advance(secs(10)).await;
            log.write(&format!("{line}\n")).await.expect("written");
            let wake = scan.look(&path).await.expect("every line wakes");
            assert_eq!(wake.notice, Notice::of(line), "line {n}");
            assert_eq!(
                wake.cadence,
                Some(Cadence {
                    since: Some(secs(10)),
                    window: Duration::ZERO
                })
            );
        }
    }

    /// A burst buys quiet: the wake that folded it names the window, the
    /// lines inside the window are held, and the window's end lets them out
    /// — a held line never waits for the next hit.
    #[tokio::test(start_paused = true)]
    async fn a_burst_buys_quiet_and_the_window_ending_flushes_what_it_held() {
        let (_dir, mut log, path) = writing().await;
        let watched = ongoing(&["HIT"]);
        let mut scan = Scan::new(&watched);

        log.write("HIT one\nHIT two\nHIT three\n").await.expect("w");
        let burst = scan.look(&path).await.expect("the first look wakes");
        assert_eq!(
            burst.notice,
            Notice {
                line: "HIT three".into(),
                more: 2
            }
        );
        // Two extra lines in no time at all read as two in one tick: eight
        // a second, sixteen seconds of quiet.
        assert_eq!(
            burst.cadence,
            Some(Cadence {
                since: None,
                window: secs(16)
            })
        );

        tokio::time::advance(secs(5)).await;
        log.write("HIT four\n").await.expect("written");
        assert_eq!(scan.look(&path).await, None, "inside the window, held");
        tokio::time::advance(secs(5)).await;
        assert_eq!(scan.look(&path).await, None, "still inside, nothing new");

        tokio::time::advance(secs(6)).await;
        let flushed = scan.look(&path).await.expect("the window's end flushes");
        assert_eq!(flushed.notice, Notice::of("HIT four"));
        assert_eq!(
            flushed.cadence,
            Some(Cadence {
                since: Some(secs(16)),
                window: Duration::ZERO
            }),
            "one line in sixteen seconds is a slow stream again"
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
        let wake = scan.look(&path).await.expect("a wake");
        assert_eq!(wake.notice.more, 1);
        let window = wake.cadence.expect("paced").window;
        assert_eq!(window, secs(8));

        log.write("HIT three\nHIT four\n").await.expect("written");
        tokio::time::advance(secs(1)).await;
        assert_eq!(scan.look(&path).await, None, "held by the window");

        tokio::time::advance(window).await;
        log.write("HIT five\n").await.expect("written");
        let carried = scan.look(&path).await.expect("past the window");
        assert_eq!(
            carried.notice,
            Notice {
                line: "HIT five".into(),
                more: 2
            }
        );
        let window = carried.cadence.expect("paced").window;
        assert!(
            window > Duration::ZERO,
            "three lines in nine seconds is a rate"
        );

        tokio::time::advance(window).await;
        log.write("HIT six\n").await.expect("written");
        let next = scan.look(&path).await.expect("past the window again");
        assert_eq!(
            next.notice,
            Notice::of("HIT six"),
            "the count went with the notice that carried it"
        );
    }

    /// `notify_quiet`: the window is the call's, whatever the stream does —
    /// a burst is held for the whole of it, and the window's end still
    /// flushes.
    #[tokio::test(start_paused = true)]
    async fn a_pinned_window_holds_a_burst_and_flushes_at_its_end() {
        let (_dir, mut log, path) = writing().await;
        let watched = pinned(&["HIT"], secs(30));
        let mut scan = Scan::new(&watched);

        log.write("HIT one\n").await.expect("written");
        let first = scan.look(&path).await.expect("the first hit wakes");
        assert_eq!(first.cadence.expect("paced").window, secs(30));

        tokio::time::advance(secs(10)).await;
        log.write("HIT two\nHIT three\n").await.expect("written");
        assert_eq!(scan.look(&path).await, None, "held by the pinned window");
        tokio::time::advance(secs(10)).await;
        log.write("HIT four\n").await.expect("written");
        assert_eq!(scan.look(&path).await, None, "still held");

        tokio::time::advance(secs(10)).await;
        let flushed = scan.look(&path).await.expect("the window's end flushes");
        assert_eq!(
            flushed.notice,
            Notice {
                line: "HIT four".into(),
                more: 2
            }
        );
        assert_eq!(
            flushed.cadence.expect("paced").window,
            secs(30),
            "a pinned window learns nothing from the stream"
        );
    }

    /// What no wake came for rides the end of the job, and only that.
    #[tokio::test(start_paused = true)]
    async fn what_the_window_held_rides_the_end_of_the_job() {
        let (_dir, mut log, path) = writing().await;
        let watched = pinned(&["HIT"], secs(30));
        let mut scan = Scan::new(&watched);

        log.write("HIT one\n").await.expect("written");
        assert!(scan.look(&path).await.is_some(), "the first hit wakes");
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

    /// Unix only: the raw wait status this reads is a POSIX encoding, and
    /// Windows has no signal to be killed by in the first place.
    #[cfg(unix)]
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

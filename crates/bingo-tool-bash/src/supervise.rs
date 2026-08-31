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

use crate::jobs::{Job, Jobs, State};
use crate::notify::{self, Conditions};
use crate::run::{self, Running};
use crate::sink::Sink;

/// How long a `SIGTERM`ed group has to leave on its own terms.
const GRACE: Duration = Duration::from_secs(2);

/// How often a watched job's new output is read for a condition.
const SCAN: Duration = Duration::from_millis(250);

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
    let hit = scan.look(&job.log).await;
    job.finished(state);
    jobs.publish(&host, &job.session).await;
    announce(
        &host,
        &job,
        &running.sink,
        notify::finished(&job, state, hit.as_deref()),
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
                if let Some(line) = scan.look(&job.log).await {
                    announce(host, job, sink, notify::matched(job, &line)).await;
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

/// The reading of a job's log that looks for a condition. It fires once: a
/// pattern that matches every line must not wake a session every line.
struct Scan<'a> {
    conditions: &'a Conditions,
    cursor: u64,
    fired: bool,
}

impl<'a> Scan<'a> {
    fn new(conditions: &'a Conditions) -> Self {
        Self {
            conditions,
            cursor: 0,
            fired: false,
        }
    }

    /// The first line of what is new that answers a condition, and `None`
    /// forever after it has answered once.
    async fn look(&mut self, log: &Path) -> Option<String> {
        if self.fired || !self.conditions.watched() {
            return None;
        }
        let window = crate::log::window(log, self.cursor, WINDOW).await.ok()?;
        self.cursor = window.cursor;
        let hit = self.conditions.hit(&window.text)?.to_string();
        self.fired = true;
        Some(hit)
    }
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
        Conditions::new(on.iter().map(|s| (*s).to_string()).collect(), None)
            .expect("the conditions compile")
    }

    #[tokio::test]
    async fn a_scan_reads_on_from_where_it_stopped_and_fires_once() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut log = Log::create(dir.path(), "job").await.expect("a log");
        let path = log.path().to_path_buf();
        let watched = conditions(&["FAILED"]);
        let mut scan = Scan::new(&watched);

        log.write("running\n").await.expect("written");
        assert_eq!(scan.look(&path).await, None, "nothing has matched yet");
        log.write("test result: FAILED\n").await.expect("written");
        assert_eq!(
            scan.look(&path).await.as_deref(),
            Some("test result: FAILED")
        );
        log.write("test result: FAILED again\n").await.expect("w");
        assert_eq!(
            scan.look(&path).await,
            None,
            "one notification, not a storm"
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

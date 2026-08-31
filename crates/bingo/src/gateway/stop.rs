//! `gateway stop`: ask the resident process to leave, and wait until it has.
//!
//! TERM and not KILL, because the whole point of the graceful end (ADR-0020
//! §4) is what happens between the two: the surfaces stop, `Plugin::stop` runs,
//! the schedule runner claim and the channel locks are given back, and the
//! pidfile goes with the last `Drop`. A killed gateway skips every one of those
//! and leaves a data dir that looks occupied by a process that is gone.
//!
//! The wait is for the pidfile to disappear rather than for the process, since
//! the file going is the proof that the `Drop`s ran.

use std::path::Path;
use std::time::{Duration, Instant};

use bingo_sdk::{ErrorCode, KernelError};

use super::paths::Paths;
use super::pidfile::Record;
use super::probe::{self, Probe};
use super::service::{self, Ask, Mode};
use super::state::State;

/// How long a stop waits for the gateway to finish leaving.
pub const PATIENCE: Duration = Duration::from_secs(30);

const GLANCE: Duration = Duration::from_millis(50);

pub async fn stop(paths: &Paths, home: &Path, probe: &dyn Probe) -> Result<String, KernelError> {
    // While a supervisor holds the service it is the authority on whether one
    // is running, not our pidfile: an unload has to be said even when there is
    // no record here, or `stop` would quietly leave a service loaded and
    // `KeepAlive` would put it straight back (ADR-0020 §7).
    if let Mode::Installed(supervisor) = Mode::here(home) {
        return unloaded(supervisor, home, paths, probe).await;
    }
    match State::read(paths, probe)? {
        State::Stopped => Ok(format!(
            "No gateway is running here: there is no {}.",
            paths.pidfile().display()
        )),
        State::Stale(record) => clear(paths, &record),
        State::Running(record) => {
            signal(&record, probe)?;
            gone(paths, &record, probe).await
        }
    }
}

/// Tell the supervisor to let go, then wait for whatever it was holding.
///
/// The pidfile may never have existed — a service that was loaded but not yet
/// running has none — so its absence afterwards is the success, not a record
/// having been there first.
async fn unloaded(
    supervisor: super::unit::Supervisor,
    home: &Path,
    paths: &Paths,
    probe: &dyn Probe,
) -> Result<String, KernelError> {
    let running = State::read(paths, probe)?.running().cloned();
    service::tell(
        supervisor,
        Ask::Stop,
        &service::uid(),
        &supervisor.path(home),
    )
    .map_err(|e| KernelError::new(ErrorCode::Internal, e))?;
    let Some(record) = running else {
        return Ok(format!(
            "{} was told to stop the gateway. Nothing was running here.",
            supervisor.name()
        ));
    };
    gone(paths, &record, probe).await
}

/// A record whose process is gone: the file is the only thing left to stop,
/// and leaving it would make the next `start` refuse for no reason.
fn clear(paths: &Paths, record: &Record) -> Result<String, KernelError> {
    let path = paths.pidfile();
    std::fs::remove_file(&path)
        .map_err(|e| KernelError::new(ErrorCode::Internal, format!("{}: {e}", path.display())))?;
    Ok(format!(
        "Nothing was running: pid {} is gone and did not stop cleanly. \
         Its record ({}) has been cleared.\n\
         Run `bingo gateway doctor` — a gateway that was killed leaves its \
         other locks behind too.",
        record.pid,
        path.display()
    ))
}

/// TERM, from whoever is entitled to send it.
///
/// A pid is reused the moment the number comes round again, so a pid that is
/// alive but is not a bingo is never signalled: the alternative is `stop`
/// killing a stranger's process because a stale file named its number
/// (M17 R-liveness).
fn signal(record: &Record, probe: &dyn Probe) -> Result<(), KernelError> {
    if !probe::is_bingo(probe, record.pid) {
        return Err(KernelError::new(
            ErrorCode::InvalidInput,
            format!(
                "pid {} is alive but is not a bingo — the number came round \
                 again. Nothing was signalled; remove the pidfile by hand, or \
                 run `bingo gateway doctor --fix`.",
                record.pid
            ),
        ));
    }
    probe
        .terminate(record.pid)
        .map_err(|e| KernelError::new(ErrorCode::Internal, e))
}

/// Wait for the pidfile to go, which is what says the plugins were stopped
/// rather than the process merely killed.
async fn gone(paths: &Paths, record: &Record, probe: &dyn Probe) -> Result<String, KernelError> {
    let started = Instant::now();
    loop {
        if matches!(State::read(paths, probe)?, State::Stopped) {
            return Ok(format!(
                "The gateway has stopped: pid {} gave back its pidfile and its locks.",
                record.pid
            ));
        }
        if started.elapsed() > PATIENCE {
            return Err(KernelError::new(
                ErrorCode::Internal,
                format!(
                    "pid {} was asked to stop {PATIENCE:?} ago and has not. \
                     Nothing was killed — look at {} and stop it by hand if it \
                     is wedged.",
                    record.pid,
                    paths.log().display()
                ),
            ));
        }
        tokio::time::sleep(GLANCE).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::pidfile;
    use crate::gateway::probe::tests::Fake;
    use jiff::Timestamp;

    fn record(pid: u32) -> Record {
        Record {
            pid,
            version: "0.1.0".into(),
            started: Timestamp::now(),
        }
    }

    fn paths_with(home: &Path, pid: Option<u32>) -> Paths {
        let paths = Paths::new(&bingo_sdk::Env::rooted(home));
        paths.ensure().expect("the directory");
        if let Some(pid) = pid {
            std::fs::write(paths.pidfile(), pidfile::render(&record(pid))).expect("a record");
        }
        paths
    }

    #[tokio::test]
    async fn stopping_what_never_started_says_so_and_is_not_a_failure() {
        let home = tempfile::tempdir().expect("a temporary home");
        let paths = paths_with(home.path(), None);
        let said = stop(&paths, home.path(), &Fake::empty())
            .await
            .expect("an idempotent stop");
        assert!(said.contains("No gateway is running here"), "{said}");
    }

    #[tokio::test]
    async fn a_stale_record_is_cleared_and_the_person_is_pointed_at_doctor() {
        let home = tempfile::tempdir().expect("a temporary home");
        let paths = paths_with(home.path(), Some(4242));
        let said = stop(&paths, home.path(), &Fake::empty())
            .await
            .expect("the record is cleared");
        assert!(said.contains("pid 4242 is gone"), "{said}");
        assert!(said.contains("doctor"), "{said}");
        assert!(!paths.pidfile().exists(), "the record went with it");
    }

    #[tokio::test]
    async fn a_live_pid_that_is_not_a_bingo_is_never_signalled() {
        let home = tempfile::tempdir().expect("a temporary home");
        let paths = paths_with(home.path(), Some(4242));
        let table = Fake::of(&[(4242, "postgres")]);
        let refused = stop(&paths, home.path(), &table)
            .await
            .expect_err("a refusal")
            .message;
        assert!(refused.contains("came round again"), "{refused}");
        assert!(table.signals().is_empty(), "nothing was signalled");
        assert!(paths.pidfile().exists(), "and nothing was removed");
    }
}

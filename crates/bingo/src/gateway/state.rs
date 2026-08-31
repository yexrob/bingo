//! What the pidfile says, and whether it is still true.
//!
//! A record on disk and a process that is running are two different facts, and
//! every verb needs both at once: `start` must not launch a second gateway,
//! `stop` must not signal a stranger, `doctor` must tell a person which of the
//! two is missing. Reading them together, once, is what keeps the three verbs
//! from each deciding it differently.

use bingo_sdk::{ErrorCode, KernelError};

use super::paths::Paths;
use super::pidfile::{self, Record};
use super::probe::Probe;

/// The gateway of one data dir, as the pidfile and the process table together
/// describe it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    /// A record, and the process it names is there.
    Running(Record),
    /// A record whose process is gone: a gateway that did not stop cleanly.
    Stale(Record),
    /// No record at all, which is what a clean stop leaves.
    Stopped,
}

impl State {
    pub fn read(paths: &Paths, probe: &dyn Probe) -> Result<Self, KernelError> {
        let record = pidfile::read(&paths.pidfile())
            .map_err(|e| KernelError::new(ErrorCode::Internal, e))?;
        Ok(match record {
            Some(record) if probe.alive(record.pid) => State::Running(record),
            Some(record) => State::Stale(record),
            None => State::Stopped,
        })
    }

    /// The record of a gateway that is actually running.
    pub fn running(&self) -> Option<&Record> {
        match self {
            State::Running(record) => Some(record),
            State::Stale(_) | State::Stopped => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::probe::tests::Fake;
    use jiff::Timestamp;

    fn written(home: &std::path::Path, pid: u32) -> Paths {
        let paths = Paths::new(&bingo_sdk::Env::rooted(home));
        paths.ensure().expect("the directory");
        let record = Record {
            pid,
            version: "0.1.0".into(),
            started: Timestamp::now(),
        };
        std::fs::write(paths.pidfile(), pidfile::render(&record)).expect("a record");
        paths
    }

    #[test]
    fn no_record_is_stopped_a_live_one_is_running_a_dead_one_is_stale() {
        let home = tempfile::tempdir().expect("a temporary home");
        let paths = Paths::new(&bingo_sdk::Env::rooted(home.path()));
        let table = Fake::of(&[(4242, "bingo")]);
        assert_eq!(
            State::read(&paths, &table).expect("readable"),
            State::Stopped
        );

        let paths = written(home.path(), 4242);
        let state = State::read(&paths, &table).expect("readable");
        assert_eq!(state.running().map(|r| r.pid), Some(4242));
        assert!(matches!(state, State::Running(_)));

        let paths = written(home.path(), 9999);
        let state = State::read(&paths, &table).expect("readable");
        let State::Stale(stale) = &state else {
            panic!("a record whose process is gone is stale, not {state:?}")
        };
        assert_eq!(state.running(), None, "a record is not a process");
        assert_eq!(
            stale.pid, 9999,
            "but it is still what a person must be told about"
        );
    }
}

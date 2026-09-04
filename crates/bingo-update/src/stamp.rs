//! When this machine last asked, and what it was told.
//!
//! One file — `<data_dir>/update.json` — holding one fact each. It is written
//! *before* the request rather than after it, so a machine whose answer never
//! comes still waits a day before asking again: the API allows sixty
//! unauthenticated requests an hour per address, and a stamp that only landed
//! on success would ask on every start of every failing day.

use std::path::{Path, PathBuf};
use std::time::Duration;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// How long a machine waits before asking again.
pub const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// The file's name under the data directory.
const FILE: &str = "update.json";

/// What the last check knew.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// When the request was made — not when it came back.
    pub checked_at: Timestamp,
    /// The newest release the answer named, whatever this build is.
    #[serde(default)]
    pub latest: Option<String>,
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(FILE)
}

/// The stamp, or nothing at all: a file that is not there, cannot be read or
/// does not parse is a machine that has never asked.
pub fn read(data_dir: &Path) -> Option<Stamp> {
    let text = std::fs::read_to_string(path(data_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Write it, or say why not. Nobody stops for this: a data directory that
/// cannot be written costs a check, not a start.
pub fn write(data_dir: &Path, stamp: &Stamp) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(data_dir)?;
    let json = serde_json::to_string(stamp).map_err(std::io::Error::other)?;
    std::fs::write(path(data_dir), json)
}

/// Whether to ask again. A stamp from the future is a clock that moved, and a
/// machine that would otherwise never ask again.
pub fn due(stamp: Option<&Stamp>, now: Timestamp, every: Duration) -> bool {
    let Some(stamp) = stamp else {
        return true;
    };
    let since = now.as_second() - stamp.checked_at.as_second();
    since < 0 || since >= every.as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(second: i64) -> Timestamp {
        Timestamp::from_second(second).expect("a timestamp")
    }

    fn stamp(second: i64, latest: Option<&str>) -> Stamp {
        Stamp {
            checked_at: at(second),
            latest: latest.map(str::to_string),
        }
    }

    #[test]
    fn a_machine_that_has_never_asked_is_due() {
        assert!(due(None, at(0), EVERY));
        let dir = tempfile::tempdir().expect("a directory");
        assert_eq!(read(dir.path()), None);
    }

    #[test]
    fn a_stamp_younger_than_the_interval_is_not_due_and_one_older_is() {
        let asked = stamp(1_000_000, Some("0.5.0"));
        let day = EVERY.as_secs() as i64;
        assert!(!due(Some(&asked), at(1_000_000), EVERY));
        assert!(!due(Some(&asked), at(1_000_000 + day - 1), EVERY));
        assert!(due(Some(&asked), at(1_000_000 + day), EVERY));
    }

    #[test]
    fn a_stamp_from_the_future_is_due_rather_than_forever() {
        let asked = stamp(2_000_000, None);
        assert!(due(Some(&asked), at(1_000_000), EVERY));
    }

    #[test]
    fn what_was_written_is_what_is_read_back() {
        let dir = tempfile::tempdir().expect("a directory");
        let written = stamp(1_700_000_000, Some("0.5.0"));
        write(dir.path(), &written).expect("the stamp is written");
        assert_eq!(read(dir.path()), Some(written));
        assert!(path(dir.path()).ends_with("update.json"));
    }

    #[test]
    fn a_stamp_that_is_not_a_stamp_reads_as_none() {
        let dir = tempfile::tempdir().expect("a directory");
        std::fs::write(path(dir.path()), "{ not json").expect("a file");
        assert_eq!(read(dir.path()), None, "a broken stamp asks again");
    }

    #[test]
    fn a_stamp_written_before_the_answer_carries_no_version() {
        let dir = tempfile::tempdir().expect("a directory");
        write(dir.path(), &stamp(1, None)).expect("the stamp is written");
        assert_eq!(read(dir.path()).and_then(|s| s.latest), None);
    }
}

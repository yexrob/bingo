//! `<data_dir>/gateway/gateway.pid` (ADR-0020 §3).
//!
//! The record says which process is the gateway, what binary it is running and
//! when it started. It is written with `create_new` — the shape the channels
//! and schedule claims already use — and given back on drop, so an end that
//! ran its `Drop`s leaves nothing behind.
//!
//! A record is proof that a process took the gateway, never proof that one
//! still runs: a bingo that was killed leaves its file, so every verb that
//! reads one asks the process table before believing it.

use std::io::Write;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// What the resident process wrote down about itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Record {
    pub pid: u32,
    /// The binary that is running, so `status` can say when it is older than
    /// the one that would be started now.
    pub version: String,
    pub started: Timestamp,
}

impl Record {
    /// This process, as of `started`.
    pub fn here(started: Timestamp) -> Self {
        Self {
            pid: std::process::id(),
            version: version().to_string(),
            started,
        }
    }
}

/// The binary running this code, which `status` and `doctor` compare against
/// the record a resident process left.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// A record as it is written: JSON, because a person reads this file when
/// something has gone wrong and a bare number would answer none of it.
pub fn render(record: &Record) -> String {
    // A record is three owned scalars; the only way this fails is a bug, and
    // an empty pidfile would be worse than the fallback line.
    match serde_json::to_string_pretty(record) {
        Ok(json) => format!("{json}\n"),
        Err(e) => format!("{{\"pid\":{},\"error\":\"{e}\"}}\n", record.pid),
    }
}

pub fn parse(text: &str) -> Result<Record, String> {
    serde_json::from_str(text).map_err(|e| format!("{e}"))
}

/// The record in `path`; `None` when no gateway ever wrote one there.
pub fn read(path: &Path) -> Result<Option<Record>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    parse(&text)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The pidfile this process holds, given back when the value drops.
#[derive(Debug)]
pub struct Claim {
    path: PathBuf,
}

impl Claim {
    /// Take the pidfile, or say who got there first. `create_new` is the whole
    /// exclusion: two gateways racing for one data dir, only one file.
    pub fn take(path: &Path, record: &Record) -> Result<Self, String> {
        let mut file =
            std::fs::File::create_new(path).map_err(|e| format!("{}: {e}", path.display()))?;
        file.write_all(render(record).as_bytes())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> Record {
        Record {
            pid: 4242,
            version: "0.1.0".into(),
            started: "2026-08-31T09:00:00Z".parse().expect("a timestamp"),
        }
    }

    #[test]
    fn a_record_survives_the_round_trip_through_the_file_it_is_written_as() {
        let rendered = render(&record());
        assert_eq!(parse(&rendered).expect("it parses"), record());
        assert!(rendered.ends_with('\n'), "{rendered}");
        assert!(rendered.contains("\"pid\": 4242"), "{rendered}");
        assert!(
            rendered.contains("2026-08-31T09:00:00Z"),
            "the start time is readable, not a count of seconds: {rendered}"
        );
    }

    #[test]
    fn a_file_that_is_not_a_record_is_an_error_naming_the_path() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("gateway.pid");
        assert_eq!(read(&path).expect("no file is no record"), None);
        std::fs::write(&path, "4242").expect("a bare pid, as an older tree wrote");
        let refused = read(&path).expect_err("a refusal");
        assert!(refused.contains("gateway.pid"), "{refused}");
    }

    #[test]
    fn the_second_claim_on_one_data_dir_is_refused_and_the_first_is_intact() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("gateway.pid");
        let _first = Claim::take(&path, &record()).expect("the first claim");
        let second = Record {
            pid: 9999,
            ..record()
        };
        let refused = Claim::take(&path, &second).expect_err("the second is refused");
        assert!(refused.contains("gateway.pid"), "{refused}");
        assert_eq!(
            read(&path).expect("readable").expect("a record").pid,
            4242,
            "the loser did not overwrite the winner"
        );
    }

    #[test]
    fn a_claim_is_given_back_when_it_is_dropped() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("gateway.pid");
        {
            let _claim = Claim::take(&path, &record()).expect("a claim");
            assert!(path.exists());
        }
        assert!(!path.exists(), "a graceful end leaves no record behind");
        Claim::take(&path, &record()).expect("the next gateway may have it");
    }

    #[test]
    fn the_record_this_process_writes_names_this_process_and_this_binary() {
        let now = Timestamp::now();
        let here = Record::here(now);
        assert_eq!(here.pid, std::process::id());
        assert_eq!(here.version, version());
        assert_eq!(here.started, now);
    }
}

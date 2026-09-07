//! `<data_dir>/gateway/gateway.log`: the first place in this tree where a
//! `warn!` lands (ADR-0020 §6, the M16 carried item).
//!
//! Until now every crate's `tracing` call went to a process with no
//! subscriber, which is to say nowhere. The resident gateway is the one
//! process that outlives the terminal it was started from, so it is the one
//! that owes an account of itself.
//!
//! The sink is `tracing-subscriber`'s `fmt` and nothing else: no `ansi`, since
//! a file wants no escape codes, and no `env-filter`, whose regex stack costs
//! more than the filtering is worth here.

use std::fs::File;
use std::path::Path;

use tracing::Level;

/// How large the log may get before the next `run` rolls it aside. Rotation
/// beyond this is a non-goal (M17): one file of history is what a person
/// reads, and an unbounded one is what fills a disk while nobody is looking.
pub const CAP: u64 = 8 * 1024 * 1024;

/// The previous log, kept across exactly one roll.
const PREVIOUS: &str = "gateway.log.1";

/// The log, ready to be appended to, rolled aside first if it had grown past
/// the cap. Appending is what makes two processes' lines interleave rather
/// than truncate each other, which matters the moment a `start` races a
/// supervisor's respawn.
pub fn open(path: &Path) -> Result<File, String> {
    roll(path)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Move the log aside when it is over the cap. A log that is not there, or is
/// small, is left exactly as it was.
fn roll(path: &Path) -> Result<(), String> {
    let Ok(metadata) = std::fs::metadata(path) else {
        return Ok(());
    };
    if metadata.len() <= CAP {
        return Ok(());
    }
    let Some(previous) = path.parent().map(|dir| dir.join(PREVIOUS)) else {
        return Ok(());
    };
    std::fs::rename(path, &previous).map_err(|e| format!("{}: {e}", previous.display()))
}

/// Send every `info!` and worse from this process into `file`, for the rest of
/// the process's life.
///
/// This is process-wide and set once; a second call is the caller's bug, and
/// it is reported rather than ignored so that bug is not the invisible kind.
pub fn install(file: File) -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_writer(file)
        .with_ansi(false)
        .with_max_level(Level::INFO)
        .with_target(true)
        .try_init()
        .map_err(|e| format!("the log sink is already installed: {e}"))
}

/// The last `lines` lines of `text`, for `logs` and for `status`.
///
/// A pure brick: the file is read by the caller, so a test says what a tail is
/// without a file existing.
pub fn tail(text: &str, lines: usize) -> &str {
    if lines == 0 {
        return "";
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    let start = body
        .rmatch_indices('\n')
        .nth(lines - 1)
        .map(|(at, _)| at + 1)
        .unwrap_or(0);
    &text[start..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tail_is_the_last_lines_and_never_more_than_there_are() {
        let log = "one\ntwo\nthree\nfour\n";
        assert_eq!(tail(log, 2), "three\nfour\n");
        assert_eq!(tail(log, 1), "four\n");
        assert_eq!(tail(log, 4), log);
        assert_eq!(tail(log, 99), log, "a short log is its own tail");
        assert_eq!(tail(log, 0), "");
    }

    #[test]
    fn a_tail_of_a_log_with_no_final_newline_keeps_the_last_line() {
        assert_eq!(tail("one\ntwo", 1), "two");
        assert_eq!(tail("only", 3), "only");
        assert_eq!(tail("", 3), "");
    }

    #[test]
    fn a_log_under_the_cap_is_appended_to_and_an_oversized_one_is_rolled_aside() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("gateway.log");
        std::fs::write(&path, "the first run\n").expect("a log");

        drop(open(&path).expect("it opens"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("still there"),
            "the first run\n",
            "a small log is not touched, and opening appends"
        );

        std::fs::write(&path, vec![b'x'; usize::try_from(CAP).expect("fits") + 1])
            .expect("an oversized log");
        drop(open(&path).expect("it opens"));
        assert_eq!(
            std::fs::read_to_string(&path).expect("a fresh log"),
            "",
            "the oversized log was moved out of the way"
        );
        assert!(
            dir.path().join(PREVIOUS).exists(),
            "and kept, because it is the history of what went wrong"
        );
    }

    #[test]
    fn a_log_that_was_never_written_opens_without_a_roll() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let path = dir.path().join("gateway.log");
        drop(open(&path).expect("it opens"));
        assert!(path.exists());
        assert!(!dir.path().join(PREVIOUS).exists());
    }
}

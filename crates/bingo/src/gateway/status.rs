//! `gateway status` and `gateway logs`: what is running, and what it said.
//!
//! Both stay at the level the process itself can answer for. Which channels
//! are connected *right now* needs a control socket the gateway answers on,
//! and there is none (ADR-0020 consequences) — so nothing here pretends to
//! know it. Every line names the file it was read from, because the next thing
//! a person does is go and look at that file.

use std::path::Path;

use bingo_sdk::KernelError;
use jiff::{SignedDuration, Timestamp};

use super::paths::Paths;
use super::pidfile;
use super::probe::Probe;
use super::service::Mode;
use super::state::State;

/// How much of the log `status` shows. `logs` shows more.
const GLIMPSE: usize = 5;

/// The default for `gateway logs`.
pub const LINES: usize = 40;

pub fn status(paths: &Paths, home: &Path, probe: &dyn Probe) -> Result<String, KernelError> {
    let state = State::read(paths, probe)?;
    let mut lines = vec![headline(&state, Timestamp::now())];
    lines.push(Mode::here(home).line(home));
    lines.push(format!("pidfile: {}", paths.pidfile().display()));
    lines.push(format!("log: {}", paths.log().display()));
    if let Some(tail) = glimpse(paths, GLIMPSE) {
        lines.push(tail);
    }
    Ok(lines.join("\n"))
}

/// The one line that says whether a gateway is up, and as what.
fn headline(state: &State, now: Timestamp) -> String {
    match state {
        State::Running(record) => {
            let version = version_line(&record.version);
            format!(
                "running: pid {}, bingo {}, up {}{version}",
                record.pid,
                record.version,
                uptime(record.started, now)
            )
        }
        State::Stale(record) => format!(
            "not running: the pidfile names pid {}, which is gone. \
             It did not stop cleanly — `bingo gateway doctor --fix`.",
            record.pid
        ),
        State::Stopped => "not running: no pidfile.".into(),
    }
}

/// A running gateway older than the binary on disk is worth saying, because a
/// person who just rebuilt will wonder why nothing changed.
fn version_line(running: &str) -> String {
    match running == pidfile::version() {
        true => String::new(),
        false => format!(
            " (the binary here is {} — restart to pick it up)",
            pidfile::version()
        ),
    }
}

/// The last few lines of the log, when there are any.
fn glimpse(paths: &Paths, lines: usize) -> Option<String> {
    let text = std::fs::read_to_string(paths.log()).ok()?;
    let tail = super::log::tail(&text, lines);
    match tail.trim().is_empty() {
        true => None,
        false => Some(format!("last {lines} log lines:\n{}", tail.trim_end())),
    }
}

pub fn logs(paths: &Paths, lines: usize) -> Result<String, KernelError> {
    let path = paths.log();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(format!(
            "{} does not exist yet: no gateway has run in this data dir.",
            path.display()
        ));
    };
    let tail = super::log::tail(&text, lines);
    match tail.trim().is_empty() {
        true => Ok(format!("{} is empty.", path.display())),
        false => Ok(format!("{}\n{}", path.display(), tail.trim_end())),
    }
}

/// How long, in the two largest units that say anything. A gateway is a thing
/// that runs for weeks, so seconds stop mattering after the first minute.
pub fn uptime(since: Timestamp, now: Timestamp) -> String {
    let elapsed = now.duration_since(since);
    if elapsed < SignedDuration::ZERO {
        return "no time at all (its start time is in the future)".into();
    }
    let seconds = elapsed.as_secs();
    let (days, hours) = (seconds / 86_400, (seconds % 86_400) / 3_600);
    let (minutes, rest) = ((seconds % 3_600) / 60, seconds % 60);
    match (days, hours, minutes) {
        (0, 0, 0) => format!("{rest}s"),
        (0, 0, _) => format!("{minutes}m {rest}s"),
        (0, _, _) => format!("{hours}h {minutes}m"),
        _ => format!("{days}d {hours}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::pidfile::Record;
    use crate::gateway::probe::tests::Fake;

    fn at(text: &str) -> Timestamp {
        text.parse().expect("a timestamp")
    }

    fn record(pid: u32, version: &str) -> Record {
        Record {
            pid,
            version: version.into(),
            started: at("2026-08-31T09:00:00Z"),
        }
    }

    #[test]
    fn uptime_says_the_two_units_that_matter_and_stops() {
        let start = at("2026-08-31T09:00:00Z");
        let after = |d: SignedDuration| uptime(start, start + d);
        assert_eq!(after(SignedDuration::from_secs(9)), "9s");
        assert_eq!(after(SignedDuration::from_secs(70)), "1m 10s");
        assert_eq!(after(SignedDuration::from_mins(150)), "2h 30m");
        assert_eq!(after(SignedDuration::from_hours(50)), "2d 2h");
        assert_eq!(
            uptime(start, start - SignedDuration::from_hours(1)),
            "no time at all (its start time is in the future)",
            "a clock that went backwards is said, not rendered as nonsense"
        );
    }

    #[test]
    fn the_headline_says_running_stale_or_stopped() {
        let now = at("2026-08-31T11:30:00Z");
        let running = headline(&State::Running(record(4242, pidfile::version())), now);
        assert!(running.starts_with("running: pid 4242"), "{running}");
        assert!(running.contains("up 2h 30m"), "{running}");
        assert!(
            !running.contains("restart to pick it up"),
            "the running binary is this one: {running}"
        );

        let stale = headline(&State::Stale(record(4242, "0.1.0")), now);
        assert!(stale.contains("which is gone"), "{stale}");
        assert!(stale.contains("doctor --fix"), "{stale}");

        assert_eq!(headline(&State::Stopped, now), "not running: no pidfile.");
    }

    #[test]
    fn a_gateway_older_than_the_binary_here_is_told_to_be_restarted() {
        let said = headline(
            &State::Running(record(4242, "0.0.1-ancient")),
            at("2026-08-31T09:00:01Z"),
        );
        assert!(said.contains("the binary here is"), "{said}");
        assert!(said.contains("restart to pick it up"), "{said}");
    }

    #[test]
    fn status_names_both_files_even_when_nothing_runs() {
        let home = tempfile::tempdir().expect("a temporary home");
        let paths = Paths::new(&bingo_sdk::Env::rooted(home.path()));
        let said = status(&paths, home.path(), &Fake::empty()).expect("a report");
        assert!(said.contains("not running"), "{said}");
        assert!(said.contains("gateway.pid"), "{said}");
        assert!(said.contains("gateway.log"), "{said}");
        assert!(said.contains("mode: by hand"), "{said}");
    }

    #[test]
    fn logs_says_what_is_there_or_that_nothing_is() {
        let home = tempfile::tempdir().expect("a temporary home");
        let paths = Paths::new(&bingo_sdk::Env::rooted(home.path()));
        let missing = logs(&paths, 10).expect("a report");
        assert!(missing.contains("does not exist yet"), "{missing}");

        paths.ensure().expect("the directory");
        std::fs::write(paths.log(), "").expect("an empty log");
        assert!(logs(&paths, 10).expect("a report").contains("is empty"));

        std::fs::write(paths.log(), "one\ntwo\nthree\n").expect("a log");
        let said = logs(&paths, 2).expect("a report");
        assert!(said.contains("gateway.log"), "{said}");
        assert!(said.ends_with("two\nthree"), "{said}");
        assert!(!said.contains("one\n"), "only the tail asked for: {said}");
    }
}

//! `gateway start`: get a resident bingo running, whoever will keep it alive.
//!
//! By hand that means spawning `gateway run` in a session of its own
//! (process-wrap's `ProcessSession` — no `unsafe`, no `libc`) with both streams
//! pointed at the log, and then waiting for the pidfile to appear rather than
//! assuming it will. Under a supervisor it means saying so to the supervisor
//! and waiting for exactly the same file, because the thing being waited for
//! is the gateway being up, not the way it was asked to come up.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use bingo_sdk::{ErrorCode, KernelError};
#[cfg(unix)]
use process_wrap::tokio::ProcessSession;
use process_wrap::tokio::{ChildWrapper, CommandWrap};

use super::paths::Paths;
use super::pidfile::Record;
use super::probe::Probe;
use super::service::{self, Ask, Mode};
use super::state::State;

/// How long a start waits for the pidfile. A gateway that has not written one
/// by then has failed, and the log says why.
pub const PATIENCE: Duration = Duration::from_secs(30);

/// How often the pidfile is looked for while waiting.
const GLANCE: Duration = Duration::from_millis(50);

/// What `gateway run` needs from the command line that started it: which
/// directory it works in, and which extra settings file it was given. Both
/// decide where the gateway's sessions and channels come from, so both must
/// survive the detach.
#[derive(Clone, Debug)]
pub struct Forward {
    pub cwd: PathBuf,
    pub settings: Option<PathBuf>,
}

impl Forward {
    /// The arguments after the binary's own name.
    pub fn argv(&self) -> Vec<PathBuf> {
        let mut argv = vec![
            PathBuf::from("gateway"),
            PathBuf::from("run"),
            PathBuf::from("--cwd"),
            self.cwd.clone(),
        ];
        if let Some(settings) = &self.settings {
            argv.push(PathBuf::from("--settings"));
            argv.push(settings.clone());
        }
        argv
    }
}

pub async fn start(
    paths: &Paths,
    home: &Path,
    forward: &Forward,
    probe: &dyn Probe,
) -> Result<String, KernelError> {
    if let Some(record) = State::read(paths, probe)?.running() {
        // The remedy comes before the paths: a refusal is truncated for the
        // terminal, and what a person must do has to survive the cut.
        return Err(invalid(format!(
            "a gateway already runs here: pid {} (bingo {}). \
             `bingo gateway stop` first, or remove {} if it is not running.",
            record.pid,
            record.version,
            paths.pidfile().display()
        )));
    }
    paths.ensure().map_err(internal)?;
    let mode = Mode::here(home);
    let child = match mode {
        Mode::Installed(supervisor) => {
            let file = supervisor.path(home);
            let uid = service::uid();
            // `install` loads the service, and launchd refuses to bootstrap a
            // loaded one (error 5: Input/output error). A start that finds it
            // loaded kicks it instead; only an unloaded one is bootstrapped.
            let ask = match service::loaded(supervisor, &uid) {
                true => Ask::Kick,
                false => Ask::Start,
            };
            service::tell(supervisor, ask, &uid, &file).map_err(invalid)?;
            None
        }
        Mode::Hand => Some(spawn(paths, forward)?),
    };
    let record = awaited(paths, probe, child, None).await?;
    Ok(receipt(&record, paths, mode, home))
}

/// `gateway restart`.
///
/// Under a supervisor this is one word to the supervisor, never a stop
/// followed by a start: letting it do both halves is what stops its own
/// respawn from racing ours for the pidfile (ADR-0020 §7). By hand it is
/// exactly the two verbs, in order, each waiting for what it asked for.
pub async fn restart(
    paths: &Paths,
    home: &Path,
    forward: &Forward,
    probe: &dyn Probe,
) -> Result<String, KernelError> {
    let Mode::Installed(supervisor) = Mode::here(home) else {
        let stopped = super::stop::stop(paths, home, probe).await?;
        let started = start(paths, home, forward, probe).await?;
        return Ok(format!("{stopped}\n{started}"));
    };
    let previous = State::read(paths, probe)?
        .running()
        .map(|record| record.pid);
    let file = supervisor.path(home);
    service::tell(supervisor, Ask::Restart, &service::uid(), &file).map_err(invalid)?;
    let record = awaited(paths, probe, None, previous).await?;
    Ok(receipt(&record, paths, Mode::Installed(supervisor), home))
}

/// `<this binary> gateway run`, in a session of its own so that closing the
/// terminal does not close it, with stdin at `/dev/null` and both streams
/// appended to the log.
fn spawn(paths: &Paths, forward: &Forward) -> Result<Box<dyn ChildWrapper>, KernelError> {
    let exe = std::env::current_exe().map_err(|e| internal(format!("this binary: {e}")))?;
    let log = super::log::open(&paths.log()).map_err(internal)?;
    let errors = log
        .try_clone()
        .map_err(|e| internal(format!("{}: {e}", paths.log().display())))?;
    let mut command = tokio::process::Command::new(exe);
    command
        .args(forward.argv())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(errors));
    let mut wrapped = CommandWrap::from(command);
    // A session, not merely a group: the gateway must survive the terminal
    // that started it, and it must never be sent the terminal's own signals.
    #[cfg(unix)]
    wrapped.wrap(ProcessSession);
    // Windows has no session to leave, and a child there already outlives the
    // parent that spawned it, so the detach is the default and nothing is
    // wrapped. It does still share the console it was started from, which a
    // `DETACHED_PROCESS` creation flag would sever — that needs the `windows`
    // crate's own types, which is a dependency this has not earned yet.

    wrapped
        .spawn()
        .map_err(|e| internal(format!("could not start the gateway: {e}")))
}

/// Wait for the gateway to write its pidfile.
///
/// `previous` is the pid a restart is replacing: while that one is still the
/// one in the file, the gateway being waited for has not arrived yet. A child
/// that exits while we wait is not something to keep waiting for — the tail of
/// the log is what a person needs, and a timeout would have hidden it.
async fn awaited(
    paths: &Paths,
    probe: &dyn Probe,
    mut child: Option<Box<dyn ChildWrapper>>,
    previous: Option<u32>,
) -> Result<Record, KernelError> {
    let started = Instant::now();
    loop {
        if let Some(record) = State::read(paths, probe)?
            .running()
            .filter(|record| Some(record.pid) != previous)
        {
            return Ok(record.clone());
        }
        if let Some(status) = child.as_mut().and_then(|c| c.try_wait().ok().flatten()) {
            return Err(invalid(gave_up(paths, &format!("it exited {status}"))));
        }
        if started.elapsed() > PATIENCE {
            return Err(invalid(gave_up(
                paths,
                &format!("nothing was there after {PATIENCE:?}"),
            )));
        }
        tokio::time::sleep(GLANCE).await;
    }
}

/// Why the wait ended badly, with the last of the log, because the reason is
/// in the log and nowhere else.
fn gave_up(paths: &Paths, why: &str) -> String {
    let log = paths.log();
    let tail = std::fs::read_to_string(&log)
        .map(|text| super::log::tail(&text, 20).to_string())
        .unwrap_or_default();
    let mut said = format!(
        "the gateway did not come up: {why}. Its log is {}",
        log.display()
    );
    if !tail.trim().is_empty() {
        said.push_str(&format!(":\n{}", tail.trim_end()));
    }
    said
}

fn receipt(record: &Record, paths: &Paths, mode: Mode, home: &Path) -> String {
    [
        format!(
            "The gateway is up: pid {} (bingo {}).",
            record.pid, record.version
        ),
        mode.line(home),
        format!("pidfile: {}", paths.pidfile().display()),
        format!("log: {}", paths.log().display()),
    ]
    .join("\n")
}

fn invalid(message: impl Into<String>) -> KernelError {
    KernelError::new(ErrorCode::InvalidInput, message)
}

fn internal(message: impl Into<String>) -> KernelError {
    KernelError::new(ErrorCode::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_forwarded_argv_carries_the_directory_and_the_settings_and_nothing_else() {
        let plain = Forward {
            cwd: PathBuf::from("/work"),
            settings: None,
        };
        assert_eq!(
            plain.argv(),
            [
                PathBuf::from("gateway"),
                PathBuf::from("run"),
                PathBuf::from("--cwd"),
                PathBuf::from("/work"),
            ]
        );
        let with = Forward {
            settings: Some(PathBuf::from("/work/channels.json")),
            ..plain
        };
        assert_eq!(
            with.argv().last(),
            Some(&PathBuf::from("/work/channels.json"))
        );
        assert!(with.argv().contains(&PathBuf::from("--settings")));
    }

    #[test]
    fn giving_up_names_the_log_and_shows_the_end_of_it() {
        let home = tempfile::tempdir().expect("a temporary home");
        let paths = Paths::new(&bingo_sdk::Env::rooted(home.path()));
        paths.ensure().expect("the directory");
        assert!(
            gave_up(&paths, "it exited 1").contains("gateway.log"),
            "an empty log still names itself"
        );
        std::fs::write(paths.log(), "boot\nno channel is configured\n").expect("a log");
        let said = gave_up(&paths, "it exited 1");
        assert!(said.contains("it exited 1"), "{said}");
        assert!(said.contains("no channel is configured"), "{said}");
    }
}

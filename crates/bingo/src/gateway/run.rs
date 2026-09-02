//! `gateway run`: the resident bingo itself (ADR-0020 §1, §3, §4).
//!
//! It is not a bridge and proxies nothing. The host it assembles is the
//! ordinary one on the existing `Work::Channels` path — the same sessions, the
//! same transcripts, the same schedule runner claim in the same place — with
//! three things wrapped around it: the log sink is installed, the pidfile is
//! held, and TERM ends the surface instead of the process, so every `Drop` and
//! every `Plugin::stop` runs before the claims are given back.
//!
//! Their order is the contract, not an accident. The pidfile is what tells
//! `gateway start` that this process is up and may be stopped, and the host
//! takes a while to build after it appears — so the signals are registered
//! first, and the file is written to a process that can already answer them.

use std::sync::Arc;

use bingo_sdk::{ErrorCode, Exit, HostHandle, KernelError, Surface, SurfaceOptions};
use jiff::Timestamp;

use super::paths::Paths;
use super::pidfile::{self, Record};
use super::probe::{self, Probe};

/// What a resident process holds for as long as it runs: the pidfile, and the
/// signals that end it. Dropping it gives the pidfile back, so it must outlive
/// `Host::shutdown`.
#[derive(Debug)]
pub struct Resident {
    _claim: pidfile::Claim,
    leaving: Leaving,
}

/// The ways the operating system asks a process to leave, in whichever words
/// this one uses. Both platforms are registered the same way and answered the
/// same way; only the names differ, and only in here.
#[derive(Debug)]
struct Leaving {
    /// TERM is what a supervisor and `gateway stop` send; INT is what a person
    /// at the keyboard sends to a `gateway run` they started in the
    /// foreground, and it deserves the same clean end.
    #[cfg(unix)]
    term: tokio::signal::unix::Signal,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    /// Windows has no signal to send a process by number. What reaches a
    /// console program instead is ctrl+c, ctrl+break, or the close event the
    /// system sends before it takes the process away.
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
    #[cfg(windows)]
    ctrl_close: tokio::signal::windows::CtrlClose,
}

impl Leaving {
    /// Register for all of them. Once this returns, none of them can kill this
    /// process where it stands — which is the whole reason it is called before
    /// the pidfile is written.
    fn registered() -> Result<Self, KernelError> {
        Ok(Self {
            #[cfg(unix)]
            term: signal(SignalKind::Terminate)?,
            #[cfg(unix)]
            interrupt: signal(SignalKind::Interrupt)?,
            #[cfg(windows)]
            ctrl_c: windows_signal(tokio::signal::windows::ctrl_c())?,
            #[cfg(windows)]
            ctrl_break: windows_signal(tokio::signal::windows::ctrl_break())?,
            #[cfg(windows)]
            ctrl_close: windows_signal(tokio::signal::windows::ctrl_close())?,
        })
    }

    /// Wait until one of them arrives, and say which. One that arrived while
    /// the host was still being built is already waiting here.
    async fn asked(&mut self) -> &'static str {
        #[cfg(unix)]
        {
            tokio::select! {
                _ = self.term.recv() => "SIGTERM",
                _ = self.interrupt.recv() => "SIGINT",
            }
        }
        #[cfg(windows)]
        {
            tokio::select! {
                _ = self.ctrl_c.recv() => "ctrl+c",
                _ = self.ctrl_break.recv() => "ctrl+break",
                _ = self.ctrl_close.recv() => "the close event",
            }
        }
    }
}

/// Everything that must be true before a host is built: the directory exists,
/// the log is open and taking lines, and this process holds the pidfile.
///
/// The sink goes in first so that a refusal below is the first thing written
/// to the log a person will go and read.
pub fn enter(paths: &Paths, probe: &dyn Probe) -> Result<Resident, KernelError> {
    paths.ensure().map_err(internal)?;
    let file = super::log::open(&paths.log()).map_err(internal)?;
    super::log::install(file).map_err(internal)?;
    // Before the pidfile, never after. `gateway start` returns the moment it
    // can read that file, so from that instant a TERM may arrive — and until
    // these are registered the kernel answers one with its default action,
    // which is to kill this process where it stands: no `Drop`, no
    // `Plugin::stop`, and a pidfile nobody gives back. The host is still being
    // built at that point, so the handlers cannot wait for it.
    let leaving = Leaving::registered()?;
    let path = paths.pidfile();
    if let Some(old) = pidfile::read(&path).map_err(internal)? {
        replace(&path, &old, probe)?;
    }
    let claim = pidfile::Claim::take(&path, &Record::here(Timestamp::now())).map_err(internal)?;
    tracing::info!(
        pid = std::process::id(),
        version = pidfile::version(),
        pidfile = %path.display(),
        "the gateway is up"
    );
    Ok(Resident {
        _claim: claim,
        leaving,
    })
}

/// A record already there: refuse if its process is still running, and take
/// the file over if it is not.
///
/// A crash leaves the record behind, and a supervisor's respawn must not wedge
/// on the corpse's file (ADR-0020 §3) — but a *live* pid is never stepped on,
/// whatever it turns out to be running.
fn replace(path: &std::path::Path, old: &Record, probe: &dyn Probe) -> Result<(), KernelError> {
    if probe.alive(old.pid) {
        return Err(KernelError::new(
            ErrorCode::InvalidInput,
            taken(path, old, probe),
        ));
    }
    tracing::warn!(
        pid = old.pid,
        version = %old.version,
        started = %old.started,
        pidfile = %path.display(),
        "replacing the record of a gateway that is gone; it did not stop cleanly"
    );
    std::fs::remove_file(path).map_err(|e| internal(format!("{}: {e}", path.display())))
}

/// The refusal a second gateway gets, with the pid it lost to and what that
/// pid turns out to be.
fn taken(path: &std::path::Path, old: &Record, probe: &dyn Probe) -> String {
    let what = match probe::is_bingo(probe, old.pid) {
        true => format!("a bingo {} started {}", old.version, old.started),
        false => format!(
            "pid {} is alive but is not a bingo — the number came round again",
            old.pid
        ),
    };
    format!(
        "a gateway already holds this data dir: pid {} ({what}). {} says so — \
         `bingo gateway stop`, or remove that file if no bingo is running.",
        old.pid,
        path.display()
    )
}

impl Resident {
    /// The surface, until it ends on its own or the operating system asks this
    /// process to leave.
    ///
    /// The channels surface is `SurfaceKind::Concurrent` and never returns of
    /// its own accord, so in practice this is the signal arm. Returning from
    /// here is what lets the caller run `Host::shutdown` — the difference
    /// between a gateway that gave its locks back and one that has to be
    /// cleaned up by hand. A signal that arrived while the host was still
    /// being built is already waiting: `enter` registered for it before it
    /// wrote the pidfile, and a registered signal is held, not lost.
    pub async fn until_signalled(
        &mut self,
        surface: Arc<dyn Surface>,
        host: HostHandle,
        options: SurfaceOptions,
    ) -> Result<Exit, KernelError> {
        tokio::select! {
            exit = surface.run(host, options) => {
                tracing::warn!("the channels surface ended on its own");
                exit
            }
            asked = self.leaving.asked() => Ok(leaving(asked)),
        }
    }
}

/// The two signals that mean "stop", by the numbers only unix has.
#[cfg(unix)]
#[derive(Clone, Copy, Debug)]
enum SignalKind {
    Terminate,
    Interrupt,
}

#[cfg(unix)]
fn signal(kind: SignalKind) -> Result<tokio::signal::unix::Signal, KernelError> {
    let kind = match kind {
        SignalKind::Terminate => tokio::signal::unix::SignalKind::terminate(),
        SignalKind::Interrupt => tokio::signal::unix::SignalKind::interrupt(),
    };
    tokio::signal::unix::signal(kind).map_err(|e| internal(format!("listening for a signal: {e}")))
}

/// The same failure, for the console events Windows registers by name.
#[cfg(windows)]
fn windows_signal<T>(registered: std::io::Result<T>) -> Result<T, KernelError> {
    registered.map_err(|e| internal(format!("listening for a signal: {e}")))
}

fn leaving(signal: &str) -> Exit {
    tracing::info!(signal, "stopping: surfaces first, then the plugins");
    Exit { code: 0 }
}

fn internal(message: impl Into<String>) -> KernelError {
    KernelError::new(ErrorCode::Internal, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::probe::tests::Fake;

    fn paths(home: &std::path::Path) -> Paths {
        Paths::new(&bingo_sdk::Env::rooted(home))
    }

    /// `enter` installs a process-wide log sink, which a test process may only
    /// do once; these exercise the pidfile policy directly instead.
    fn wrote(home: &std::path::Path, record: &Record) -> std::path::PathBuf {
        let paths = paths(home);
        paths.ensure().expect("the directory");
        let path = paths.pidfile();
        std::fs::write(&path, pidfile::render(record)).expect("a record");
        path
    }

    fn record(pid: u32) -> Record {
        Record {
            pid,
            version: "0.1.0".into(),
            started: "2026-08-31T09:00:00Z".parse().expect("a timestamp"),
        }
    }

    #[test]
    fn a_record_whose_process_is_gone_is_taken_over() {
        let home = tempfile::tempdir().expect("a temporary home");
        let path = wrote(home.path(), &record(4242));
        replace(&path, &record(4242), &Fake::empty()).expect("the corpse's file is taken");
        assert!(
            !path.exists(),
            "a respawn after a crash must not wedge on the file"
        );
    }

    #[test]
    fn a_record_whose_process_is_alive_is_refused_and_left_alone() {
        let home = tempfile::tempdir().expect("a temporary home");
        let path = wrote(home.path(), &record(4242));
        let table = Fake::of(&[(4242, "bingo")]);
        let refused = replace(&path, &record(4242), &table)
            .expect_err("the live gateway keeps its file")
            .message;
        assert!(refused.contains("pid 4242"), "{refused}");
        assert!(refused.contains("a bingo 0.1.0 started"), "{refused}");
        assert!(refused.contains("bingo gateway stop"), "{refused}");
        assert!(path.exists(), "the holder's record is untouched");
    }

    #[test]
    fn a_live_pid_that_is_not_a_bingo_is_still_never_stepped_on() {
        let home = tempfile::tempdir().expect("a temporary home");
        let path = wrote(home.path(), &record(4242));
        let table = Fake::of(&[(4242, "postgres")]);
        let refused = replace(&path, &record(4242), &table)
            .expect_err("a live pid is a live pid")
            .message;
        assert!(
            refused.contains("came round again"),
            "it says why it will not guess: {refused}"
        );
        assert!(path.exists());
    }
}

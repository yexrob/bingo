//! `bingo gateway`: one resident bingo per data dir, managed like a service
//! (ADR-0020).
//!
//! An IM channel is an inbound door, and a door that is only open while a
//! terminal is open is a door that misses conversations. So one process stays
//! up. It is not a bridge and proxies nothing: [`run`] assembles the ordinary
//! plugin host on the existing `Work::Channels` path, and the sessions,
//! transcripts, schedules and locks are the normal ones in the normal places.
//!
//! The pieces, in the order they rest on each other:
//!
//! - [`paths`] — where the gateway's two files live.
//! - [`pidfile`] — the record of who is resident, claimed and given back.
//! - [`probe`] — the process table, asked the way a shell asks it.
//! - [`state`] — the record and the process table read as one fact.
//! - [`log`] — the sink every `warn!` in this tree has been missing.
//! - [`unit`] / [`service`] — the supervisor's file, and which master is in
//!   force.
//! - [`run`] — the resident process; [`start`], [`stop`], [`status`] and
//!   [`doctor`] — what a person says to it.

pub mod doctor;
pub mod log;
pub mod paths;
pub mod pidfile;
pub mod probe;
pub mod run;
pub mod service;
pub mod start;
pub mod state;
pub mod status;
pub mod stop;
pub mod unit;

use std::path::Path;

use bingo_sdk::{Env, KernelError};
use clap::Subcommand;

use paths::Paths;
use probe::Kill;
use start::Forward;

#[derive(Subcommand, Debug, Clone)]
pub enum Verb {
    /// Start the resident gateway and wait until it is up.
    Start,
    /// Ask it to stop, and wait until its locks are given back.
    Stop,
    /// Stop it and start it again.
    Restart,
    /// Whether one is running here, as what, and since when.
    Status,
    /// The gateway log, and where it is.
    Logs {
        /// How many lines from the end.
        #[arg(long, short = 'n', default_value_t = status::LINES)]
        lines: usize,
    },
    /// Read the settings, the credentials and every lock, and say what to do.
    Doctor {
        /// Remove exactly the locks whose process is gone.
        #[arg(long)]
        fix: bool,
    },
    /// Keep the gateway alive across logins, through launchd or systemd.
    Install,
    /// Give the service back and start managing it by hand again.
    Uninstall,
    /// The resident process itself: what `start` launches. It runs in the
    /// foreground until it is asked to stop.
    Run,
}

impl Verb {
    /// Whether this verb *is* the gateway rather than something said to one.
    /// `run` needs the whole host, so the binary builds it; every other verb
    /// is answered here, before any plugin exists.
    pub fn is_run(&self) -> bool {
        matches!(self, Verb::Run)
    }
}

/// Every verb but [`Verb::Run`], answered without a host.
pub async fn dispatch(
    verb: &Verb,
    env: &Env,
    cwd: &Path,
    settings: Option<&Path>,
) -> Result<i32, KernelError> {
    let paths = Paths::new(env);
    let home = env.home.clone();
    let probe = Kill;
    let forward = Forward {
        cwd: cwd.to_path_buf(),
        settings: settings.map(Path::to_path_buf),
    };
    // The verbs that bring a gateway up validate its channels first
    // (user-directed): a configuration that cannot run is refused here, with
    // the doctor's own lines, rather than crash-looping under a supervisor.
    if matches!(verb, Verb::Start | Verb::Restart | Verb::Install) {
        doctor::preflight(&doctor::Patient {
            paths: &paths,
            env,
            cwd,
            settings,
        })?;
    }
    let said = match verb {
        Verb::Start => start::start(&paths, &home, &forward, &probe).await?,
        Verb::Restart => start::restart(&paths, &home, &forward, &probe).await?,
        Verb::Stop => stop::stop(&paths, &home, &probe).await?,
        Verb::Status => status::status(&paths, &home, &probe)?,
        Verb::Logs { lines } => status::logs(&paths, *lines)?,
        Verb::Doctor { fix } => doctor::doctor(
            &doctor::Patient {
                paths: &paths,
                env,
                cwd,
                settings,
            },
            &probe,
            *fix,
        )?,
        Verb::Install => service::install(&home, &paths.log())?,
        Verb::Uninstall => service::uninstall(&home)?,
        // `run` is the host itself; the binary builds it rather than routing
        // here, so reaching this arm is a wiring bug and says so.
        Verb::Run => {
            return Err(KernelError::new(
                bingo_sdk::ErrorCode::Internal,
                "`gateway run` is the host and is not dispatched here",
            ));
        }
    };
    println!("{said}");
    Ok(0)
}

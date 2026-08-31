//! Which master the gateway answers to (ADR-0020 §7).
//!
//! launchd's `KeepAlive` and a hand-spawned `gateway run` are two supervisors
//! for one pidfile, and two supervisors is a fight. So there is one switch:
//! **the service file is on disk**. `install` writes it only if the supervisor
//! accepted it and takes it back off disk if the supervisor refused, so a file
//! that exists is a service that is loaded — the mode cannot be half true.
//!
//! While installed, `start`, `stop` and `restart` say so to the supervisor
//! rather than spawning or signalling anything themselves.

use std::path::Path;

use bingo_sdk::{ErrorCode, KernelError};

use super::unit::Supervisor;

/// How the resident process is kept alive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// A per-user supervisor holds it: its file is on disk.
    Installed(Supervisor),
    /// Nobody holds it. `start` spawns and `stop` signals.
    Hand,
}

impl Mode {
    /// The mode in force under `home`.
    pub fn here(home: &Path) -> Self {
        match Supervisor::here() {
            Some(supervisor) if supervisor.path(home).exists() => Mode::Installed(supervisor),
            _ => Mode::Hand,
        }
    }

    /// The one line `status` and `doctor` print about it.
    pub fn line(self, home: &Path) -> String {
        match self {
            Mode::Installed(supervisor) => format!(
                "mode: installed — {} keeps it alive ({})",
                supervisor.name(),
                supervisor.path(home).display()
            ),
            Mode::Hand => "mode: by hand — `gateway start` spawns it, nothing respawns it".into(),
        }
    }
}

/// What the gateway asks a supervisor to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Re-read the files on disk. Only systemd needs telling.
    Reload,
    /// Take this service and keep it, now and at every login.
    Enable,
    /// Give it back and forget it.
    Disable,
    Start,
    /// Run a service the supervisor already holds. launchd refuses to
    /// bootstrap a loaded service (error 5), so a loaded one is kicked.
    Kick,
    Stop,
    Restart,
}

/// The command line for one ask, or `None` where this supervisor needs
/// nothing done for it.
///
/// Nothing here runs anything: every argv is pinned by a test, because a wrong
/// word to `launchctl` is a mistake nobody sees until a machine reboots
/// (M17 R-supervisor).
pub fn argv(supervisor: Supervisor, ask: Ask, uid: &str, file: &Path) -> Option<Vec<String>> {
    let words = match supervisor {
        Supervisor::Launchd => launchctl(ask, uid, file)?,
        Supervisor::Systemd => systemctl(ask)?,
    };
    Some(words)
}

/// launchd loads a service by bootstrapping its file into the user's domain,
/// and there is no separate "start": `KeepAlive` runs it the moment it is in.
fn launchctl(ask: Ask, uid: &str, file: &Path) -> Option<Vec<String>> {
    let domain = format!("gui/{uid}");
    let target = format!("{domain}/{}", super::unit::LABEL);
    let words: Vec<String> = match ask {
        Ask::Reload => return None,
        Ask::Enable | Ask::Start => vec![
            "launchctl".into(),
            "bootstrap".into(),
            domain,
            file.display().to_string(),
        ],
        Ask::Kick => vec!["launchctl".into(), "kickstart".into(), target],
        Ask::Disable | Ask::Stop => vec!["launchctl".into(), "bootout".into(), target],
        Ask::Restart => vec!["launchctl".into(), "kickstart".into(), "-k".into(), target],
    };
    Some(words)
}

/// Whether the supervisor already holds the service. Only launchd needs the
/// answer: its `bootstrap` refuses a loaded service, while `systemctl start`
/// is idempotent — so for systemd the question is never asked.
pub fn loaded(supervisor: Supervisor, uid: &str) -> bool {
    match supervisor {
        Supervisor::Launchd => std::process::Command::new("launchctl")
            .args(["print", &format!("gui/{uid}/{}", super::unit::LABEL)])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success()),
        Supervisor::Systemd => false,
    }
}

fn systemctl(ask: Ask) -> Option<Vec<String>> {
    let unit = super::unit::UNIT;
    let verb: &[&str] = match ask {
        Ask::Reload => {
            return Some(vec![
                "systemctl".into(),
                "--user".into(),
                "daemon-reload".into(),
            ]);
        }
        Ask::Enable => &["enable", "--now"],
        Ask::Disable => &["disable", "--now"],
        Ask::Start | Ask::Kick => &["start"],
        Ask::Stop => &["stop"],
        Ask::Restart => &["restart"],
    };
    let mut words = vec!["systemctl".to_string(), "--user".to_string()];
    words.extend(verb.iter().map(|word| (*word).to_string()));
    words.push(unit.to_string());
    Some(words)
}

/// Say it, and report what the supervisor said back. The output is kept
/// because `launchctl bootout` on a service that was not loaded is the one
/// failure a caller forgives, and it can only forgive what it can read.
pub fn tell(supervisor: Supervisor, ask: Ask, uid: &str, file: &Path) -> Result<(), String> {
    let Some(words) = argv(supervisor, ask, uid, file) else {
        return Ok(());
    };
    let (program, arguments) = words.split_first().ok_or("an empty command")?;
    let out = std::process::Command::new(program)
        .args(arguments)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    Err(format!(
        "`{}` failed: {}",
        words.join(" "),
        said.trim().replace('\n', "; ")
    ))
}

/// This user's id, which launchd needs to name the domain. `id -u` rather than
/// `libc::getuid`, for the reason every other question here is a subprocess.
pub fn uid() -> String {
    std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|uid| !uid.is_empty())
        .unwrap_or_else(|| "0".into())
}

/// Write the service file and hand it to the supervisor. If the supervisor
/// refuses it, the file comes back off disk: the mode switch is the file, and
/// a file the supervisor never took would be a lie about which master is in
/// charge.
pub fn install(home: &Path, log: &Path) -> Result<String, KernelError> {
    let supervisor = supervisor_here()?;
    let file = supervisor.path(home);
    if file.exists() {
        return Err(invalid(format!(
            "already installed: {} is on disk. `bingo gateway uninstall` first",
            file.display()
        )));
    }
    let exe = std::env::current_exe()
        .map_err(|e| KernelError::new(ErrorCode::Internal, format!("this binary: {e}")))?;
    write(&file, &supervisor.render(&exe, log))?;
    if let Err(refused) = load(supervisor, &file) {
        let _ = std::fs::remove_file(&file);
        // The fact that matters comes first: a refusal is truncated for the
        // terminal, and "nothing was installed" must survive the cut.
        return Err(invalid(format!(
            "the supervisor refused it, so the service file was removed again \
             — an installed gateway is one the supervisor took. {refused}"
        )));
    }
    Ok(receipt(supervisor, &file, &exe, log))
}

/// Reload first, because systemd will not see a unit it has not been told
/// about, then enable it now and at every login.
fn load(supervisor: Supervisor, file: &Path) -> Result<(), String> {
    let uid = uid();
    tell(supervisor, Ask::Reload, &uid, file)?;
    tell(supervisor, Ask::Enable, &uid, file)
}

/// Unload and remove. A supervisor that says it never had the service is not
/// an error — the file is going either way, and leaving it would leave the
/// mode switch stuck on.
pub fn uninstall(home: &Path) -> Result<String, KernelError> {
    let supervisor = supervisor_here()?;
    let file = supervisor.path(home);
    if !file.exists() {
        return Err(invalid(format!(
            "not installed: there is no {}",
            file.display()
        )));
    }
    let uid = uid();
    let refused = tell(supervisor, Ask::Disable, &uid, &file).err();
    std::fs::remove_file(&file)
        .map_err(|e| KernelError::new(ErrorCode::Internal, format!("{}: {e}", file.display())))?;
    let _ = tell(supervisor, Ask::Reload, &uid, &file);
    let mut lines = vec![format!("{} is gone.", file.display())];
    if let Some(refused) = refused {
        lines.push(format!("The supervisor had already let it go ({refused})."));
    }
    lines.push("The gateway is now started and stopped by hand.".into());
    Ok(lines.join("\n"))
}

fn supervisor_here() -> Result<Supervisor, KernelError> {
    Supervisor::here().ok_or_else(|| {
        invalid(format!(
            "no per-user service manager on {}: install is launchd (macOS) or \
             systemd --user (Linux)",
            std::env::consts::OS
        ))
    })
}

fn write(file: &Path, text: &str) -> Result<(), KernelError> {
    let internal = |e: std::io::Error| {
        KernelError::new(ErrorCode::Internal, format!("{}: {e}", file.display()))
    };
    if let Some(directory) = file.parent() {
        std::fs::create_dir_all(directory).map_err(internal)?;
    }
    std::fs::write(file, text).map_err(internal)
}

fn receipt(supervisor: Supervisor, file: &Path, exe: &Path, log: &Path) -> String {
    [
        format!("{} is installed as {}.", file.display(), supervisor.name()),
        format!(
            "It runs {} gateway run and logs to {}.",
            exe.display(),
            log.display()
        ),
        "It carries no credentials: a service started at boot has no shell to \
         inherit them from, so put a channel secret in the store with \
         `bingo channels secret <adapter>`."
            .into(),
    ]
    .join("\n")
}

fn invalid(message: impl Into<String>) -> KernelError {
    KernelError::new(ErrorCode::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(supervisor: Supervisor, ask: Ask) -> Vec<String> {
        argv(supervisor, ask, "501", Path::new("/home/me/svc.file")).unwrap_or_default()
    }

    #[test]
    fn launchd_is_told_in_the_words_launchctl_answers_to() {
        assert_eq!(
            words(Supervisor::Launchd, Ask::Enable),
            ["launchctl", "bootstrap", "gui/501", "/home/me/svc.file"]
        );
        assert_eq!(
            words(Supervisor::Launchd, Ask::Disable),
            ["launchctl", "bootout", "gui/501/com.bingo.gateway"]
        );
        assert_eq!(
            words(Supervisor::Launchd, Ask::Restart),
            ["launchctl", "kickstart", "-k", "gui/501/com.bingo.gateway"]
        );
        assert_eq!(
            words(Supervisor::Launchd, Ask::Start),
            words(Supervisor::Launchd, Ask::Enable),
            "launchd has no start apart from loading it"
        );
        assert_eq!(
            words(Supervisor::Launchd, Ask::Kick),
            ["launchctl", "kickstart", "gui/501/com.bingo.gateway"],
            "a loaded service is kicked, never bootstrapped twice"
        );
        assert_eq!(
            words(Supervisor::Systemd, Ask::Kick),
            ["systemctl", "--user", "start", "bingo-gateway.service"],
            "systemctl start is idempotent, so the kick is the start"
        );
        assert!(
            argv(Supervisor::Launchd, Ask::Reload, "501", Path::new("/x")).is_none(),
            "launchd re-reads the file when it is bootstrapped"
        );
    }

    #[test]
    fn systemd_is_told_in_the_words_systemctl_answers_to() {
        assert_eq!(
            words(Supervisor::Systemd, Ask::Reload),
            ["systemctl", "--user", "daemon-reload"]
        );
        assert_eq!(
            words(Supervisor::Systemd, Ask::Enable),
            [
                "systemctl",
                "--user",
                "enable",
                "--now",
                "bingo-gateway.service"
            ]
        );
        assert_eq!(
            words(Supervisor::Systemd, Ask::Disable),
            [
                "systemctl",
                "--user",
                "disable",
                "--now",
                "bingo-gateway.service"
            ]
        );
        assert_eq!(
            words(Supervisor::Systemd, Ask::Start),
            ["systemctl", "--user", "start", "bingo-gateway.service"]
        );
        assert_eq!(
            words(Supervisor::Systemd, Ask::Stop),
            ["systemctl", "--user", "stop", "bingo-gateway.service"]
        );
        assert_eq!(
            words(Supervisor::Systemd, Ask::Restart),
            ["systemctl", "--user", "restart", "bingo-gateway.service"]
        );
    }

    #[test]
    fn the_mode_is_the_file_and_nothing_else() {
        let home = tempfile::tempdir().expect("a temporary home");
        assert_eq!(Mode::here(home.path()), Mode::Hand);
        let Some(supervisor) = Supervisor::here() else {
            return; // Nothing to install on this platform; `Hand` is the answer.
        };
        let file = supervisor.path(home.path());
        std::fs::create_dir_all(file.parent().expect("a directory")).expect("made");
        std::fs::write(&file, "anything at all").expect("a service file");
        assert_eq!(Mode::here(home.path()), Mode::Installed(supervisor));
        assert!(
            Mode::here(home.path())
                .line(home.path())
                .contains("installed")
        );

        std::fs::remove_file(&file).expect("removed");
        assert_eq!(Mode::here(home.path()), Mode::Hand);
        assert!(
            Mode::here(home.path())
                .line(home.path())
                .contains("by hand")
        );
    }

    #[test]
    fn the_uid_is_a_number_this_machine_agrees_with() {
        assert!(
            uid().chars().all(|c| c.is_ascii_digit()),
            "`id -u` answers in digits: {}",
            uid()
        );
    }
}

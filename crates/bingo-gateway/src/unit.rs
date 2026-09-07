//! The file a per-user supervisor reads to keep the gateway alive
//! (ADR-0020 §7): a launchd agent on macOS, a systemd user unit on Linux.
//!
//! Rendering it is pure — a path in, text out — because the one property that
//! matters about this file is asserted byte-wise: it names the binary that
//! wrote it, it points both streams at the gateway log, and it carries **no
//! secrets**. A boot-started gateway has no exported environment, which is why
//! §8 gives the secret a disk home instead of putting it here.

use std::path::{Path, PathBuf};

/// The per-user supervisor of the platform this build runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Supervisor {
    Launchd,
    Systemd,
}

/// launchd's name for us, and the last path segment of its file.
pub const LABEL: &str = "com.bingo.gateway";
/// systemd's name for us, which is its file name too.
pub const UNIT: &str = "bingo-gateway.service";

impl Supervisor {
    /// The supervisor here, or `None` where this tree does not go. Windows is
    /// out of scope, as everywhere (ADR-0020 consequences).
    pub fn here() -> Option<Self> {
        match std::env::consts::OS {
            "macos" => Some(Supervisor::Launchd),
            "linux" => Some(Supervisor::Systemd),
            _ => None,
        }
    }

    /// Where this supervisor looks for a user's own services.
    pub fn path(self, home: &Path) -> PathBuf {
        match self {
            Supervisor::Launchd => home
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
            Supervisor::Systemd => home.join(".config/systemd/user").join(UNIT),
        }
    }

    /// What this supervisor calls the service, for a message a person reads.
    pub fn name(self) -> &'static str {
        match self {
            Supervisor::Launchd => LABEL,
            Supervisor::Systemd => UNIT,
        }
    }

    /// The file itself: `<exe> gateway run`, kept alive, both streams to the
    /// log, and not one environment variable.
    pub fn render(self, exe: &Path, log: &Path) -> String {
        match self {
            Supervisor::Launchd => plist(exe, log),
            Supervisor::Systemd => service(exe, log),
        }
    }
}

/// launchd restarts on a bad exit only: a `stop` ends the process with 0, and
/// a supervisor that respawned that would make `stop` a lie.
fn plist(exe: &Path, log: &Path) -> String {
    let exe = xml(&exe.display().to_string());
    let log = xml(&log.display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{exe}</string>
		<string>gateway</string>
		<string>run</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ProcessType</key>
	<string>Background</string>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
</dict>
</plist>
"#
    )
}

/// `Restart=on-failure` for the same reason as launchd's `SuccessfulExit`:
/// the graceful end of ADR-0020 §4 exits 0 and must stay ended.
fn service(exe: &Path, log: &Path) -> String {
    let exe = exe.display();
    let log = log.display();
    format!(
        "[Unit]\n\
         Description=bingo gateway\n\
         Documentation=https://github.com/yexrob/bingo\n\
         After=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart=\"{exe}\" gateway run\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         StandardOutput=append:{log}\n\
         StandardError=append:{log}\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

/// The five characters that would otherwise end an element early. A home
/// directory may hold any of them, and a plist that will not parse is a
/// gateway that will not start.
fn xml(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRETS: [&str; 5] = [
        "BINGO_FEISHU_APP_SECRET",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
        "EnvironmentVariables",
        "Environment=",
    ];

    fn rendered(supervisor: Supervisor) -> String {
        supervisor.render(
            Path::new("/opt/bingo/bin/bingo"),
            Path::new("/home/me/.bingo/data/gateway/gateway.log"),
        )
    }

    #[test]
    fn the_plist_runs_this_binary_and_keeps_it_alive_only_when_it_failed() {
        let plist = rendered(Supervisor::Launchd);
        assert!(plist.starts_with("<?xml version=\"1.0\""), "{plist}");
        assert!(
            plist.contains("<string>com.bingo.gateway</string>"),
            "{plist}"
        );
        assert!(
            plist.contains(
                "\t\t<string>/opt/bingo/bin/bingo</string>\n\
                 \t\t<string>gateway</string>\n\
                 \t\t<string>run</string>"
            ),
            "the exe and the verb it was installed for: {plist}"
        );
        assert!(
            plist.contains("<key>SuccessfulExit</key>\n\t\t<false/>"),
            "a stop that exited 0 must stay stopped: {plist}"
        );
        assert!(
            plist
                .matches("/home/me/.bingo/data/gateway/gateway.log")
                .count()
                == 2,
            "both streams go to the log: {plist}"
        );
    }

    #[test]
    fn the_unit_runs_this_binary_and_restarts_only_on_failure() {
        let unit = rendered(Supervisor::Systemd);
        assert!(
            unit.contains("ExecStart=\"/opt/bingo/bin/bingo\" gateway run\n"),
            "the exe is quoted, because a home directory may have a space: {unit}"
        );
        assert!(unit.contains("Restart=on-failure\n"), "{unit}");
        assert!(unit.contains("WantedBy=default.target\n"), "{unit}");
        assert!(
            unit.contains("StandardOutput=append:/home/me/.bingo/data/gateway/gateway.log\n"),
            "{unit}"
        );
    }

    #[test]
    fn neither_file_carries_a_secret_or_any_environment_at_all() {
        for supervisor in [Supervisor::Launchd, Supervisor::Systemd] {
            let text = rendered(supervisor);
            for secret in SECRETS {
                assert!(
                    !text.contains(secret),
                    "{secret} reached the {supervisor:?} file: {text}"
                );
            }
        }
    }

    #[test]
    fn each_supervisor_keeps_its_file_where_it_looks_for_one() {
        let home = Path::new("/home/me");
        assert_eq!(
            Supervisor::Launchd.path(home),
            Path::new("/home/me/Library/LaunchAgents/com.bingo.gateway.plist")
        );
        assert_eq!(
            Supervisor::Systemd.path(home),
            Path::new("/home/me/.config/systemd/user/bingo-gateway.service")
        );
    }

    #[test]
    fn a_path_with_xml_in_it_cannot_break_the_plist_open() {
        let plist = Supervisor::Launchd.render(
            Path::new("/home/a&b/<bingo>"),
            Path::new("/home/a&b/gateway.log"),
        );
        assert!(
            plist.contains("<string>/home/a&amp;b/&lt;bingo&gt;</string>"),
            "{plist}"
        );
        assert!(!plist.contains("/home/a&b/"), "{plist}");
    }

    #[test]
    fn the_supervisor_here_is_the_one_this_platform_has() {
        let here = Supervisor::here();
        #[cfg(target_os = "macos")]
        assert_eq!(here, Some(Supervisor::Launchd));
        #[cfg(target_os = "linux")]
        assert_eq!(here, Some(Supervisor::Systemd));
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        assert_eq!(here, None);
    }
}

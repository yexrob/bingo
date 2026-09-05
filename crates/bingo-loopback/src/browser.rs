//! Opening a URL in the person's browser, best effort.
//!
//! Never awaited, and what `false` means is the caller's to decide: a login
//! shows the URL through the prompter and carries on, while a page nobody can
//! open is a call that fails at once (ADR-0042 §4). Either way a machine with
//! no browser — a container, an ssh session — still has the URL. A launcher
//! may instead delegate presentation of that URL to its client (ADR-0046).

use std::ffi::OsStr;
use std::process::{Command, Stdio};

/// Set to anything to keep a browser from opening; a test sets it so no run
/// ever steals the screen. Takes precedence over [`BROWSER_MODE_ENV`].
pub const NO_BROWSER_ENV: &str = "BINGO_NO_BROWSER";

/// Set to `client` when an attached client owns presenting the URL already
/// carried by tool progress or a login interaction. Other values keep the
/// platform opener; delegation does not acknowledge delivery or rendering.
pub const BROWSER_MODE_ENV: &str = "BINGO_BROWSER_MODE";

#[derive(Debug, PartialEq, Eq)]
enum BrowserMode {
    Disabled,
    Client,
    System,
}

fn selected(no_browser: bool, mode: Option<&OsStr>) -> BrowserMode {
    if no_browser {
        BrowserMode::Disabled
    } else if mode == Some(OsStr::new("client")) {
        BrowserMode::Client
    } else {
        BrowserMode::System
    }
}

/// Whether a browser was asked to open the URL or its presentation was
/// delegated to the client. `false` is the caller's cue to lean on the URL
/// it is already showing.
pub fn open(url: &str) -> bool {
    match selected(
        std::env::var_os(NO_BROWSER_ENV).is_some(),
        std::env::var_os(BROWSER_MODE_ENV).as_deref(),
    ) {
        BrowserMode::Disabled => return false,
        BrowserMode::Client => return true,
        BrowserMode::System => {}
    }
    command(url).is_some_and(|mut command| {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    })
}

fn command(url: &str) -> Option<Command> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("open");
        command.arg(url);
        command
    } else if cfg!(target_os = "windows") {
        let mut command = Command::new("cmd");
        command.args(["/c", "start", "", url]);
        command
    } else if cfg!(target_os = "linux") {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    } else {
        return None;
    };
    command.env_remove(NO_BROWSER_ENV);
    Some(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_opt_out_wins_over_every_browser_mode() {
        for mode in [None, Some("client"), Some(""), Some("unknown")] {
            assert_eq!(selected(true, mode.map(OsStr::new)), BrowserMode::Disabled);
        }
    }

    #[test]
    fn only_the_exact_client_mode_delegates_presentation() {
        assert_eq!(
            selected(false, Some(OsStr::new("client"))),
            BrowserMode::Client
        );
        for mode in [
            None,
            Some(""),
            Some("unknown"),
            Some("Client"),
            Some("client "),
        ] {
            assert_eq!(selected(false, mode.map(OsStr::new)), BrowserMode::System);
        }
    }

    /// `std::env::set_var` is unsafe and this workspace forbids `unsafe`, so
    /// selection is tested with explicit inputs, without mutating the harness.
    #[test]
    fn the_opt_out_is_honoured_and_a_command_is_built_for_this_platform() {
        if std::env::var_os(NO_BROWSER_ENV).is_some() {
            assert!(!open("https://example.com"), "the opt-out wins");
            return;
        }
        let command = command("https://example.com").expect("a command for this platform");
        let arguments: Vec<_> = command.get_args().collect();
        assert!(
            arguments.iter().any(|a| *a == "https://example.com"),
            "{arguments:?}"
        );
    }
}

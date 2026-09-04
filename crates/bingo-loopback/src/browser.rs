//! Opening a URL in the person's browser, best effort.
//!
//! Never awaited, and what `false` means is the caller's to decide: a login
//! shows the URL through the prompter and carries on, while a page nobody can
//! open is a call that fails at once (ADR-0042 §4). Either way a machine with
//! no browser — a container, an ssh session — still has the URL.

use std::process::{Command, Stdio};

/// Set to anything to keep a browser from opening; a test sets it so no run
/// ever steals the screen.
pub const NO_BROWSER_ENV: &str = "BINGO_NO_BROWSER";

/// Whether a browser was asked to open the URL. `false` is the caller's cue
/// to lean on the URL it is already showing.
pub fn open(url: &str) -> bool {
    if std::env::var_os(NO_BROWSER_ENV).is_some() {
        return false;
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

    /// `std::env::set_var` is unsafe and this workspace forbids `unsafe`, so
    /// the opt-out is exercised through the variable the harness itself sets
    /// when it is present, and through the command table otherwise.
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

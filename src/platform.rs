//! Platform abstraction: shell execution, process-tree termination, TTY query.
//!
//! Unix and Windows differ in process semantics:
//! - Unix spawns the child shell as its own process-group leader (pgid == shell pid);
//!   timeout/cancel kills the whole group so grandchild processes are not orphaned.
//! - Windows has no process groups; `taskkill /T` terminates the whole process tree.
//! - Default shell: macOS `/bin/zsh`, other Unix `/bin/bash`, Windows `powershell.exe`;
//!   overridable via `settings.shell` (see `init_shell`).

use std::path::Path;
use std::sync::OnceLock;

/// Process-wide shell selection: `settings.shell` wins, otherwise the platform default.
/// Settled once at startup — the shell never changes mid-process.
static SHELL: OnceLock<String> = OnceLock::new();

/// Resolve the shell used for Bash tool and hooks. Must be called once at startup
/// with the settings value; before that (tests) the platform default applies.
pub fn init_shell(configured: Option<&str>) {
    let _ = SHELL.set(
        configured
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| default_shell().to_string()),
    );
}

pub fn shell() -> &'static str {
    SHELL
        .get()
        .map(String::as_str)
        .unwrap_or_else(default_shell)
}

pub fn default_shell() -> &'static str {
    #[cfg(windows)]
    {
        "powershell.exe"
    }
    #[cfg(all(unix, target_os = "macos"))]
    {
        "/bin/zsh"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "/bin/bash"
    }
}

/// PowerShell-family shells take `-Command` instead of the POSIX `-c` flag.
/// Only the Windows branch consults this; the test keeps it exercised everywhere.
/// Splits on both separators so Windows paths parse the same on Unix hosts.
#[cfg(any(windows, test))]
fn is_powershell(shell: &str) -> bool {
    let last = shell.rsplit(['/', '\\']).next().unwrap_or(shell);
    let base = last
        .strip_suffix(".exe")
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| last.to_ascii_lowercase());
    base == "powershell" || base == "pwsh"
}

/// Build a Command running `command` through the current shell in `cwd`.
/// stdio is left for the caller (Bash: stdin null + pipes; hooks: all pipes).
/// On Unix the child gets its own process group — the basis for tree termination.
pub fn shell_command(command: &str, cwd: &Path) -> tokio::process::Command {
    let shell = shell();
    let mut std_command = std::process::Command::new(shell);
    std_command.current_dir(cwd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // New process group: pgid == child shell pid; grandchildren land in the same group.
        std_command.arg("-c").arg(command).process_group(0);
    }
    #[cfg(windows)]
    {
        if is_powershell(shell) {
            // -NonInteractive: never pop prompts; -ExecutionPolicy Bypass: scripts run unchecked.
            std_command
                .arg("-NoProfile")
                .arg("-NonInteractive")
                .arg("-ExecutionPolicy")
                .arg("Bypass")
                .arg("-Command")
                .arg(command);
        } else {
            // A configured POSIX-style shell (e.g. Git Bash's bash.exe) takes -c.
            std_command.arg("-c").arg(command);
        }
    }
    tokio::process::Command::from(std_command)
}

/// Terminate the whole process tree rooted at `pid` (best effort).
/// Unix: kill the process group via `/bin/sh`'s built-in kill with a negative pid
/// (AGENTS.md forbids unsafe, so killpg(2) is not used). Windows: `taskkill /T /F`.
pub async fn kill_process_tree(pid: u32) {
    #[cfg(unix)]
    {
        let script = format!("kill -TERM -{pid} 2>/dev/null; kill -KILL -{pid} 2>/dev/null");
        let _ = tokio::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    }
    #[cfg(windows)]
    {
        let _ = tokio::process::Command::new("taskkill")
            .arg("/PID")
            .arg(pid.to_string())
            .arg("/T")
            .arg("/F")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    }
}

/// Read+Write trait object: a `dyn TtyIo` box is the only way to return either
/// a Unix file or nothing while keeping the caller generic.
pub(crate) trait TtyIo: std::io::Read + std::io::Write {}
impl<T: std::io::Read + std::io::Write> TtyIo for T {}

/// Open the controlling terminal non-blocking (Unix `/dev/tty`).
/// Windows has no equivalent for the query protocol — returns None and theme
/// detection falls back safely (same path as a missing tty on Unix).
pub fn open_tty() -> Option<Box<dyn TtyIo>> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/tty")
            .ok()
            .map(|f| Box::new(f) as Box<dyn TtyIo>)
    }
    #[cfg(windows)]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shell_is_platform_appropriate() {
        let s = default_shell();
        assert!(!s.is_empty());
        #[cfg(windows)]
        assert_eq!(s, "powershell.exe");
        #[cfg(all(unix, target_os = "macos"))]
        assert_eq!(s, "/bin/zsh");
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(s, "/bin/bash");
    }

    #[test]
    fn powershell_detection() {
        for (shell, is_ps) in [
            ("powershell.exe", true),
            ("pwsh", true),
            ("C:\\Program Files\\PowerShell\\7\\pwsh.exe", true),
            ("/bin/bash", false),
            ("bash", false),
        ] {
            assert_eq!(is_powershell(shell), is_ps, "{shell}");
        }
    }

    #[test]
    fn init_shell_sets_once_and_ignores_later_calls() {
        init_shell(Some("  "));
        let first = shell();
        assert_eq!(
            first,
            default_shell(),
            "blank config falls back to the platform default"
        );
        init_shell(Some("/bin/custom-sh"));
        assert_eq!(
            shell(),
            first,
            "OnceLock semantics: first set wins, later calls are ignored"
        );
        init_shell(None);
        assert_eq!(shell(), first);
    }

    #[test]
    fn shell_command_uses_shell_and_cwd() {
        let cmd = shell_command("echo hi", Path::new("/"));
        let program = cmd.as_std().get_program().to_string_lossy().to_string();
        assert_eq!(program, shell());
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().to_string())
            .collect();
        assert_eq!(cmd.as_std().get_current_dir(), Some(Path::new("/")));
        #[cfg(windows)]
        assert!(
            args.iter().any(|a| a == "-Command"),
            "PowerShell uses -Command: {args:?}"
        );
        #[cfg(not(windows))]
        assert_eq!(args.first().map(String::as_str), Some("-c"));
    }
}

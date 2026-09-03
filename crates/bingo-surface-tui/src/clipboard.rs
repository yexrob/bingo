//! A picture on the system clipboard, read the way Claude Code reads it: by
//! asking the platform's own tool, so nothing links against a display
//! server. The terminal's paste (`ctrl+shift+v`, `cmd+v`) still delivers text
//! through bracketed paste; this is the other half, behind `ctrl+v`.
//!
//! Every road ends in a PNG file under the temp directory that is read once
//! and removed: a file is the one shape all three tools can be told to
//! produce, and it keeps a five-megabyte picture off a pipe nobody drains.
//! No tool, an empty clipboard, or a clipboard holding words is `None` — a
//! paste with nothing to paste is not an error.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long a clipboard tool may take before the paste is given up on: a
/// display server that never answers must not hold the composer.
const PATIENCE: Duration = Duration::from_secs(5);

/// The PNG bytes on the clipboard, when there is a picture on it.
pub fn image() -> Option<Vec<u8>> {
    let file = scratch();
    let bytes = fetch(&file).then(|| fs::read(&file).ok()).flatten();
    let _ = fs::remove_file(&file);
    bytes.filter(|bytes| !bytes.is_empty())
}

fn scratch() -> PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("bingo-paste-{}-{unique}.png", std::process::id()))
}

/// Run one tool to completion, or kill it once patience runs out. `true` is
/// an exit status of zero — the tool's own word that it had a picture.
fn bounded(mut command: Command) -> bool {
    let Ok(mut child) = command.stdin(Stdio::null()).stderr(Stdio::null()).spawn() else {
        return false;
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < PATIENCE => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn fetch(file: &Path) -> bool {
    // AppleScript is the one way to ask for a PNG rendering without a
    // framework; a clipboard with no picture on it errors inside the `try`
    // and leaves the file empty, which `image` reads as nothing.
    let path = file
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let script = [
        format!("set f to open for access POSIX file \"{path}\" with write permission"),
        "try".to_string(),
        "write (the clipboard as «class PNGf») to f".to_string(),
        "end try".to_string(),
        "close access f".to_string(),
    ];
    let mut command = Command::new("osascript");
    for line in &script {
        command.arg("-e").arg(line);
    }
    command.stdout(Stdio::null());
    bounded(command)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn fetch(file: &Path) -> bool {
    // Wayland first, then X11; each tool writes the picture to its stdout, so
    // stdout is the file. A tool that is not installed fails to spawn and the
    // next is tried.
    let attempts: [(&str, &[&str]); 2] = [
        ("wl-paste", &["-t", "image/png"]),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        ),
    ];
    attempts.into_iter().any(|(tool, args)| {
        let Ok(out) = fs::File::create(file) else {
            return false;
        };
        let mut command = Command::new(tool);
        command.args(args).stdout(out);
        bounded(command)
    })
}

#[cfg(windows)]
fn fetch(file: &Path) -> bool {
    // The clipboard is a single-threaded-apartment API; `-STA` is what makes
    // `GetImage` answer from a script.
    let path = file.to_string_lossy().replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $i = [System.Windows.Forms.Clipboard]::GetImage(); \
         if ($i -eq $null) {{ exit 1 }}; \
         $i.Save('{path}', [System.Drawing.Imaging.ImageFormat]::Png)"
    );
    let mut command = Command::new("powershell");
    command
        .args(["-NoProfile", "-NonInteractive", "-STA", "-Command", &script])
        .stdout(Stdio::null());
    bounded(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_that_does_not_exist_is_no_picture() {
        let mut command = Command::new("bingo-no-such-clipboard-tool");
        command.stdout(Stdio::null());
        assert!(!bounded(command));
    }

    #[test]
    fn a_scratch_file_that_was_never_written_reads_as_nothing() {
        let file = scratch();
        assert!(!file.exists());
        assert_eq!(
            fs::read(&file).ok().filter(|b| !b.is_empty()),
            None,
            "an absent file is the same nothing as an empty one"
        );
    }
}

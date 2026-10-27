//! Extracted helper source and its process-lifetime worker.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

mod worker;

const SOURCE: &str = include_str!("../../../../../scripts/windows-image-paste.cs");
static NEXT: AtomicU64 = AtomicU64::new(0);

pub(in crate::terminal) struct Companion {
    worker: Option<worker::Worker>,
    directory: PathBuf,
    marker: String,
}

impl Companion {
    pub(super) fn begin(&mut self) -> io::Result<std::sync::mpsc::Receiver<bool>> {
        let mut command = system_powershell()?;
        command
            .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
            .arg(compile_command(
                &self.directory.join("companion.cs"),
                std::process::id(),
                self.marker(),
            ));
        let (worker, ready) = worker::Worker::spawn(command, worker::STARTUP)?;
        self.worker = Some(worker);
        Ok(ready)
    }

    pub(super) fn prepare() -> io::Result<Self> {
        let marker = marker()?;
        let directory = std::env::temp_dir().join(&marker);
        // create_dir (not create_dir_all) refuses an existing run's directory.
        fs::create_dir(&directory)?;
        let companion = Self {
            worker: None,
            directory,
            marker,
        };
        let mut source = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(companion.directory.join("companion.cs"))?;
        source.write_all(SOURCE.as_bytes())?;
        Ok(companion)
    }

    pub(in crate::terminal) fn marker(&self) -> &str {
        &self.marker
    }
}

impl Drop for Companion {
    fn drop(&mut self) {
        drop(self.worker.take());
        let _ = fs::remove_file(self.directory.join("companion.cs"));
        let _ = fs::remove_dir(&self.directory);
    }
}

pub(super) fn available(ready: std::sync::mpsc::Receiver<bool>) -> bool {
    ready.recv_timeout(worker::STARTUP).unwrap_or(false)
}

fn system_powershell() -> io::Result<Command> {
    let root = std::env::var_os("SystemRoot")
        .ok_or_else(|| io::Error::other("Windows system directory unavailable"))?;
    Ok(Command::new(
        PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe"),
    ))
}

fn compile_command(path: &std::path::Path, owner: u32, marker: &str) -> String {
    let path = path.to_string_lossy().replace('\'', "''");
    let marker = marker.replace('\'', "''");
    format!(
        "$ErrorActionPreference='Stop'; \
         $f=[System.Runtime.InteropServices.RuntimeEnvironment]::GetRuntimeDirectory(); \
         Add-Type -TypeDefinition ([IO.File]::ReadAllText('{path}')) \
         -ReferencedAssemblies @('System.dll',($f+'WPF/UIAutomationClient.dll'),\
         ($f+'WPF/UIAutomationTypes.dll'),($f+'WPF/WindowsBase.dll')); \
         exit [Bingo.ImagePaste.Companion]::Start({owner},'{marker}')"
    )
}

fn marker() -> io::Result<String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    Ok(format!(
        "[bingo-image-paste-{}-{nanos:x}-{:x}]",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_are_unique_ascii_and_each_run_owns_its_temporary_source() {
        let first = Companion::prepare().unwrap();
        let second = Companion::prepare().unwrap();
        assert_ne!(first.marker(), second.marker());
        assert!(first.marker().is_ascii());
        let directory = first.directory.clone();
        assert_eq!(
            fs::read_to_string(directory.join("companion.cs")).unwrap(),
            SOURCE
        );
        drop(first);
        assert!(!directory.exists());
        assert!(second.directory.exists());
    }

    #[test]
    #[ignore = "isolated disabled/headless startup fixture"]
    fn disabled_fixture() {
        crate::terminal::image_paste::start(|_| panic!("disabled start wrote a title"));
        assert!(
            crate::terminal::image_paste::COMPANION
                .lock()
                .unwrap()
                .is_none()
        );
        assert!(fs::read_dir(std::env::temp_dir()).unwrap().next().is_none());
    }

    #[test]
    fn actual_disabled_and_headless_start_leave_no_companion_source_or_title() {
        for value in [None, Some("0"), Some("1")] {
            let directory = tempfile::tempdir().unwrap();
            let module = module_path!().split_once("::").unwrap().1;
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    &format!("{module}::disabled_fixture"),
                    "--ignored",
                ])
                .env("TMP", directory.path())
                .env("TEMP", directory.path())
                .env_remove("BINGO_WINDOWS_IMAGE_PASTE");
            if let Some(value) = value {
                command.env("BINGO_WINDOWS_IMAGE_PASTE", value);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "disabled fixture: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(fs::read_dir(directory.path()).unwrap().next().is_none());
        }
    }

    fn install_fake_companion() -> PathBuf {
        let mut companion = Companion::prepare().unwrap();
        let directory = companion.directory.clone();
        let mut command = system_powershell().unwrap();
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
            "[Console]::Out.WriteLine('BINGO_IMAGE_PASTE_READY_V1'); [Console]::In.ReadToEnd() | Out-Null"]);
        companion.worker = Some(worker::Worker::start(command).unwrap());
        *crate::terminal::image_paste::COMPANION.lock().unwrap() = Some(companion);
        crate::terminal::ENTERED.store(true, Ordering::SeqCst);
        directory
    }

    fn restore_during_startup() {
        use std::sync::mpsc;
        use std::time::Duration;
        let helper = Companion::prepare().unwrap();
        let directory = helper.directory.clone();
        crate::terminal::ENTERED.store(true, Ordering::SeqCst);
        let (stopping, stopped) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (marker, ready) = super::super::publish(helper, |helper| {
            let (worker, ready) = worker::waiting_fixture(stopping, released);
            helper.worker = Some(worker);
            Ok(ready)
        })
        .unwrap();
        let (returned, restored) = mpsc::channel();
        let restoring = std::thread::spawn(move || {
            crate::terminal::restore();
            returned.send(()).unwrap();
        });
        stopped.recv_timeout(Duration::from_secs(8)).unwrap();
        assert!(
            matches!(restored.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "restore returned before startup worker stopped"
        );
        release.send(()).unwrap();
        restored.recv_timeout(Duration::from_secs(8)).unwrap();
        restoring.join().unwrap();
        assert!(!directory.exists());
        assert!(!available(ready));
        assert!(
            super::super::finish(&marker, true, |_| panic!("post-restore marker write")).is_err()
        );
        let helper = Companion::prepare().unwrap();
        let directory = helper.directory.clone();
        assert!(super::super::publish(helper, |_| panic!("post-restore helper launch")).is_err());
        assert!(!directory.exists());
    }

    fn worker_originated_panic() {
        use std::time::Duration;
        let mut helper = Companion::prepare().unwrap();
        let directory = helper.directory.clone();
        let mut command = system_powershell().unwrap();
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
            "[Console]::Out.WriteLine('BINGO_IMAGE_PASTE_READY_V1'); [Console]::In.ReadToEnd() | Out-Null"]);
        let (worker, ready, trigger, finished) = worker::panic_fixture(command);
        assert!(available(ready));
        helper.worker = Some(worker);
        *crate::terminal::image_paste::COMPANION.lock().unwrap() = Some(helper);
        crate::terminal::ENTERED.store(true, Ordering::SeqCst);
        trigger.send(()).unwrap();
        finished.recv_timeout(Duration::from_secs(8)).unwrap();
        assert!(!crate::terminal::ENTERED.load(Ordering::SeqCst));
        assert!(
            crate::terminal::image_paste::COMPANION
                .lock()
                .unwrap()
                .is_none()
        );
        assert!(!directory.exists());
    }

    #[test]
    #[ignore = "isolated subprocess fixture for real terminal restore and panic hook"]
    fn restore_fixture() {
        crate::terminal::install_hook();
        let directory = install_fake_companion();
        crate::terminal::restore();
        assert!(!directory.exists());
        let directory = install_fake_companion();
        assert!(std::panic::catch_unwind(|| panic!("caught TUI panic fixture")).is_err());
        assert!(!directory.exists());
        let directory = install_fake_companion();
        assert!(
            std::thread::spawn(|| panic!("other-thread panic fixture"))
                .join()
                .is_err()
        );
        assert!(!directory.exists());
        assert!(
            crate::terminal::image_paste::COMPANION
                .lock()
                .unwrap()
                .is_none()
        );
        restore_during_startup();
        worker_originated_panic();
    }

    #[test]
    fn real_restore_covers_startup_race_and_worker_originated_panic() {
        use std::os::windows::process::CommandExt;
        let module = module_path!().split_once("::").unwrap().1;
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("{module}::restore_fixture"),
                "--ignored",
                "--nocapture",
            ])
            .creation_flags(0x0800_0000)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "restore fixture: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn compiler_command_quotes_paths_without_policy_or_profile_changes() {
        let command = compile_command(
            std::path::Path::new("D:/a b/it's/companion.cs"),
            42,
            "[marker]",
        );
        assert!(command.contains("ReadAllText('D:/a b/it''s/companion.cs')"));
        assert!(command.ends_with("::Start(42,'[marker]')"));
        assert!(!command.contains("ExecutionPolicy"));
        assert!(!command.contains("Invoke-Expression"));
    }
}

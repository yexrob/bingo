//! A JobObject owns PowerShell before any compiler or managed code executes.

use std::io;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::oneshot;

pub(super) const STARTUP: Duration = Duration::from_secs(8);

pub(super) struct Worker {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    #[cfg(test)]
    pub(super) fn start(command: Command) -> io::Result<Self> {
        Self::launch(command, STARTUP)
    }

    #[cfg(test)]
    fn launch(command: Command, timeout: Duration) -> io::Result<Self> {
        let (worker, ready) = Self::spawn(command, timeout)?;
        if !ready.recv_timeout(timeout).unwrap_or(false) {
            return Err(io::Error::other("helper readiness unavailable"));
        }
        Ok(worker)
    }

    pub(super) fn spawn(
        command: Command,
        timeout: Duration,
    ) -> io::Result<(Self, mpsc::Receiver<bool>)> {
        let (ready, rx) = mpsc::sync_channel(1);
        let (stop, stopped) = oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("image-paste-lifetime".into())
            .spawn(move || {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    runtime.block_on(own(command, ready, stopped, timeout));
                }
            })?;
        let worker = Self {
            stop: Some(stop),
            thread: Some(thread),
        };
        Ok((worker, rx))
    }

    fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take()
            && thread.thread().id() != std::thread::current().id()
        {
            let _ = thread.join();
        }
        // The panic hook may stop this worker on its own thread. Its stack
        // still owns Tree; unwinding releases the JobObject after the hook.
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn own(
    mut command: Command,
    ready: mpsc::SyncSender<bool>,
    mut stopped: oneshot::Receiver<()>,
    timeout: Duration,
) {
    use std::os::windows::process::CommandExt;
    command
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW; helper attaches explicitly.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // Tree creates the child suspended, assigns its kill-on-close JobObject,
    // then resumes it. Owner death during Add-Type needs no managed watcher.
    let Ok(mut child) = bingo_process::tree(command.into()) else {
        return;
    };
    // Tokio's wait closes stdin still owned by Child. Keep the lifetime pipe
    // outside it so observing process exit cannot shut down a ready helper.
    let lifetime = child.take_stdin();
    let available = match child.take_stdout() {
        Some(stdout) => tokio::select! {
            _ = &mut stopped => false,
            ready = tokio::time::timeout(timeout, read_ready(stdout)) => ready.unwrap_or(false),
        },
        None => false,
    };
    let _ = ready.send(available);
    if available {
        tokio::select! {
            _ = &mut stopped => {},
            _ = child.wait() => return,
        }
    }
    drop(lifetime);
    if tokio::time::timeout(Duration::from_millis(200), child.wait())
        .await
        .is_err()
    {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

async fn read_ready(mut stdout: impl AsyncRead + Unpin) -> bool {
    let mut line = Vec::new();
    for _ in 0..64 {
        let mut byte = [0];
        if stdout.read_exact(&mut byte).await.is_err() {
            return false;
        }
        line.push(byte[0]);
        if byte[0] == b'\n' {
            return crate::terminal::image_paste::ready(&line);
        }
    }
    false
}

#[cfg(test)]
pub(super) use fixtures::{panic_fixture, waiting_fixture};

#[cfg(test)]
mod fixtures {
    use super::*;

    pub(in crate::terminal::image_paste::companion) fn waiting_fixture(
        stopping: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    ) -> (Worker, mpsc::Receiver<bool>) {
        let (ready, receiver) = mpsc::sync_channel(1);
        let (stop, stopped) = oneshot::channel();
        let thread = std::thread::spawn(move || {
            let _ = stopped.blocking_recv();
            let _ = stopping.send(());
            let _ = release.recv_timeout(STARTUP);
            let _ = ready.send(false);
        });
        (
            Worker {
                stop: Some(stop),
                thread: Some(thread),
            },
            receiver,
        )
    }

    pub(in crate::terminal::image_paste::companion) fn panic_fixture(
        mut command: Command,
    ) -> (
        Worker,
        mpsc::Receiver<bool>,
        mpsc::Sender<()>,
        mpsc::Receiver<()>,
    ) {
        use std::os::windows::process::CommandExt;
        command
            .creation_flags(0x0800_0000)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let (ready, receiver) = mpsc::sync_channel(1);
        let (trigger, triggered) = mpsc::channel();
        let (finished, done) = mpsc::channel();
        let (stop, _stopped) = oneshot::channel();
        let thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runtime.block_on(async {
                    let mut child = bingo_process::tree(command.into()).unwrap();
                    let stdout = child.take_stdout().unwrap();
                    let available = tokio::time::timeout(STARTUP, read_ready(stdout))
                        .await
                        .unwrap();
                    ready.send(available).unwrap();
                    triggered.recv_timeout(STARTUP).unwrap();
                    panic!("lifetime worker panic fixture");
                })
            }));
            drop(runtime);
            let _ = finished.send(());
        });
        (
            Worker {
                stop: Some(stop),
                thread: Some(thread),
            },
            receiver,
            trigger,
            done,
        )
    }
}

#[cfg(test)]
mod startup_tests;

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn fake_helper(body: &str) -> Command {
        let mut command = super::super::system_powershell().unwrap();
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", body]);
        command
    }

    #[tokio::test]
    async fn readiness_is_bounded_and_eof_or_long_output_is_unavailable() {
        assert!(read_ready(crate::terminal::image_paste::READY).await);
        assert!(!read_ready(&b"warning\n"[..]).await);
        assert!(!read_ready(&b"BINGO_IMAGE_PASTE_READY_V1"[..]).await);
        assert!(!read_ready(&[b'x'; 65][..]).await);
    }

    #[test]
    fn failed_readiness_stops_the_worker_and_running_child() {
        let command =
            fake_helper("[Console]::Out.WriteLine('wrong'); [Console]::In.ReadToEnd() | Out-Null");
        assert!(Worker::start(command).is_err());
    }

    #[test]
    fn ready_child_stops_before_its_source_is_removed() {
        let mut companion = super::super::Companion::prepare().unwrap();
        let directory = companion.directory.clone();
        let command = fake_helper(
            "[Console]::Out.WriteLine('BINGO_IMAGE_PASTE_READY_V1'); [Console]::In.ReadToEnd() | Out-Null",
        );
        let mut worker = Worker::start(command).unwrap();
        worker.stop();
        assert!(worker.thread.is_none());
        companion.worker = Some(worker);
        assert!(directory.join("companion.cs").exists());
        drop(companion);
        assert!(!directory.exists());
    }

    fn persistence_helper(directory: &std::path::Path) -> Command {
        let source = r#"$ErrorActionPreference='Stop'
Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Threading.Tasks;
public static class PipeLifetimeFixture {
    public static void Run(string request, string alive, string eof) {
        using (Stream input = Console.OpenStandardInput()) {
            Task<int> reading = Task.Run(() => {
                int value = input.ReadByte();
                if (value == -1) File.WriteAllText(eof, "EOF");
                return value;
            });
            Console.Out.WriteLine("BINGO_IMAGE_PASTE_READY_V1");
            Console.Out.Flush();
            DateTime deadline = DateTime.UtcNow.AddSeconds(20);
            while (!File.Exists(request)) {
                if (reading.Wait(25)) throw new Exception("EOF before post-ready request");
                if (DateTime.UtcNow >= deadline) throw new Exception("request unavailable");
            }
            // Observe the live pipe across four production helper timer ticks.
            // This is a persistence property, not a response-time assertion.
            if (reading.Wait(1000)) throw new Exception("EOF before explicit stop");
            File.WriteAllText(alive, "alive after readiness");
            reading.Wait();
            if (reading.Result != -1) throw new Exception("unexpected stdin data");
        }
    }
}
'@
"#;
        let quote = |name: &str| directory.join(name).to_string_lossy().replace('\'', "''");
        fake_helper(&format!(
            "{source}[PipeLifetimeFixture]::Run('{}','{}','{}')",
            quote("request"),
            quote("alive"),
            quote("eof"),
        ))
    }

    #[test]
    fn ready_worker_retains_lifetime_pipe_until_explicit_stop() {
        let directory = tempfile::tempdir().unwrap();
        let mut worker = Worker::start(persistence_helper(directory.path())).unwrap();
        // The request can only be issued after the actual readiness handshake.
        std::fs::write(directory.path().join("request"), b"post-ready").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !directory.path().join("alive").exists() {
            assert!(
                !directory.path().join("eof").exists(),
                "wait closed lifetime stdin after readiness"
            );
            assert!(
                !worker.thread.as_ref().unwrap().is_finished(),
                "ready helper exited before persistence acknowledgment"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "post-ready acknowledgment unavailable"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!directory.path().join("eof").exists());
        worker.stop();
        assert!(worker.thread.is_none());
        assert_eq!(
            std::fs::read_to_string(directory.path().join("eof")).unwrap(),
            "EOF",
            "explicit stop must deliver EOF before the kill fallback"
        );
    }

    #[test]
    fn silent_startup_times_out_and_cancels_the_worker() {
        let command = fake_helper("[Console]::In.ReadToEnd() | Out-Null");
        assert!(Worker::launch(command, Duration::ZERO).is_err());
    }
}

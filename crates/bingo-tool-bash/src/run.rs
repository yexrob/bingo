//! The process a command runs in.
//!
//! The shell is spawned as its own process-group leader, so a timeout or an
//! interrupt reaches every grandchild instead of orphaning the ones the shell
//! started. Its stdin is `/dev/null`, so nothing it runs can wait for a person.
//! Its stdout and stderr go into one buffer in arrival order, because a
//! command's error lines are part of its story and the model reads them where
//! they happened.
//!
//! POSIX only in M1 (the plan's non-goals put Windows dialects in M6). A Windows
//! port replaces two things and nothing else: [`shell`], which would resolve
//! `powershell -Command`, and the process group in [`spawn`], which would become
//! `process-wrap`'s job object.

use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use bingo_sdk::{ToolContext, ToolError};
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop, ProcessGroup};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::output::{Bounded, Ended, MAX_OUTPUT_CHARS};
use crate::tail::{self, Tail};

/// How long the pipes are given once the process is gone. A grandchild that
/// inherited them and outlived the kill must not hold the turn open.
const DRAIN: Duration = Duration::from_secs(2);

/// The shell every command runs under. `bash` is what the tool is named after
/// and what the model writes for; `sh` is the fallback where there is no bash.
/// Settled once: the shell does not change while the process lives.
pub fn shell() -> &'static str {
    static SHELL: OnceLock<&'static str> = OnceLock::new();
    SHELL.get_or_init(|| {
        if Path::new("/bin/bash").exists() {
            "/bin/bash"
        } else {
            "/bin/sh"
        }
    })
}

/// What a command left behind: everything it wrote, within the cap, and the way
/// it ended.
#[derive(Debug)]
pub struct Run {
    pub output: String,
    pub ended: Ended,
}

/// Run one command to its end, to the timeout, or to the interrupt.
pub async fn run(command: &str, timeout: Duration, cx: &ToolContext) -> Result<Run, ToolError> {
    let mut child = spawn(command, &cx.cwd)?;
    let output = Arc::new(Mutex::new(Bounded::new(MAX_OUTPUT_CHARS)));
    let readers = read_pipes(child.as_mut(), &output);
    let ended = watch(&mut child, timeout, cx, &output).await?;
    drain(readers).await;
    let output = output.lock().await.finish();
    Ok(Run { output, ended })
}

fn spawn(command: &str, cwd: &Path) -> Result<Box<dyn ChildWrapper>, ToolError> {
    let mut shell_command = Command::new(shell());
    shell_command
        .arg("-c")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut wrapped = CommandWrap::from(shell_command);
    wrapped.wrap(ProcessGroup::leader());
    wrapped.wrap(KillOnDrop);
    wrapped
        .spawn()
        .map_err(|e| ToolError::Failed(format!("could not run {}: {e}", shell())))
}

/// How the wait ended, before anything is done about it.
enum Stop {
    Exited(ExitStatus),
    Timeout,
    Interrupted,
}

/// Wait for the command, sampling the tail on the way, and stop for whichever
/// comes first: the exit, the deadline, or the turn's interrupt.
async fn watch(
    child: &mut Box<dyn ChildWrapper>,
    timeout: Duration,
    cx: &ToolContext,
    output: &Mutex<Bounded>,
) -> Result<Ended, ToolError> {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut tail = Tail::default();
    loop {
        let stop = tokio::select! {
            status = child.wait() => Stop::Exited(
                status.map_err(|e| ToolError::Failed(format!("waiting for the command: {e}")))?,
            ),
            () = tokio::time::sleep_until(deadline) => Stop::Timeout,
            () = cx.cancel.cancelled() => Stop::Interrupted,
            () = tokio::time::sleep(tail::INTERVAL) => {
                tail.sample(output, cx).await;
                continue;
            }
        };
        return settle(child, stop, timeout).await;
    }
}

/// An exit needs nothing; anything else kills the group first. The kernel keeps
/// an interrupted `Block` tool's real output, so an interrupt is reported as
/// what the command had produced, not as a cancellation.
async fn settle(
    child: &mut Box<dyn ChildWrapper>,
    stop: Stop,
    timeout: Duration,
) -> Result<Ended, ToolError> {
    match stop {
        // A shell killed by a signal left no status of its own to report.
        Stop::Exited(status) => Ok(Ended::Exited(status.code().unwrap_or(-1))),
        Stop::Timeout => {
            kill(child).await;
            Ok(Ended::Timeout {
                after_ms: timeout.as_millis().try_into().unwrap_or(u64::MAX),
            })
        }
        Stop::Interrupted => {
            kill(child).await;
            Ok(Ended::Interrupted)
        }
    }
}

/// `SIGKILL` to the whole group — the wrapper's `start_kill` is a `killpg`, so a
/// grandchild that outlived its parent goes with it — then reap what is left,
/// bounded so an escapee cannot hold the turn.
async fn kill(child: &mut Box<dyn ChildWrapper>) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(DRAIN, child.wait()).await;
}

type Reader = JoinHandle<()>;

/// One task per pipe, both writing into the same buffer, so stdout and stderr
/// interleave in the order they arrived.
fn read_pipes(child: &mut dyn ChildWrapper, output: &Arc<Mutex<Bounded>>) -> Vec<Reader> {
    let pipes = [
        child.stdout().take().map(boxed),
        child.stderr().take().map(boxed),
    ];
    pipes
        .into_iter()
        .flatten()
        .map(|pipe| tokio::spawn(pump(pipe, output.clone())))
        .collect()
}

type Pipe = Box<dyn AsyncRead + Unpin + Send>;

fn boxed(pipe: impl AsyncRead + Unpin + Send + 'static) -> Pipe {
    Box::new(pipe)
}

/// Read one pipe to its end, decoding as the bytes arrive.
async fn pump(mut pipe: Pipe, output: Arc<Mutex<Bounded>>) {
    let mut buffer = [0u8; 8 * 1024];
    let mut stream = Utf8Stream::default();
    loop {
        let read = match pipe.read(&mut buffer).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        let text = stream.decode(&buffer[..read]);
        if !text.is_empty() {
            output.lock().await.push(&text);
        }
    }
}

/// Bytes arriving in whatever chunks the pipe hands over, out as text. A
/// character split across two reads waits for the second one instead of
/// becoming two replacements.
#[derive(Debug, Default)]
struct Utf8Stream {
    pending: Vec<u8>,
}

impl Utf8Stream {
    fn decode(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut text = String::new();
        loop {
            let error = match std::str::from_utf8(&self.pending) {
                Ok(all) => {
                    text.push_str(all);
                    self.pending.clear();
                    return text;
                }
                Err(error) => error,
            };
            let valid = error.valid_up_to();
            text.push_str(&String::from_utf8_lossy(&self.pending[..valid]));
            let Some(broken) = error.error_len() else {
                // The chunk ended mid-character: keep it for the next read.
                self.pending.drain(..valid);
                return text;
            };
            text.push(char::REPLACEMENT_CHARACTER);
            self.pending.drain(..valid + broken);
        }
    }
}

/// Let the readers finish what the pipes already hold, then let them go.
async fn drain(readers: Vec<Reader>) {
    let deadline = tokio::time::Instant::now() + DRAIN;
    for mut reader in readers {
        if tokio::time::timeout_at(deadline, &mut reader)
            .await
            .is_err()
        {
            reader.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{context, context_in};

    use std::path::PathBuf;
    use std::time::Instant;

    const NO_TIMEOUT: Duration = Duration::from_secs(30);

    async fn bash(command: &str) -> Run {
        let (_host, cx) = context();
        run(command, NO_TIMEOUT, &cx)
            .await
            .expect("the command ran")
    }

    #[tokio::test]
    async fn a_command_comes_back_with_what_it_wrote() {
        let out = bash("echo hi").await;
        assert_eq!(out.output, "hi\n");
        assert_eq!(out.ended, Ended::Exited(0));
    }

    #[tokio::test]
    async fn the_exit_code_is_the_command_s_own() {
        assert_eq!(bash("exit 3").await.ended, Ended::Exited(3));
        assert_eq!(bash("false").await.ended, Ended::Exited(1));
    }

    #[tokio::test]
    async fn stderr_arrives_with_stdout() {
        let out = bash("echo out; echo err >&2").await;
        assert!(out.output.contains("out"), "{:?}", out.output);
        assert!(out.output.contains("err"), "{:?}", out.output);
    }

    #[tokio::test]
    async fn the_command_runs_in_the_call_s_working_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("marker"), "").expect("write");
        let (_host, cx) = context_in(dir.path().to_path_buf());
        let out = run("ls", NO_TIMEOUT, &cx).await.expect("the command ran");
        assert_eq!(out.output, "marker\n");
    }

    #[tokio::test]
    async fn a_directory_that_is_not_there_fails_to_spawn() {
        let (_host, cx) = context_in(PathBuf::from("/no/such/directory/here"));
        let error = run("echo hi", NO_TIMEOUT, &cx).await.err();
        assert!(
            matches!(&error, Some(ToolError::Failed(m)) if m.starts_with("could not run")),
            "got {error:?}"
        );
    }

    #[tokio::test]
    async fn multibyte_output_survives_the_pipe() {
        // 60 000 bytes: several reads, and the character on every boundary.
        let out = bash("printf '字%.0s' $(seq 1 20000)").await;
        assert_eq!(out.output.chars().filter(|c| *c == '字').count(), 20_000);
        assert!(!out.output.contains(char::REPLACEMENT_CHARACTER));
    }

    #[tokio::test]
    async fn the_timeout_kills_the_whole_group_and_says_so() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ticks = dir.path().join("ticks");
        let (_host, cx) = context();
        let command = format!(
            "(while true; do echo tick >> '{0}'; sleep 0.02; done) & sleep 30",
            ticks.display()
        );

        let started = Instant::now();
        let out = run(&command, Duration::from_millis(200), &cx)
            .await
            .expect("the command ran");
        let elapsed = started.elapsed();

        assert_eq!(out.ended, Ended::Timeout { after_ms: 200 });
        assert!(elapsed < Duration::from_secs(5), "took {elapsed:?}");

        // The grandchild was in the group, so it stopped writing with the shell.
        let after_kill = std::fs::metadata(&ticks).map(|m| m.len()).unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let later = std::fs::metadata(&ticks).map(|m| m.len()).unwrap_or(0);
        assert!(after_kill > 0, "the grandchild never ran");
        assert_eq!(after_kill, later, "the grandchild outlived the group");
    }

    #[tokio::test]
    async fn output_past_the_cap_is_truncated_in_the_middle() {
        let out = bash("yes | head -c 200000").await;
        assert_eq!(out.ended, Ended::Exited(0));
        assert!(out.output.contains("chars truncated"), "no marker");
        assert!(
            out.output.chars().count() < MAX_OUTPUT_CHARS + 100,
            "{} characters",
            out.output.chars().count()
        );
        assert!(out.output.starts_with("y\ny\n"), "the head is missing");
        assert!(out.output.ends_with("y\n"), "the tail is missing");
    }

    #[tokio::test]
    async fn an_interrupt_returns_what_the_command_had_produced() {
        let (_host, cx) = context();
        let cancel = cx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });

        let started = Instant::now();
        let out = run("echo started; sleep 30", NO_TIMEOUT, &cx)
            .await
            .expect("the command ran");

        assert_eq!(out.ended, Ended::Interrupted);
        assert_eq!(out.output, "started\n");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_running_command_streams_its_tail() {
        let (host, cx) = context();
        let out = run(
            "for i in 1 2 3; do echo line $i; sleep 0.15; done",
            NO_TIMEOUT,
            &cx,
        )
        .await
        .expect("the command ran");
        assert_eq!(out.ended, Ended::Exited(0));
        let tails = host.tails();
        assert!(!tails.is_empty(), "no tail went out");
        assert!(
            tails.iter().all(|t| t.starts_with("line 1")),
            "the tail is not the output: {tails:?}"
        );
        assert!(
            tails.last().is_some_and(|t| t.contains("line 2")),
            "the tail never moved: {tails:?}"
        );
    }

    #[test]
    fn the_shell_is_a_real_one() {
        assert!(Path::new(shell()).exists(), "{}", shell());
        assert!(shell().ends_with("bash") || shell().ends_with("sh"));
    }

    #[test]
    fn a_character_split_across_two_reads_is_decoded_once() {
        let mut stream = Utf8Stream::default();
        let bytes = "字".as_bytes();
        assert_eq!(stream.decode(&bytes[..1]), "");
        assert_eq!(stream.decode(&bytes[1..]), "字");
    }

    #[test]
    fn bytes_that_are_not_text_become_one_replacement_each() {
        let mut stream = Utf8Stream::default();
        assert_eq!(stream.decode(b"a\xffb"), "a\u{fffd}b");
    }
}

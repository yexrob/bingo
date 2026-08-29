//! One hook process.
//!
//! The hook is its own process-group leader, so the deadline reaches whatever it
//! started rather than orphaning it. Its stdin is written by a task that runs
//! *beside* the wait, never before it: a 64 KB `tool_input` is larger than a pipe
//! buffer, and a hook that never reads stdin would otherwise deadlock the turn
//! against a write that can never finish. Both pipes are drained to the end and
//! kept up to a cap, so a hook that talks forever cannot grow the process.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop, ProcessGroup};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::task::JoinHandle;

/// How much of each pipe is kept. Stdout is a small JSON verdict and stderr is a
/// sentence; anything past this is a hook misbehaving, and is read and dropped.
const MAX_PIPE_BYTES: usize = 1024 * 1024;

/// How long the pipes are given once the process is gone.
const DRAIN: Duration = Duration::from_secs(2);

/// The shell a hook command runs under. `bash` is what hook commands are written
/// for; `sh` is the fallback where there is no bash. Settled once: the shell does
/// not change while the process lives. (`bingo-tool-bash` resolves the same two
/// paths for the same reason; a plugin may not import another plugin, so the four
/// lines are spelt twice rather than shared.)
fn shell() -> &'static str {
    static SHELL: OnceLock<&'static str> = OnceLock::new();
    SHELL.get_or_init(|| {
        if Path::new("/bin/bash").exists() {
            "/bin/bash"
        } else {
            "/bin/sh"
        }
    })
}

/// One hook, and everything it needs to run.
pub struct Request<'a> {
    pub command: &'a str,
    pub input: &'a Value,
    pub cwd: &'a Path,
    pub timeout: Duration,
    /// Added to the inherited environment.
    pub env: BTreeMap<String, String>,
}

/// What a hook left behind.
#[derive(Debug)]
pub struct Completed {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("could not run {shell}: {source}")]
    Spawn {
        shell: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("timed out after {}ms", .0.as_millis())]
    Timeout(Duration),
    #[error("waiting for the hook: {0}")]
    Wait(std::io::Error),
}

pub async fn run(request: Request<'_>) -> Result<Completed, RunError> {
    let mut child = spawn(&request)?;
    let pipes = Pipes::attach(child.as_mut(), request.input.to_string());
    match tokio::time::timeout(request.timeout, child.wait()).await {
        Ok(status) => {
            let code = status.map_err(RunError::Wait)?.code().unwrap_or(-1);
            Ok(pipes.finish(code).await)
        }
        Err(_) => {
            kill(&mut child).await;
            pipes.abort();
            Err(RunError::Timeout(request.timeout))
        }
    }
}

fn spawn(request: &Request<'_>) -> Result<Box<dyn ChildWrapper>, RunError> {
    let mut command = Command::new(shell());
    command
        .arg("-c")
        .arg(request.command)
        .current_dir(request.cwd)
        .envs(&request.env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut wrapped = CommandWrap::from(command);
    wrapped.wrap(ProcessGroup::leader());
    wrapped.wrap(KillOnDrop);
    wrapped.spawn().map_err(|source| RunError::Spawn {
        shell: shell(),
        source,
    })
}

/// `SIGKILL` to the whole group — the wrapper's `start_kill` is a `killpg` — then
/// reap what is left, bounded so an escapee cannot hold the turn.
async fn kill(child: &mut Box<dyn ChildWrapper>) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(DRAIN, child.wait()).await;
}

/// The three tasks a hook's pipes need, all running while the wait runs.
struct Pipes {
    feed: JoinHandle<()>,
    out: JoinHandle<Vec<u8>>,
    err: JoinHandle<Vec<u8>>,
}

impl Pipes {
    fn attach(child: &mut dyn ChildWrapper, input: String) -> Self {
        let stdin = child.stdin().take();
        let stdout = child.stdout().take();
        let stderr = child.stderr().take();
        Self {
            feed: tokio::spawn(async move {
                let Some(mut stdin) = stdin else { return };
                // A hook that exits without reading closes the pipe; that is not
                // an error, it is the hook not caring what it was told.
                let _ = stdin.write_all(input.as_bytes()).await;
                let _ = stdin.shutdown().await;
            }),
            out: tokio::spawn(read_capped(stdout)),
            err: tokio::spawn(read_capped(stderr)),
        }
    }

    /// Let the readers finish what the pipes already hold, then read the answer.
    async fn finish(self, code: i32) -> Completed {
        self.feed.abort();
        let deadline = tokio::time::Instant::now() + DRAIN;
        let stdout = drain(self.out, deadline).await;
        let stderr = drain(self.err, deadline).await;
        Completed {
            code,
            stdout,
            stderr: stderr.trim().to_string(),
        }
    }

    fn abort(self) {
        self.feed.abort();
        self.out.abort();
        self.err.abort();
    }
}

async fn drain(reader: JoinHandle<Vec<u8>>, deadline: tokio::time::Instant) -> String {
    match tokio::time::timeout_at(deadline, reader).await {
        Ok(Ok(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
        Ok(Err(_)) | Err(_) => String::new(),
    }
}

/// Read a pipe to its end, keeping the first [`MAX_PIPE_BYTES`]. Reading past the
/// cap and dropping the excess is what keeps a chatty hook from blocking on a
/// full pipe until its deadline.
async fn read_capped(pipe: Option<impl AsyncRead + Unpin>) -> Vec<u8> {
    let Some(mut pipe) = pipe else {
        return Vec::new();
    };
    let mut kept = Vec::new();
    let mut buffer = [0u8; 8 * 1024];
    loop {
        let read = match pipe.read(&mut buffer).await {
            Ok(0) | Err(_) => return kept,
            Ok(read) => read,
        };
        let room = MAX_PIPE_BYTES.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..read.min(room)]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Instant;

    const NO_TIMEOUT: Duration = Duration::from_secs(30);

    async fn hook(command: &str, input: &Value) -> Result<Completed, RunError> {
        let cwd = std::env::temp_dir();
        run(Request {
            command,
            input,
            cwd: &cwd,
            timeout: NO_TIMEOUT,
            env: BTreeMap::new(),
        })
        .await
    }

    fn ok(completed: Result<Completed, RunError>) -> Completed {
        completed.expect("the hook ran")
    }

    #[tokio::test]
    async fn the_hook_reads_its_event_on_stdin() {
        let out = ok(hook("cat", &json!({"hook_event_name": "Stop"})).await);
        assert_eq!(out.code, 0);
        assert_eq!(out.stdout.trim(), r#"{"hook_event_name":"Stop"}"#);
    }

    #[tokio::test]
    async fn the_exit_code_and_stderr_come_back() {
        let out = ok(hook("echo nope >&2; exit 2", &json!({})).await);
        assert_eq!(out.code, 2);
        assert_eq!(out.stderr, "nope");
        assert!(out.stdout.is_empty());
    }

    #[tokio::test]
    async fn the_hook_runs_where_the_session_is() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(dir.path().join("marker"), "").expect("write");
        let out = run(Request {
            command: "ls",
            input: &json!({}),
            cwd: dir.path(),
            timeout: NO_TIMEOUT,
            env: BTreeMap::new(),
        })
        .await
        .expect("the hook ran");
        assert_eq!(out.stdout.trim(), "marker");
    }

    #[tokio::test]
    async fn the_environment_reaches_the_hook() {
        let cwd = std::env::temp_dir();
        let out = run(Request {
            command: "printf '%s' \"$FOO\"",
            input: &json!({}),
            cwd: &cwd,
            timeout: NO_TIMEOUT,
            env: [("FOO".to_string(), "bar".to_string())].into(),
        })
        .await
        .expect("the hook ran");
        assert_eq!(out.stdout, "bar");
    }

    #[tokio::test]
    async fn a_hook_that_never_reads_a_large_stdin_still_finishes() {
        // 64 KB is past every pipe buffer: writing before waiting would hang here.
        let input = json!({"tool_input": {"content": "x".repeat(64 * 1024)}});
        let started = Instant::now();
        let out = ok(hook("exit 0", &input).await);
        assert_eq!(out.code, 0);
        assert!(started.elapsed() < Duration::from_secs(5), "it hung");
    }

    #[tokio::test]
    async fn the_deadline_kills_the_whole_group() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ticks = dir.path().join("ticks");
        let command = format!(
            "(while true; do echo tick >> '{0}'; sleep 0.02; done) & sleep 30",
            ticks.display()
        );
        let cwd = std::env::temp_dir();

        let started = Instant::now();
        let error = run(Request {
            command: &command,
            input: &json!({}),
            cwd: &cwd,
            timeout: Duration::from_millis(200),
            env: BTreeMap::new(),
        })
        .await
        .expect_err("it timed out");
        let elapsed = started.elapsed();

        assert!(matches!(error, RunError::Timeout(_)), "{error:?}");
        assert!(elapsed < Duration::from_secs(1), "took {elapsed:?}");

        let after_kill = std::fs::metadata(&ticks).map(|m| m.len()).unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let later = std::fs::metadata(&ticks).map(|m| m.len()).unwrap_or(0);
        assert!(after_kill > 0, "the grandchild never ran");
        assert_eq!(after_kill, later, "the grandchild outlived the group");
    }

    #[tokio::test]
    async fn a_hook_that_writes_more_than_the_cap_still_ends() {
        let out = ok(hook("yes | head -c 2000000", &json!({})).await);
        assert_eq!(out.code, 0);
        assert_eq!(out.stdout.len(), MAX_PIPE_BYTES);
    }

    #[tokio::test]
    async fn a_command_that_cannot_start_is_an_error_not_a_panic() {
        let missing = Path::new("/no/such/directory/here");
        let error = run(Request {
            command: "true",
            input: &json!({}),
            cwd: missing,
            timeout: NO_TIMEOUT,
            env: BTreeMap::new(),
        })
        .await
        .expect_err("it could not start");
        assert!(matches!(error, RunError::Spawn { .. }), "{error:?}");
    }
}

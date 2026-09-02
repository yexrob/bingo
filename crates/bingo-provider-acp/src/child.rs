//! The adapter process: `{command, args, env}` and its three pipes.
//!
//! An adapter is a tree, not a process. `npx -y @agentclientprotocol/codex-acp`
//! is node running node running `codex app-server`, and killing the one pid we
//! spawned would leave the grandchildren holding the terminal. So the whole
//! tree is one handle — a process group on unix, a job object on Windows — and
//! dropping [`Adapter`] ends it, which is what makes a session's end the
//! child's end (`bingo-tool-bash`'s `Group`, ADR-0018 §6).
//!
//! stderr is drained and thrown away rather than left unread: a pipe nobody
//! reads fills, and a full pipe stops an agent mid-turn.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;

#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::process::{ChildStdin, ChildStdout};

use crate::error::AcpError;

/// A running adapter. The pipes come out once, into the connection; what stays
/// here is the handle whose drop ends the tree.
pub struct Adapter {
    child: Box<dyn ChildWrapper>,
}

impl Drop for Adapter {
    fn drop(&mut self) {
        // A reaped child's group id is no longer ours to signal: the number
        // may already name somebody else's work.
        if self.child.id().is_none() {
            return;
        }
        let _ = self.child.start_kill();
    }
}

/// What a spawn hands back: the tree, and the two ends of its conversation.
pub struct Spawned {
    pub adapter: Adapter,
    pub reader: ChildStdout,
    pub writer: ChildStdin,
}

/// Start an adapter in `cwd`. `env` is added to the environment this process
/// already has: an adapter reads its own credentials from there, and clearing
/// it would take away the login it depends on.
pub fn spawn(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> Result<Spawned, AcpError> {
    let mut child = wrapped(command, args, env, cwd).spawn().map_err(|e| {
        AcpError::Spawn(format!(
            "could not start the ACP adapter `{command}`: {e}. \
             The first-tier adapters are node packages; `npx` must be on PATH."
        ))
    })?;
    drain_stderr(child.as_mut());
    let reader = take_pipe(child.stdout().take(), "stdout")?;
    let writer = take_pipe(child.stdin().take(), "stdin")?;
    Ok(Spawned {
        adapter: Adapter { child },
        reader,
        writer,
    })
}

fn wrapped(
    command: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
    cwd: &Path,
) -> CommandWrap {
    let mut process = tokio::process::Command::new(command);
    process
        .args(args)
        .envs(env)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut wrapped = CommandWrap::from(process);
    // One handle for the whole tree, so an ended session takes the npx tree
    // with it. The two platforms spell that differently: a process group on
    // unix, a job object on Windows.
    #[cfg(unix)]
    wrapped.wrap(ProcessGroup::leader());
    #[cfg(windows)]
    wrapped.wrap(JobObject);
    wrapped.wrap(KillOnDrop);
    wrapped
}

fn take_pipe<T>(pipe: Option<T>, which: &str) -> Result<T, AcpError> {
    pipe.ok_or_else(|| AcpError::Spawn(format!("the adapter has no {which}")))
}

/// An adapter logs to stderr, sometimes a lot of it. Nothing here reads those
/// lines for meaning; they are read so the pipe never fills.
fn drain_stderr(child: &mut dyn ChildWrapper) {
    let Some(mut stderr) = child.stderr().take() else {
        return;
    };
    tokio::spawn(async move {
        let mut sink = tokio::io::sink();
        let _ = tokio::io::copy(&mut stderr, &mut sink).await;
    });
}

/// Close the adapter's stdin and let it end on its own terms. `codex-acp`
/// takes its own child down two seconds after its stdin closes, so asking
/// first is kinder than the kill that follows when the handle drops.
pub async fn hang_up(mut writer: impl AsyncWrite + Unpin) {
    let _ = writer.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    /// A command nobody has is a configuration fault, not a transport one:
    /// no retry brings it back, and the message says what to install.
    #[test]
    fn a_command_that_is_not_there_names_itself_and_says_what_is_missing() {
        let cwd = std::env::temp_dir();
        let failed = spawn("bingo-no-such-adapter-xyz", &[], &env(), &cwd)
            .err()
            .expect("nothing starts");
        let said = failed.to_string();
        assert!(said.contains("bingo-no-such-adapter-xyz"), "{said}");
        assert!(said.contains("npx"), "{said}");
        assert!(matches!(failed, AcpError::Spawn(_)));
    }

    /// The pipes are real, both ways, and the tree ends when the handle does.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_spawned_adapter_speaks_both_ways_and_dies_with_its_handle() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let cwd = std::env::temp_dir();
        let args = [
            "-c".to_string(),
            "read line; echo \"heard $line\"".to_string(),
        ];
        let spawned = spawn("/bin/sh", &args, &env(), &cwd).expect("a shell starts");
        let Spawned {
            adapter,
            reader,
            mut writer,
        } = spawned;
        writer.write_all(b"hello\n").await.expect("stdin takes it");
        let mut lines = BufReader::new(reader).lines();
        assert_eq!(
            lines.next_line().await.expect("a line").as_deref(),
            Some("heard hello")
        );
        let id = adapter.child.id();
        assert!(id.is_some(), "a live child has a pid");
        drop(adapter);
    }
}

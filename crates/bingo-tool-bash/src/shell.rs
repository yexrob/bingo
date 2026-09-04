//! `!<line>`: the shell line a person typed, run as they typed it.
//!
//! It is not a tool call and is not gated: the person is at the keyboard, the
//! line runs with their own privileges under the session's directory, and a
//! shell would not have asked either (ADR-0008 §5). The reject tables are the
//! model's too — they keep a turn from being spent on a program waiting for
//! keys nobody will press — and a person who typed a line knows what they
//! asked for. What still holds is the process itself: stdin is `/dev/null`,
//! the output is bounded, and the tool's own default timeout ends a line that
//! would not end on its own.
//!
//! The record is one `ItemBody::Shell` — the line, what it wrote, the code it
//! came to and where it ran — so every surface draws it as the shell line it
//! is and `ContextView::fold` tells the model what the person ran.

use async_trait::async_trait;
use bingo_sdk::{
    ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, ErrorCode, ItemBody, KernelError,
};

use crate::output::Ended;
use crate::{deadline, output, run};

#[derive(Debug, Default, Clone, Copy)]
pub struct ShellCommand;

#[async_trait]
impl Command for ShellCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "!".into(),
            aliases: Vec::new(),
            hint: "<command>".into(),
            args: ArgSpec::Free {
                hint: "shell command".into(),
            },
            // Nothing about a shell line waits on the turn: the person asked
            // for it now.
            instant: true,
            family: "shell".into(),
        }
    }

    async fn run(&self, args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let line = args.trim();
        if line.is_empty() {
            return Err(KernelError::new(
                ErrorCode::InvalidInput,
                "! needs a command to run",
            ));
        }
        let finished = run::run(line, deadline(None), &run::Context::unwatched(&cx.cwd))
            .await
            .map_err(failed)?;
        Ok(CommandOutcome::Record {
            body: ItemBody::Shell {
                command: line.to_string(),
                output: wrote(&finished),
                exit: exit_of(finished.ended),
                cwd: cx.cwd.clone(),
            },
        })
    }
}

/// What the person reads back: what the line wrote, and — when it never
/// reached an exit code — a last line saying what ended it instead. A code it
/// did reach is not written here; it is the item's own field.
fn wrote(finished: &run::Run) -> String {
    let Some(ending) = output::unfinished(finished.ended) else {
        return finished.output.clone();
    };
    let body = finished
        .output
        .strip_suffix('\n')
        .unwrap_or(&finished.output);
    if body.is_empty() {
        return ending;
    }
    format!("{body}\n{ending}")
}

/// The code the command came to, when it came to one at all.
fn exit_of(ended: Ended) -> Option<i32> {
    match ended {
        Ended::Exited(code) => Some(code),
        Ended::Timeout { .. } | Ended::Interrupted => None,
    }
}

/// A shell that could not be started is the kernel's `TOOL_FAILED`: the line
/// never ran, so there is nothing to record.
fn failed(error: bingo_sdk::ToolError) -> KernelError {
    KernelError::new(ErrorCode::ToolFailed, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::tests::command_context as context;

    /// The line as the journal records it: what was run, what it wrote, and
    /// the code it came to.
    async fn typed(line: &str, cwd: &Path) -> (String, String, Option<i32>) {
        let outcome = ShellCommand
            .run(line, &context(cwd.to_path_buf()))
            .await
            .expect("the line ran");
        let CommandOutcome::Record {
            body:
                ItemBody::Shell {
                    command,
                    output,
                    exit,
                    cwd: ran_in,
                },
        } = &outcome
        else {
            panic!("a shell line records a shell item, got {outcome:?}");
        };
        assert_eq!(ran_in, cwd, "the line runs where the session is");
        (command.clone(), output.clone(), *exit)
    }

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    #[tokio::test]
    async fn a_line_records_what_it_wrote() {
        let dir = scratch();
        let (command, output, exit) = typed("echo hi", dir.path()).await;
        assert_eq!(command, "echo hi");
        assert_eq!(output, "hi\n");
        assert_eq!(exit, Some(0));
    }

    /// The code is the item's own field, so nothing is appended to what the
    /// command wrote — a surface says how it ended in its own words.
    #[tokio::test]
    async fn a_failing_line_carries_its_exit_code_beside_its_output() {
        let dir = scratch();
        let (_, output, exit) = typed("echo oops >&2; exit 3", dir.path()).await;
        assert_eq!(output, "oops\n");
        assert_eq!(exit, Some(3));

        let (_, silent, exit) = typed("exit 3", dir.path()).await;
        assert_eq!(silent, "");
        assert_eq!(exit, Some(3));
    }

    #[tokio::test]
    async fn the_line_runs_in_the_session_s_directory() {
        let dir = scratch();
        std::fs::write(dir.path().join("marker"), "").expect("write");
        let (_, output, _) = typed("ls", dir.path()).await;
        assert_eq!(output, "marker\n");
    }

    #[tokio::test]
    async fn a_line_the_tool_would_refuse_runs_for_the_person_who_typed_it() {
        let dir = scratch();
        std::fs::write(dir.path().join("note"), "read me\n").expect("write");
        // A pager is refused for the model, whose turn it would hold; a person
        // typed this one, and with its output captured the pager acts like cat.
        assert!(
            crate::reject::interactive_reason("less note").is_some(),
            "the table no longer refuses a pager"
        );
        let (_, output, _) = typed("less note", dir.path()).await;
        assert_eq!(output, "read me\n");
    }

    #[tokio::test]
    async fn an_empty_line_is_nothing_to_run() {
        let dir = scratch();
        for line in ["", "   "] {
            let error = ShellCommand
                .run(line, &context(dir.path().to_path_buf()))
                .await
                .expect_err("nothing to run");
            assert_eq!(error.code, ErrorCode::InvalidInput);
        }
    }

    #[test]
    fn the_spec_runs_now_and_takes_the_rest_of_the_line() {
        let spec = ShellCommand.spec();
        assert_eq!(spec.name, "!");
        assert!(spec.aliases.is_empty());
        assert!(spec.instant, "a shell line never waits for a turn");
        assert_eq!(spec.family, "shell");
        assert!(matches!(spec.args, ArgSpec::Free { .. }));
    }

    /// A command that never exited has no code to record, so what stopped it
    /// is the last line of what the person reads.
    #[test]
    fn an_ending_that_reached_no_exit_code_gets_its_own_last_line() {
        let timed_out = run::Run {
            output: "started\n".into(),
            ended: Ended::Timeout { after_ms: 120_000 },
        };
        assert_eq!(wrote(&timed_out), "started\n[timed out after 120s]");
        assert_eq!(exit_of(timed_out.ended), None);
    }
}

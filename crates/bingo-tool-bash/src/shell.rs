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
//! The record is one `Action`, so `ContextView::fold` tells the model what the
//! person ran and what came back.

use async_trait::async_trait;
use bingo_sdk::{
    ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, ErrorCode, ItemBody, KernelError,
};
use serde_json::Value;

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
            body: ItemBody::Action {
                name: "!".into(),
                args: Value::String(line.to_string()),
                result: Some(Value::String(transcript(&finished))),
            },
        })
    }
}

/// What the person reads back: what the line wrote, and — unless it simply
/// succeeded — a last line saying how it ended.
fn transcript(finished: &run::Run) -> String {
    let Some(ending) = output::ending(finished.ended) else {
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

    /// The line as the transcript records it.
    async fn typed(line: &str, cwd: &Path) -> (Value, Value) {
        let outcome = ShellCommand
            .run(line, &context(cwd.to_path_buf()))
            .await
            .expect("the line ran");
        let CommandOutcome::Record {
            body: ItemBody::Action { name, args, result },
        } = &outcome
        else {
            panic!("a shell line records an action, got {outcome:?}");
        };
        assert_eq!(name, "!");
        (
            args.clone(),
            result.clone().expect("a line always has a result"),
        )
    }

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().expect("temp dir")
    }

    #[tokio::test]
    async fn a_line_records_what_it_wrote() {
        let dir = scratch();
        let (args, result) = typed("echo hi", dir.path()).await;
        assert_eq!(args, Value::String("echo hi".into()));
        assert_eq!(result, Value::String("hi\n".into()));
    }

    #[tokio::test]
    async fn a_failing_line_carries_its_exit_code() {
        let dir = scratch();
        let (_, result) = typed("echo oops >&2; exit 3", dir.path()).await;
        assert_eq!(result, Value::String("oops\n[exit 3]".into()));

        let (_, silent) = typed("exit 3", dir.path()).await;
        assert_eq!(silent, Value::String("[exit 3]".into()));
    }

    #[tokio::test]
    async fn the_line_runs_in_the_session_s_directory() {
        let dir = scratch();
        std::fs::write(dir.path().join("marker"), "").expect("write");
        let (_, result) = typed("ls", dir.path()).await;
        assert_eq!(result, Value::String("marker\n".into()));
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
        let (_, result) = typed("less note", dir.path()).await;
        assert_eq!(result, Value::String("read me\n".into()));
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

    #[test]
    fn an_ending_that_is_not_a_clean_exit_gets_its_own_last_line() {
        let timed_out = run::Run {
            output: "started\n".into(),
            ended: output::Ended::Timeout { after_ms: 120_000 },
        };
        assert_eq!(transcript(&timed_out), "started\n[timed out after 120s]");
    }
}

//! `/tasks`: the session's list as a person reads it, in the columns of the
//! table every surface draws.

use async_trait::async_trait;
use bingo_sdk::{ArgSpec, Command, CommandContext, CommandOutcome, CommandSpec, KernelError, View};

use crate::{journal, render};

/// What a person is told when there is nothing on the list. The model's copy
/// names the tool that adds one; a person has this command and no tool.
const NONE: &str = "no tasks in this session";

#[derive(Debug, Default, Clone, Copy)]
pub struct TasksCommand;

#[async_trait]
impl Command for TasksCommand {
    fn spec(&self) -> CommandSpec {
        CommandSpec {
            name: "tasks".into(),
            aliases: Vec::new(),
            hint: "the tasks of this session".into(),
            args: ArgSpec::None,
            // Reading the journal touches nothing a turn is using.
            instant: true,
            family: "tasks".into(),
        }
    }

    async fn run(&self, _args: &str, cx: &CommandContext) -> Result<CommandOutcome, KernelError> {
        let tasks = journal::read(&cx.host, &cx.session).await?;
        if tasks.is_empty() {
            return Ok(CommandOutcome::View {
                view: View::Text { text: NONE.into() },
            });
        }
        Ok(CommandOutcome::View {
            view: View::Table {
                headers: render::HEADERS.map(str::to_string).to_vec(),
                rows: render::rows(&tasks),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create::TaskCreateTool;
    use crate::tests::{Journals, command_context, tool_context};
    use crate::update::TaskUpdateTool;
    use bingo_sdk::Tool;
    use serde_json::json;

    #[tokio::test]
    async fn the_table_has_a_row_per_task_in_the_columns_of_the_headers() {
        let journals = Journals::new();
        let session = journals.session();
        let cx = tool_context(&session, &journals);
        TaskCreateTool
            .call(
                json!({"subject": "write the plan", "owner": "reviewer"}),
                &cx,
            )
            .await
            .expect("a task");
        TaskCreateTool
            .call(json!({"subject": "ship it"}), &cx)
            .await
            .expect("a task");
        TaskUpdateTool
            .call(json!({"id": 1, "status": "in_progress"}), &cx)
            .await
            .expect("an update");

        let outcome = TasksCommand
            .run("", &command_context(&session, &journals))
            .await
            .expect("a table");
        let CommandOutcome::View {
            view: View::Table { headers, rows },
        } = outcome
        else {
            panic!("a task list is a table");
        };
        assert_eq!(headers, render::HEADERS);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ["1", "in_progress", "write the plan", "reviewer"]);
        assert_eq!(rows[1], ["2", "pending", "ship it", ""]);
    }

    #[tokio::test]
    async fn a_session_with_no_tasks_gets_one_line() {
        let journals = Journals::new();
        let session = journals.session();
        let outcome = TasksCommand
            .run("", &command_context(&session, &journals))
            .await
            .expect("a line");
        assert_eq!(
            outcome,
            CommandOutcome::View {
                view: View::Text { text: NONE.into() }
            }
        );
    }

    #[test]
    fn the_spec_runs_now_and_takes_nothing() {
        let spec = TasksCommand.spec();
        assert_eq!(spec.name, "tasks");
        assert!(spec.instant, "reading the journal never waits for a turn");
        assert_eq!(spec.args, ArgSpec::None);
        assert_eq!(spec.family, "tasks");
    }
}

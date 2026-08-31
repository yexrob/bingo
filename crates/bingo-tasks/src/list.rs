//! `TaskList`: the whole list, one task per line. The listing is read at the
//! moment it is asked for — this plugin keeps no list of its own.

use async_trait::async_trait;
use bingo_sdk::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::board::{self, In};
use crate::render::Present;
use crate::{failed, journal, render};

const DESCRIPTION: &str = "\
List this session's tasks: their ids, their statuses, their subjects, who \
owns them and what holds them up. Read it before writing to a task whose id \
you are unsure of, and to see what is left to do. With `in`, it is a room's \
shared board that is listed, and an owner no session in that room answers to \
any more reads `owner (gone)` — nobody rewrote the task, it is only being \
said plainly that its owner is not here.";

/// The arguments a listing takes: which board, and nothing else.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListArgs {
    #[serde(flatten)]
    pub board: In,
}

/// Reading the list; it changes nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct TaskListTool;

#[async_trait]
impl Tool for TaskListTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "TaskList".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<ListArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits()
    }

    /// A listing asks for at most a board: a model that sends a null, an
    /// empty object or a stray key still gets its answer, and only an `in`
    /// that is not a name at all is refused rather than quietly read as none.
    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args = args(input)?;
        let board = match board::of(&cx.host, &cx.session, &args.board).await {
            Ok(board) => board,
            Err(error) => return crate::misaddressed(error),
        };
        let tasks = journal::read(&cx.host, &board.session)
            .await
            .map_err(failed)?;
        let here = board.present();
        Ok(ToolOutput::text(render::listing(
            &tasks,
            Present::among(here.as_deref()),
        )))
    }
}

fn args(input: Value) -> Result<ListArgs, ToolError> {
    match input {
        Value::Null => Ok(ListArgs::default()),
        input => serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create::TaskCreateTool;
    use crate::tests::{Journals, text, tool_context};
    use crate::update::TaskUpdateTool;
    use serde_json::json;

    #[tokio::test]
    async fn a_session_with_no_tasks_is_told_how_to_add_one() {
        let journals = Journals::new();
        let session = journals.session();
        let out = TaskListTool
            .call(json!({}), &tool_context(&session, &journals))
            .await
            .expect("a listing");
        assert_eq!(text(&out), render::NONE);
    }

    #[tokio::test]
    async fn every_task_is_one_line_in_the_order_it_was_added() {
        let journals = Journals::new();
        let session = journals.session();
        let cx = tool_context(&session, &journals);
        TaskCreateTool
            .call(json!({"subject": "write the plan"}), &cx)
            .await
            .expect("a task");
        TaskCreateTool
            .call(
                json!({"subject": "ship it", "owner": "reviewer", "blockedBy": [1]}),
                &cx,
            )
            .await
            .expect("a task");
        TaskUpdateTool
            .call(json!({"id": 1, "status": "in_progress"}), &cx)
            .await
            .expect("an update");

        let text = text(&TaskListTool.call(json!({}), &cx).await.expect("a listing"));
        assert_eq!(
            text,
            "#1 [in_progress] write the plan\n#2 [pending] ship it — reviewer (blocked by #1)"
        );
    }

    /// A listing answers whatever the model sent, as `ListAgents` does.
    #[tokio::test]
    async fn arguments_are_ignored() {
        let journals = Journals::new();
        let session = journals.session();
        let cx = tool_context(&session, &journals);
        TaskCreateTool
            .call(json!({"subject": "write the plan"}), &cx)
            .await
            .expect("a task");
        for input in [json!(null), json!({"filter": "open"})] {
            let out = TaskListTool.call(input, &cx).await.expect("a listing");
            assert_eq!(text(&out), "#1 [pending] write the plan");
        }
    }

    /// The board's listing, and the one thing it says that a private list
    /// does not: who is not here any more. Nothing is written by the saying.
    #[tokio::test]
    async fn a_board_marks_an_owner_no_session_here_answers_to() {
        let journals = Journals::new();
        let root = journals.session();
        let room = journals.room(&root, "#design");
        journals.child(&root, "reviewer");
        let cx = tool_context(&root, &journals);
        TaskCreateTool
            .call(
                json!({"subject": "write the plan", "owner": "reviewer", "in": "#design"}),
                &cx,
            )
            .await
            .expect("a task");
        TaskCreateTool
            .call(
                json!({"subject": "ship it", "owner": "scout", "in": "#design"}),
                &cx,
            )
            .await
            .expect("a task");

        let out = TaskListTool
            .call(json!({"in": "#design"}), &cx)
            .await
            .expect("a listing");
        assert_eq!(
            text(&out),
            "#1 [pending] write the plan — reviewer\n#2 [pending] ship it — scout (gone)"
        );
        let tasks = journal::read(&journals.handle(), &room)
            .await
            .expect("the board");
        assert_eq!(
            tasks[1].owner.as_deref(),
            Some("scout"),
            "the mark reached the journal"
        );
    }

    /// The same owner on the caller's own list is a note to itself, and the
    /// listing asserts nothing about it.
    #[tokio::test]
    async fn a_private_list_never_marks_an_owner() {
        let journals = Journals::new();
        let session = journals.session();
        let cx = tool_context(&session, &journals);
        TaskCreateTool
            .call(json!({"subject": "ship it", "owner": "scout"}), &cx)
            .await
            .expect("a task");
        let out = TaskListTool.call(json!({}), &cx).await.expect("a listing");
        assert_eq!(text(&out), "#1 [pending] ship it — scout");
    }

    #[tokio::test]
    async fn an_in_that_is_not_a_name_is_refused_rather_than_read_as_none() {
        let journals = Journals::new();
        let session = journals.session();
        let error = TaskListTool
            .call(json!({"in": 7}), &tool_context(&session, &journals))
            .await
            .expect_err("a board is named by a word");
        assert!(matches!(error, ToolError::InvalidInput(_)), "{error:?}");
    }

    #[test]
    fn the_schema_is_an_empty_object() {
        let spec = TaskListTool.spec();
        assert_eq!(spec.name, "TaskList");
        assert_eq!(spec.input_schema["type"], "object");
        assert!(spec.input_schema.get("$schema").is_none());
        assert!(spec.input_schema.get("required").is_none());
        assert!(spec.input_schema["properties"].get("in").is_some());
    }
}

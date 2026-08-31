//! `TaskCreate`: one more task on the session's list, numbered by the list
//! itself so the model never has to pick an id.

use async_trait::async_trait;
use bingo_sdk::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::board::{self, In};
use crate::task::Draft;
use crate::{failed, journal, task};

const DESCRIPTION: &str = "\
Add one task to this session's list, and get back the id it was given. Write \
the subject in the imperative — \"write the plan\", not \"writing the plan\" \
— and give `activeForm` the present-continuous form, which is what is shown \
while the task is in progress. `blockedBy` names the ids that must finish \
before this task can start, `blocks` the ids waiting on it. One task per unit \
of work someone would tick off; the list survives the run, so record what is \
worth coming back to. With `in`, the task goes on a room's shared board \
instead — everyone in the room reads and writes that one list, and two \
writers at once overwrite each other, so put a task there once and let its \
owner move it on.";

/// What `TaskCreate` takes: the task, and the board it goes on.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateArgs {
    #[serde(flatten)]
    pub draft: Draft,
    #[serde(flatten)]
    pub board: In,
}

/// Reading the list, adding to it, writing it back.
#[derive(Debug, Default, Clone, Copy)]
pub struct TaskCreateTool;

#[async_trait]
impl Tool for TaskCreateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "TaskCreate".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<CreateArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: CreateArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let board = match board::of(&cx.host, &cx.session, &args.board).await {
            Ok(board) => board,
            Err(error) => return crate::misaddressed(error),
        };
        let mut tasks = journal::read(&cx.host, &board.session)
            .await
            .map_err(failed)?;
        let task = task::create(&mut tasks, args.draft);
        journal::write(&cx.host, &board.session, &tasks)
            .await
            .map_err(failed)?;
        Ok(ToolOutput::text(format!(
            "Created #{}: {}",
            task.id, task.subject
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{Journals, text, tool_context};
    use serde_json::json;

    #[tokio::test]
    async fn the_first_task_is_created_and_named_back() {
        let journals = Journals::new();
        let session = journals.session();
        let out = TaskCreateTool
            .call(
                json!({"subject": "write the plan"}),
                &tool_context(&session, &journals),
            )
            .await
            .expect("a task");
        assert!(!out.is_error);
        assert_eq!(text(&out), "Created #1: write the plan");

        let tasks = journal::read(&journals.handle(), &session)
            .await
            .expect("the journal has it");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].subject, "write the plan");
        assert_eq!(tasks[0].status, crate::task::Status::Pending);
    }

    #[tokio::test]
    async fn everything_a_draft_carries_reaches_the_journal() {
        let journals = Journals::new();
        let session = journals.session();
        TaskCreateTool
            .call(
                json!({
                    "subject": "ship it",
                    "description": "tag and publish",
                    "activeForm": "shipping it",
                    "owner": "reviewer",
                    "blockedBy": [1, 2],
                    "blocks": [4],
                    "metadata": {"pr": 7},
                }),
                &tool_context(&session, &journals),
            )
            .await
            .expect("a task");
        let tasks = journal::read(&journals.handle(), &session)
            .await
            .expect("the journal has it");
        let task = &tasks[0];
        assert_eq!(task.description, "tag and publish");
        assert_eq!(task.active_form.as_deref(), Some("shipping it"));
        assert_eq!(task.owner.as_deref(), Some("reviewer"));
        assert_eq!(task.blocked_by, [1, 2]);
        assert_eq!(task.blocks, [4]);
        assert_eq!(task.metadata["pr"], json!(7));
    }

    #[tokio::test]
    async fn a_call_without_a_subject_is_invalid_input() {
        let journals = Journals::new();
        let session = journals.session();
        let error = TaskCreateTool
            .call(json!({}), &tool_context(&session, &journals))
            .await
            .expect_err("a task needs a subject");
        assert!(matches!(error, ToolError::InvalidInput(_)), "{error:?}");
    }

    /// The board is a room's list, reached by a name and nothing else: the
    /// caller's own list is untouched by a call that named one.
    #[tokio::test]
    async fn a_task_created_in_a_room_lands_on_the_room_s_list() {
        let journals = Journals::new();
        let root = journals.session();
        let room = journals.room(&root, "#design");
        let out = TaskCreateTool
            .call(
                json!({"subject": "write the plan", "in": "#design"}),
                &tool_context(&root, &journals),
            )
            .await
            .expect("a task");
        assert_eq!(text(&out), "Created #1: write the plan");

        let host = journals.handle();
        assert_eq!(
            journal::read(&host, &room).await.expect("the board")[0].subject,
            "write the plan"
        );
        assert!(
            journal::read(&host, &root)
                .await
                .expect("its own")
                .is_empty(),
            "the caller's own list was written to"
        );
    }

    #[tokio::test]
    async fn a_board_nothing_answers_to_is_an_error_the_model_can_read() {
        let journals = Journals::new();
        let root = journals.session();
        journals.room(&root, "#design");
        let out = TaskCreateTool
            .call(
                json!({"subject": "write the plan", "in": "#nowhere"}),
                &tool_context(&root, &journals),
            )
            .await
            .expect("an output, not a failure");
        assert!(out.is_error);
        assert!(text(&out).contains("#nowhere"), "{}", text(&out));
        assert!(text(&out).contains("#design"), "{}", text(&out));
    }

    #[test]
    fn the_schema_is_a_bare_object_and_the_traits_are_the_crate_s() {
        let spec = TaskCreateTool.spec();
        assert_eq!(spec.name, "TaskCreate");
        assert_eq!(spec.input_schema["type"], "object");
        assert!(spec.input_schema.get("$schema").is_none());
        assert!(spec.input_schema["properties"].get("activeForm").is_some());
        assert!(spec.input_schema["properties"].get("blockedBy").is_some());
        assert!(spec.input_schema["properties"].get("in").is_some());
        assert_eq!(spec.input_schema["required"], json!(["subject"]));
        assert_eq!(TaskCreateTool.traits(&Value::Null), crate::traits());
    }
}

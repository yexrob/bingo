//! `TaskCreate`: one more task on the session's list, numbered by the list
//! itself so the model never has to pick an id.

use async_trait::async_trait;
use bingo_sdk::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema};
use serde_json::Value;

use crate::task::Draft;
use crate::{failed, journal, task};

const DESCRIPTION: &str = "\
Add one task to this session's list, and get back the id it was given. Write \
the subject in the imperative — \"write the plan\", not \"writing the plan\" \
— and give `activeForm` the present-continuous form, which is what is shown \
while the task is in progress. `blockedBy` names the ids that must finish \
before this task can start, `blocks` the ids waiting on it. One task per unit \
of work someone would tick off; the list survives the run, so record what is \
worth coming back to.";

/// Reading the list, adding to it, writing it back.
#[derive(Debug, Default, Clone, Copy)]
pub struct TaskCreateTool;

#[async_trait]
impl Tool for TaskCreateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "TaskCreate".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<Draft>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let draft: Draft =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let mut tasks = journal::read(&cx.host, &cx.session).await.map_err(failed)?;
        let task = task::create(&mut tasks, draft);
        journal::write(&cx.host, &cx.session, &tasks)
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

    #[test]
    fn the_schema_is_a_bare_object_and_the_traits_are_the_crate_s() {
        let spec = TaskCreateTool.spec();
        assert_eq!(spec.name, "TaskCreate");
        assert_eq!(spec.input_schema["type"], "object");
        assert!(spec.input_schema.get("$schema").is_none());
        assert!(spec.input_schema["properties"].get("activeForm").is_some());
        assert!(spec.input_schema["properties"].get("blockedBy").is_some());
        assert_eq!(TaskCreateTool.traits(&Value::Null), crate::traits());
    }
}

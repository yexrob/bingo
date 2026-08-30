//! `TaskUpdate`: a task moves on. An id the list does not have is an error
//! the model reads and recovers from, not a failed call.

use async_trait::async_trait;
use bingo_sdk::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema};
use serde_json::Value;

use crate::task::Change;
use crate::{failed, journal, task};

const DESCRIPTION: &str = "\
Change one task on this session's list. Only the fields you name change; the \
rest stay as they are. Mark a task `in_progress` when you start it and \
`completed` the moment it is done — one task in progress at a time reads \
best. `addBlockedBy` and `addBlocks` add ids to what the task waits for and \
what waits on it, and `metadata` merges by key. Use `TaskList` first if you \
are unsure of an id.";

/// Reading the list, changing one task, writing it back.
#[derive(Debug, Default, Clone, Copy)]
pub struct TaskUpdateTool;

#[async_trait]
impl Tool for TaskUpdateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "TaskUpdate".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<Change>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let change: Change =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let id = change.id;
        let mut tasks = journal::read(&cx.host, &cx.session).await.map_err(failed)?;
        let Some(task) = task::update(&mut tasks, change) else {
            return Ok(crate::unknown(id));
        };
        journal::write(&cx.host, &cx.session, &tasks)
            .await
            .map_err(failed)?;
        Ok(ToolOutput::text(format!(
            "Updated #{} ({}): {}",
            task.id,
            task.status.as_str(),
            task.subject
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create::TaskCreateTool;
    use crate::tests::{Journals, text, tool_context};
    use serde_json::json;

    async fn with_a_task() -> (Journals, bingo_sdk::SessionId) {
        let journals = Journals::new();
        let session = journals.session();
        TaskCreateTool
            .call(
                json!({"subject": "write the plan"}),
                &tool_context(&session, &journals),
            )
            .await
            .expect("a task");
        (journals, session)
    }

    #[tokio::test]
    async fn a_status_change_is_reported_with_the_task() {
        let (journals, session) = with_a_task().await;
        let out = TaskUpdateTool
            .call(
                json!({"id": 1, "status": "in_progress"}),
                &tool_context(&session, &journals),
            )
            .await
            .expect("an update");
        assert!(!out.is_error);
        assert_eq!(text(&out), "Updated #1 (in_progress): write the plan");
        let tasks = journal::read(&journals.handle(), &session)
            .await
            .expect("the journal has it");
        assert_eq!(tasks[0].status, crate::task::Status::InProgress);
    }

    #[tokio::test]
    async fn dependencies_and_metadata_accumulate_across_calls() {
        let (journals, session) = with_a_task().await;
        let cx = tool_context(&session, &journals);
        TaskUpdateTool
            .call(
                json!({"id": 1, "addBlockedBy": [2], "metadata": {"area": "kernel"}}),
                &cx,
            )
            .await
            .expect("an update");
        TaskUpdateTool
            .call(
                json!({"id": 1, "addBlockedBy": [2, 3], "addBlocks": [4], "metadata": {"pr": 7}}),
                &cx,
            )
            .await
            .expect("an update");
        let tasks = journal::read(&journals.handle(), &session)
            .await
            .expect("the journal has it");
        assert_eq!(tasks[0].blocked_by, [2, 3]);
        assert_eq!(tasks[0].blocks, [4]);
        assert_eq!(tasks[0].metadata["area"], json!("kernel"));
        assert_eq!(tasks[0].metadata["pr"], json!(7));
    }

    #[tokio::test]
    async fn an_unknown_id_is_an_error_the_model_can_read() {
        let (journals, session) = with_a_task().await;
        let out = TaskUpdateTool
            .call(
                json!({"id": 9, "status": "completed"}),
                &tool_context(&session, &journals),
            )
            .await
            .expect("an output, not a failure");
        assert!(out.is_error);
        assert!(text(&out).contains("#9"), "{}", text(&out));
        assert!(text(&out).contains("TaskList"), "{}", text(&out));
        let tasks = journal::read(&journals.handle(), &session)
            .await
            .expect("the journal has it");
        assert_eq!(tasks[0].status, crate::task::Status::Pending);
    }

    #[test]
    fn the_schema_names_the_id_and_the_fields_that_may_change() {
        let spec = TaskUpdateTool.spec();
        assert_eq!(spec.name, "TaskUpdate");
        let properties = &spec.input_schema["properties"];
        for field in ["id", "subject", "status", "addBlockedBy", "addBlocks"] {
            assert!(properties.get(field).is_some(), "{field}");
        }
        assert_eq!(spec.input_schema["required"], json!(["id"]));
    }
}

//! `TaskList`: the whole list, one task per line. The listing is read at the
//! moment it is asked for — this plugin keeps no list of its own.

use async_trait::async_trait;
use bingo_sdk::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{failed, journal, render};

const DESCRIPTION: &str = "\
List this session's tasks: their ids, their statuses, their subjects, who \
owns them and what holds them up. Read it before writing to a task whose id \
you are unsure of, and to see what is left to do.";

/// The arguments a listing takes, which is none. Named so the schema the
/// model reads is an object like every other tool's.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ListArgs {}

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

    /// The arguments are ignored: a listing has none, and a model that sends
    /// an empty object, a null or a stray key still gets its answer.
    async fn call(&self, _input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let tasks = journal::read(&cx.host, &cx.session).await.map_err(failed)?;
        Ok(ToolOutput::text(render::listing(&tasks)))
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

    #[test]
    fn the_schema_is_an_empty_object() {
        let spec = TaskListTool.spec();
        assert_eq!(spec.name, "TaskList");
        assert_eq!(spec.input_schema["type"], "object");
        assert!(spec.input_schema.get("$schema").is_none());
        assert!(spec.input_schema.get("required").is_none());
    }
}

//! `TaskGet`: one task in full, for when a line is not enough — the
//! description, the metadata, everything that was written down.

use async_trait::async_trait;
use bingo_sdk::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::{failed, journal, task};

const DESCRIPTION: &str = "\
Read one task of this session's list in full — its description, its owner, \
what it waits for and its metadata — by the id `TaskCreate` or `TaskList` \
reported. `TaskList` is the cheaper way to see them all.";

/// The one argument a read takes.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GetArgs {
    /// The task to read, by the id `TaskCreate` or `TaskList` reported.
    pub id: u64,
}

/// Reading one task; it changes nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct TaskGetTool;

#[async_trait]
impl Tool for TaskGetTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "TaskGet".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<GetArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let args: GetArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let tasks = journal::read(&cx.host, &cx.session).await.map_err(failed)?;
        let Some(task) = task::get(&tasks, args.id) else {
            return Ok(crate::unknown(args.id));
        };
        let json = serde_json::to_string_pretty(task)
            .map_err(|e| ToolError::Failed(format!("a task is json: {e}")))?;
        Ok(ToolOutput::text(json))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create::TaskCreateTool;
    use crate::tests::{Journals, text, tool_context};
    use serde_json::json;

    #[tokio::test]
    async fn the_task_comes_back_as_the_json_it_was_written_as() {
        let journals = Journals::new();
        let session = journals.session();
        let cx = tool_context(&session, &journals);
        TaskCreateTool
            .call(
                json!({
                    "subject": "write the plan",
                    "description": "the M9 one",
                    "activeForm": "writing the plan",
                    "metadata": {"pr": 7},
                }),
                &cx,
            )
            .await
            .expect("a task");

        let out = TaskGetTool
            .call(json!({"id": 1}), &cx)
            .await
            .expect("the task");
        assert!(!out.is_error);
        let value: Value = serde_json::from_str(&text(&out)).expect("pretty json");
        assert_eq!(
            value,
            json!({
                "id": 1,
                "subject": "write the plan",
                "description": "the M9 one",
                "activeForm": "writing the plan",
                "status": "pending",
                "metadata": {"pr": 7},
            })
        );
        assert!(text(&out).contains('\n'), "pretty, not one line");
    }

    #[tokio::test]
    async fn an_unknown_id_is_an_error_the_model_can_read() {
        let journals = Journals::new();
        let session = journals.session();
        let out = TaskGetTool
            .call(json!({"id": 3}), &tool_context(&session, &journals))
            .await
            .expect("an output, not a failure");
        assert!(out.is_error);
        assert!(text(&out).contains("#3"), "{}", text(&out));
    }

    #[test]
    fn the_schema_asks_for_an_id() {
        let spec = TaskGetTool.spec();
        assert_eq!(spec.name, "TaskGet");
        assert_eq!(spec.input_schema["required"], json!(["id"]));
        assert_eq!(spec.input_schema["properties"]["id"]["type"], "integer");
    }
}

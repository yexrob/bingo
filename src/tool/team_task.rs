//! Task-specific control available only to members assigned to a durable team task.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;

use crate::query::Session;
use crate::team_tasks::TeamTaskStatus;
use crate::tool::{Tool, ToolContext, ToolError, ToolResult, parse_input};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TeamTaskAction {
    RequestReview,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct TeamTaskInput {
    #[schemars(
        description = "Task action. Use request_review only when the work is ready for the user to inspect."
    )]
    action: TeamTaskAction,
    #[schemars(
        description = "Concise handoff summary: completed work, validation, and anything the user must decide."
    )]
    summary: String,
}

pub struct TeamTaskTool {
    session: Arc<Session>,
}

impl TeamTaskTool {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Tool for TeamTaskTool {
    fn name(&self) -> String {
        "TeamTask".to_string()
    }

    fn description(&self) -> String {
        "Control the durable team task you are currently assigned to. The task leader calls request_review once the team's work is ready; this drains current member turns and hands the task to the user for acceptance.".to_string()
    }

    fn input_schema(&self) -> serde_json::Value {
        super::schema_for::<TeamTaskInput>()
    }

    fn is_concurrency_safe(&self, _input: &serde_json::Value) -> bool {
        false
    }

    fn is_read_only(&self, _input: &serde_json::Value) -> bool {
        false
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _ctx: &ToolContext,
    ) -> Result<ToolResult, ToolError> {
        let params: TeamTaskInput = parse_input(&input)?;
        let member = self
            .session
            .instance
            .as_deref()
            .ok_or_else(|| ToolError::failed("TeamTask is available only to task members"))?;
        let task = self
            .session
            .team_tasks
            .active_task_for_member(member)
            .ok_or_else(|| {
                ToolError::failed("this member is not assigned to an active team task")
            })?;
        let channel = self
            .session
            .team_tasks
            .get(&task.id, None, Some(1))
            .map_err(|error| ToolError::failed(error.to_string()))?
            .channel;
        if task.leader != member {
            return Err(ToolError::failed(format!(
                "only task leader {} can request user review",
                task.leader
            )));
        }
        let summary = params.summary.trim();
        if summary.is_empty() {
            return Err(ToolError::failed("summary must not be empty"));
        }
        match params.action {
            TeamTaskAction::RequestReview => {
                self.session
                    .team_tasks
                    .begin_pause(
                        &task.id,
                        TeamTaskStatus::AwaitingReview,
                        "Review requested by task leader".to_string(),
                        Some(summary.to_string()),
                    )
                    .map_err(|error| ToolError::failed(error.to_string()))?;
                for participant in &task.participants {
                    self.session
                        .agents
                        .discard_channel_inbox(&participant.name, &channel);
                }
                Ok(ToolResult {
                    content: serde_json::Value::String(
                        "review requested; finish this turn with your final handoff".to_string(),
                    ),
                    is_error: false,
                    diff: None,
                })
            }
        }
    }
}

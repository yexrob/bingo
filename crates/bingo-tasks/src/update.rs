//! `TaskUpdate`: a task moves on. An id the list does not have is an error
//! the model reads and recovers from, not a failed call.

use async_trait::async_trait;
use bingo_sdk::{Tool, ToolContext, ToolError, ToolOutput, ToolSpec, ToolTraits, input_schema};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::board::{self, In};
use crate::task::{Change, Task};
use crate::{failed, journal, task};

const DESCRIPTION: &str = "\
Change one task on this session's list. Only the fields you name change; the \
rest stay as they are. Mark a task `in_progress` when you start it and \
`completed` the moment it is done — one task in progress at a time reads \
best. `addBlockedBy` and `addBlocks` add ids to what the task waits for and \
what waits on it, and `metadata` merges by key. Use `TaskList` first if you \
are unsure of an id. With `in`, the task is one on a room's shared board: \
`claim` takes it for yourself, `owner` gives it to somebody else, and two \
writers at once overwrite each other, so change a board task once and say so \
in the room rather than racing.";

/// What `TaskUpdate` takes: the change, the board the task is on, and whether
/// the caller is taking it for itself.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateArgs {
    #[serde(flatten)]
    pub change: Change,
    #[serde(flatten)]
    pub board: In,
    /// Take the task for yourself: your own name is written as its owner. The
    /// runtime knows which session you are, so do not say who — and do not
    /// pass `owner` as well, which is for giving a task to somebody else.
    #[serde(default)]
    pub claim: Option<bool>,
}

impl UpdateArgs {
    fn claims(&self) -> bool {
        self.claim.unwrap_or(false)
    }
}

/// Reading the list, changing one task, writing it back.
#[derive(Debug, Default, Clone, Copy)]
pub struct TaskUpdateTool;

#[async_trait]
impl Tool for TaskUpdateTool {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "TaskUpdate".into(),
            description: DESCRIPTION.into(),
            input_schema: input_schema::<UpdateArgs>(),
            meta: Default::default(),
        }
    }

    fn traits(&self, _input: &Value) -> ToolTraits {
        crate::traits()
    }

    async fn call(&self, input: Value, cx: &ToolContext) -> Result<ToolOutput, ToolError> {
        let mut args: UpdateArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let board = match board::of(&cx.host, &cx.session, &args.board).await {
            Ok(board) => board,
            Err(error) => return crate::misaddressed(error),
        };
        if args.claims() {
            match board::claimant(&cx.host, &cx.session).await {
                Ok(name) => args.change.owner = Some(name),
                Err(error) => return crate::misaddressed(error),
            }
        }
        let (id, claimed) = (args.change.id, args.claims());
        let mut tasks = journal::read(&cx.host, &board.session)
            .await
            .map_err(failed)?;
        let Some(task) = task::update(&mut tasks, args.change) else {
            return Ok(crate::unknown(id));
        };
        journal::write(&cx.host, &board.session, &tasks)
            .await
            .map_err(failed)?;
        Ok(ToolOutput::text(receipt(&task, claimed)))
    }
}

/// What the call answers with. A claim names the owner it stamped: the caller
/// never wrote that name, so this is where it learns what it is called.
fn receipt(task: &Task, claimed: bool) -> String {
    let line = format!(
        "Updated #{} ({}): {}",
        task.id,
        task.status.as_str(),
        task.subject
    );
    match (claimed, &task.owner) {
        (true, Some(owner)) => format!("{line} — {owner}"),
        _ => line,
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

    /// A board task with a member on the other end: the member never says who
    /// it is, and the name that lands is its own (ADR-0023 §2).
    #[tokio::test]
    async fn a_claim_stamps_the_caller_s_own_name_without_it_saying_so() {
        let journals = Journals::new();
        let root = journals.session();
        let room = journals.room(&root, "#design");
        let member = journals.child(&root, "reviewer");
        TaskCreateTool
            .call(
                json!({"subject": "write the plan", "in": "#design"}),
                &tool_context(&root, &journals),
            )
            .await
            .expect("a task on the board");

        let out = TaskUpdateTool
            .call(
                json!({"id": 1, "status": "in_progress", "claim": true, "in": "#design"}),
                &tool_context(&member, &journals),
            )
            .await
            .expect("a claim");
        assert!(!out.is_error);
        assert_eq!(
            text(&out),
            "Updated #1 (in_progress): write the plan — reviewer"
        );
        let tasks = journal::read(&journals.handle(), &room)
            .await
            .expect("the board");
        assert_eq!(tasks[0].owner.as_deref(), Some("reviewer"));
    }

    /// A session with no name of its own cannot sign a claim, and is told to
    /// name the doer instead of having one guessed for it.
    #[tokio::test]
    async fn a_root_claiming_is_refused_in_words_and_changes_nothing() {
        let journals = Journals::new();
        let root = journals.session();
        let room = journals.room(&root, "#design");
        let cx = tool_context(&root, &journals);
        TaskCreateTool
            .call(json!({"subject": "write the plan", "in": "#design"}), &cx)
            .await
            .expect("a task on the board");

        let out = TaskUpdateTool
            .call(json!({"id": 1, "claim": true, "in": "#design"}), &cx)
            .await
            .expect("an output, not a failure");
        assert!(out.is_error);
        assert!(text(&out).contains("`owner`"), "{}", text(&out));
        let tasks = journal::read(&journals.handle(), &room)
            .await
            .expect("the board");
        assert_eq!(tasks[0].owner, None, "the refusal wrote an owner anyway");
    }

    /// Assignment is untouched: a named owner still means the caller is
    /// handing the task to somebody.
    #[tokio::test]
    async fn a_named_owner_is_still_an_assignment() {
        let (journals, session) = with_a_task().await;
        TaskUpdateTool
            .call(
                json!({"id": 1, "owner": "scout"}),
                &tool_context(&session, &journals),
            )
            .await
            .expect("an assignment");
        let tasks = journal::read(&journals.handle(), &session)
            .await
            .expect("the journal has it");
        assert_eq!(tasks[0].owner.as_deref(), Some("scout"));
    }

    #[test]
    fn the_schema_names_the_id_and_the_fields_that_may_change() {
        let spec = TaskUpdateTool.spec();
        assert_eq!(spec.name, "TaskUpdate");
        let properties = &spec.input_schema["properties"];
        for field in [
            "id",
            "subject",
            "status",
            "addBlockedBy",
            "addBlocks",
            "in",
            "claim",
        ] {
            assert!(properties.get(field).is_some(), "{field}");
        }
        assert_eq!(spec.input_schema["required"], json!(["id"]));
    }

    /// The gate asks a person the same thing for a board write as for a
    /// private one. Writing another session's journal is what `SendMessage`
    /// already does, and it is trusted read-only for the same reason: nothing
    /// outside the process changes, and whatever the board's readers then do
    /// is gated where they do it (ADR-0023 §4).
    #[test]
    fn a_board_write_is_gated_no_differently_than_a_private_one() {
        assert_eq!(
            TaskUpdateTool.traits(&json!({"id": 1, "in": "#design", "claim": true})),
            TaskUpdateTool.traits(&json!({"id": 1}))
        );
    }
}

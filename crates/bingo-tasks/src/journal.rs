//! Where the list lives: the session's own journal, as the extension
//! `bingo.tasks`/`tasks` (ADR-0011 §2). Every reader opens the session and
//! folds the frames the kernel already folded; every writer publishes the
//! whole list again. Nothing is kept between calls — a file or a map here
//! would be a second copy of a fact the journal already holds, and the one
//! that survives `--continue` is the journal's.

use bingo_sdk::{
    ClientIdentity, ErrorCode, HostHandle, KernelError, OpenOptions, SessionId, SessionSelector,
    SessionState,
};

use crate::task::Task;

/// The plugin the payload belongs to, and the kind within it.
pub const PLUGIN: &str = "bingo.tasks";
pub const KIND: &str = "tasks";

/// The list as the session's journal has it. A session that never wrote one,
/// or wrote something this crate cannot read, has no tasks.
pub async fn read(host: &HostHandle, session: &SessionId) -> Result<Vec<Task>, KernelError> {
    let attachment = host
        .open(
            SessionSelector::ById {
                id: session.clone(),
            },
            ClientIdentity {
                name: "tasks".into(),
                surface: "tasks".into(),
            },
            OpenOptions::default(),
        )
        .await?;
    Ok(tasks_of(&attachment.snapshot))
}

fn tasks_of(snapshot: &SessionState) -> Vec<Task> {
    let Some(payload) = snapshot
        .extensions
        .get(PLUGIN)
        .and_then(|kinds| kinds.get(KIND))
    else {
        return Vec::new();
    };
    match serde_json::from_value(payload.clone()) {
        Ok(tasks) => tasks,
        Err(error) => {
            tracing::warn!(%error, "the tasks in this journal are not a task list; starting empty");
            Vec::new()
        }
    }
}

/// Publishes the whole list, which is what the kind means: the next snapshot
/// carries exactly this, and so does the next run that continues the session.
pub async fn write(
    host: &HostHandle,
    session: &SessionId,
    tasks: &[Task],
) -> Result<(), KernelError> {
    let payload = serde_json::to_value(tasks)
        .map_err(|e| KernelError::new(ErrorCode::Internal, format!("a task list is json: {e}")))?;
    host.extend(session, PLUGIN, KIND, payload).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::{self, Draft};
    use crate::tests::Journals;
    use serde_json::json;

    #[tokio::test]
    async fn a_session_that_wrote_nothing_has_no_tasks() {
        let journals = Journals::new();
        let session = journals.session();
        assert!(
            read(&journals.handle(), &session)
                .await
                .expect("an empty list")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn what_was_written_is_what_the_next_read_folds() {
        let journals = Journals::new();
        let session = journals.session();
        let host = journals.handle();
        let mut tasks = Vec::new();
        task::create(
            &mut tasks,
            Draft {
                subject: "write the plan".into(),
                ..Draft::default()
            },
        );
        write(&host, &session, &tasks).await.expect("published");
        assert_eq!(read(&host, &session).await.expect("read back"), tasks);
    }

    #[tokio::test]
    async fn a_payload_that_is_not_a_task_list_reads_as_none() {
        let journals = Journals::new();
        let session = journals.session();
        let host = journals.handle();
        host.extend(&session, PLUGIN, KIND, json!({"tasks": "later"}))
            .await
            .expect("the kernel takes any payload");
        assert!(read(&host, &session).await.expect("no tasks").is_empty());
    }

    /// The four tools keep nothing between them: what one wrote, the next
    /// reads back out of the journal, and a fresh attachment sees the same.
    #[tokio::test]
    async fn the_four_tools_share_one_list_and_nothing_else() {
        use crate::create::TaskCreateTool;
        use crate::get::TaskGetTool;
        use crate::list::TaskListTool;
        use crate::tests::{Journals, text, tool_context};
        use crate::update::TaskUpdateTool;
        use bingo_sdk::Tool;

        let journals = Journals::new();
        let session = journals.session();
        let cx = tool_context(&session, &journals);

        let created = TaskCreateTool
            .call(json!({"subject": "write the plan"}), &cx)
            .await
            .expect("a task");
        assert_eq!(text(&created), "Created #1: write the plan");

        let listed = TaskListTool.call(json!({}), &cx).await.expect("a listing");
        assert_eq!(text(&listed), "#1 [pending] write the plan");

        let updated = TaskUpdateTool
            .call(json!({"id": 1, "status": "in_progress"}), &cx)
            .await
            .expect("an update");
        assert_eq!(text(&updated), "Updated #1 (in_progress): write the plan");

        let got = TaskGetTool
            .call(json!({"id": 1}), &cx)
            .await
            .expect("the task");
        let value: serde_json::Value = serde_json::from_str(&text(&got)).expect("pretty json");
        assert_eq!(value["status"], json!("in_progress"));

        assert_eq!(
            read(&journals.handle(), &session).await.expect("read back"),
            vec![Task {
                id: 1,
                subject: "write the plan".into(),
                description: String::new(),
                active_form: None,
                status: crate::task::Status::InProgress,
                owner: None,
                blocks: Vec::new(),
                blocked_by: Vec::new(),
                metadata: Default::default(),
            }],
            "a second attachment sees the same list"
        );
    }

    /// Another plugin's state in the same journal is not this one's.
    #[tokio::test]
    async fn only_this_plugin_s_kind_is_the_list() {
        let journals = Journals::new();
        let session = journals.session();
        let host = journals.handle();
        host.extend(&session, "bingo.rooms", "members", json!([]))
            .await
            .expect("published");
        assert!(read(&host, &session).await.expect("no tasks").is_empty());
    }
}

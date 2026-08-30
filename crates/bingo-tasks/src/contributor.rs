//! The reminder in the system prompt: what is still to do, recomputed every
//! request from the journal. Late among the system blocks — the kernel's own
//! instructions frame the work, this is the state of it — and never cached,
//! because a task the model just finished must not be listed as open on the
//! next round.

use async_trait::async_trait;
use bingo_sdk::{
    ContextContributor, ContextError, ContextPiece, ContextQuery, Placement, SystemBlock,
};

use crate::{journal, render};

/// After everything the kernel and the other plugins put in the prompt.
const ORDER: i32 = 900;

/// Lists the session's open tasks, or contributes nothing at all.
#[derive(Debug, Default, Clone, Copy)]
pub struct TasksContributor;

#[async_trait]
impl ContextContributor for TasksContributor {
    fn id(&self) -> &str {
        "tasks"
    }

    fn placement(&self) -> Placement {
        Placement::System { order: ORDER }
    }

    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        let tasks = journal::read(query.host, &query.session.id)
            .await
            .map_err(|e| ContextError(e.message))?;
        Ok(render::reminder(&tasks)
            .map(|text| ContextPiece::System(SystemBlock { text, cache: false }))
            .into_iter()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create::TaskCreateTool;
    use crate::tests::{Asked, Journals, tool_context};
    use crate::update::TaskUpdateTool;
    use bingo_sdk::{SessionId, Tool};
    use serde_json::json;

    async fn pieces(journals: &Journals, session: &SessionId) -> Vec<ContextPiece> {
        let asked = Asked::new(session, journals);
        TasksContributor
            .contribute(asked.query())
            .await
            .expect("tasks never fail a turn")
    }

    fn text(pieces: &[ContextPiece]) -> String {
        match &pieces[0] {
            ContextPiece::System(block) => block.text.clone(),
            ContextPiece::User { .. } => panic!("a reminder is a system block"),
        }
    }

    #[tokio::test]
    async fn the_open_tasks_reach_the_prompt() {
        let journals = Journals::new();
        let session = journals.session();
        let cx = tool_context(&session, &journals);
        TaskCreateTool
            .call(json!({"subject": "write the plan"}), &cx)
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

        let pieces = pieces(&journals, &session).await;
        assert_eq!(pieces.len(), 1);
        assert_eq!(
            text(&pieces),
            "# Tasks\n- #1 [in_progress] write the plan\n- #2 [pending] ship it"
        );
        let ContextPiece::System(block) = &pieces[0] else {
            panic!("a reminder is a system block");
        };
        assert!(
            !block.cache,
            "a list that changes within the turn is not a cache prefix"
        );
    }

    #[tokio::test]
    async fn a_session_with_no_tasks_adds_nothing_to_the_prompt() {
        let journals = Journals::new();
        let session = journals.session();
        assert!(pieces(&journals, &session).await.is_empty());
    }

    #[tokio::test]
    async fn a_list_that_is_all_done_adds_nothing_either() {
        let journals = Journals::new();
        let session = journals.session();
        let cx = tool_context(&session, &journals);
        TaskCreateTool
            .call(json!({"subject": "write the plan"}), &cx)
            .await
            .expect("a task");
        TaskUpdateTool
            .call(json!({"id": 1, "status": "completed"}), &cx)
            .await
            .expect("an update");
        assert!(pieces(&journals, &session).await.is_empty());
    }

    #[tokio::test]
    async fn a_host_that_cannot_be_read_is_a_notice_not_a_dead_turn() {
        let journals = Journals::new();
        let asked = Asked::new(&SessionId::from_raw("ses_gone"), &journals);
        assert!(TasksContributor.contribute(asked.query()).await.is_err());
    }

    #[test]
    fn it_comes_after_the_kernel_s_own_blocks_and_is_never_cached() {
        assert_eq!(TasksContributor.id(), "tasks");
        assert_eq!(
            TasksContributor.placement(),
            Placement::System { order: 900 }
        );
    }
}

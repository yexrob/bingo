//! Open task state, recomputed from the journal and appended when it changes.

use async_trait::async_trait;
use bingo_sdk::{ContextContributor, ContextError, ContextPiece, ContextQuery, Placement};

use crate::{journal, render};

/// Lists the session's open tasks, including an explicit cleared state.
#[derive(Debug, Default, Clone, Copy)]
pub struct TasksContributor;

#[async_trait]
impl ContextContributor for TasksContributor {
    fn id(&self) -> &str {
        "tasks"
    }

    fn placement(&self) -> Placement {
        Placement::RoundStart
    }

    async fn contribute(&self, query: ContextQuery<'_>) -> Result<Vec<ContextPiece>, ContextError> {
        let tasks = journal::read(query.host, &query.session.id)
            .await
            .map_err(|e| ContextError(e.message))?;
        let text = render::reminder(&tasks).unwrap_or_else(|| "# Tasks\nNo open tasks.".into());
        Ok(ContextPiece::snapshot(self.id(), text, query.items)
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
            ContextPiece::System(_) => panic!("a reminder is an appended user snapshot"),
            ContextPiece::User { parts, .. } => parts
                .iter()
                .filter_map(|part| match part {
                    bingo_sdk::ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
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
        assert!(matches!(&pieces[0], ContextPiece::User { .. }));
    }

    fn recorded(pieces: &[ContextPiece]) -> bingo_sdk::Item {
        let ContextPiece::User { parts, .. } = &pieces[0] else {
            panic!("tasks are appended user snapshots");
        };
        bingo_sdk::Item {
            id: bingo_sdk::ItemId::mint(),
            turn: None,
            round: 0,
            status: bingo_sdk::ItemStatus::Completed,
            started_at: jiff::Timestamp::UNIX_EPOCH,
            completed_at: None,
            intent: None,
            body: bingo_sdk::ItemBody::User {
                parts: parts.clone(),
                origin: bingo_sdk::Origin::surface("contributor:tasks"),
            },
            meta: Default::default(),
        }
    }

    async fn visible(
        journals: &Journals,
        session: &SessionId,
        items: &[bingo_sdk::Item],
    ) -> Vec<ContextPiece> {
        let asked = Asked::new(session, journals);
        let mut query = asked.query();
        query.items = items;
        TasksContributor
            .contribute(query)
            .await
            .expect("task context")
    }

    #[tokio::test]
    async fn task_changes_append_and_completion_clears_the_previous_snapshot() {
        let journals = Journals::new();
        let session = journals.session();
        let cx = tool_context(&session, &journals);
        TaskCreateTool
            .call(json!({"subject": "write the plan"}), &cx)
            .await
            .expect("a task");
        let first = visible(&journals, &session, &[]).await;
        let mut items = vec![recorded(&first)];
        assert!(visible(&journals, &session, &items).await.is_empty());

        TaskUpdateTool
            .call(json!({"id": 1, "status": "in_progress"}), &cx)
            .await
            .expect("an update");
        let changed = visible(&journals, &session, &items).await;
        assert_eq!(text(&changed), "# Tasks\n- #1 [in_progress] write the plan");
        items.push(recorded(&changed));
        assert!(visible(&journals, &session, &items).await.is_empty());

        TaskUpdateTool
            .call(json!({"id": 1, "status": "completed"}), &cx)
            .await
            .expect("an update");
        let cleared = visible(&journals, &session, &items).await;
        assert_eq!(text(&cleared), "# Tasks\nNo open tasks.");
        items.push(recorded(&cleared));
        assert!(visible(&journals, &session, &items).await.is_empty());
        // Removing the visible snapshot, as compaction can, restores current state.
        assert_eq!(
            text(&visible(&journals, &session, &[]).await),
            text(&cleared)
        );
    }

    #[tokio::test]
    async fn a_session_with_no_tasks_says_there_are_no_open_tasks() {
        let journals = Journals::new();
        let session = journals.session();
        assert_eq!(
            text(&pieces(&journals, &session).await),
            "# Tasks\nNo open tasks."
        );
    }

    #[tokio::test]
    async fn a_list_that_is_all_done_says_there_are_no_open_tasks() {
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
        assert_eq!(
            text(&pieces(&journals, &session).await),
            "# Tasks\nNo open tasks."
        );
    }

    #[tokio::test]
    async fn a_host_that_cannot_be_read_is_a_notice_not_a_dead_turn() {
        let journals = Journals::new();
        let asked = Asked::new(&SessionId::from_raw("ses_gone"), &journals);
        assert!(TasksContributor.contribute(asked.query()).await.is_err());
    }

    #[test]
    fn it_refreshes_at_the_start_of_each_round() {
        assert_eq!(TasksContributor.id(), "tasks");
        assert_eq!(TasksContributor.placement(), Placement::RoundStart);
    }
}

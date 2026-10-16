use bingo_sdk::*;
use std::collections::BTreeSet;

pub struct Refresh(pub crate::Shared);
#[async_trait::async_trait]
impl Hook for Refresh {
    fn id(&self) -> &str {
        "crew.refresh"
    }
    fn matcher(&self) -> HookMatcher {
        HookMatcher {
            points: vec![HookPoint::Event],
            tool: None,
        }
    }
    async fn on_event(&self, frame: &Frame, cx: &HookContext) {
        let sessions = self
            .0
            .lock()
            .map(|mut panels| {
                if matches!(frame.event, Event::SessionClosed { .. }) {
                    panels.remove(&frame.session);
                }
                panels
                    .iter()
                    .filter(|(_, panel)| relevant(panel, frame))
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for session in sessions {
            let _ = cx
                .host
                .invalidate(&session, vec![crate::REQUEST.into()])
                .await;
        }
    }
}
pub(crate) fn relevant(panel: &crate::state::Panel, frame: &Frame) -> bool {
    match &frame.event {
        Event::SessionUpdated { summary } => {
            panel.watching.contains(&summary.id)
                || summary
                    .parent
                    .as_ref()
                    .is_some_and(|p| panel.watching.contains(&p.session))
        }
        Event::SessionClosed { .. } => panel.watching.contains(&frame.session),
        Event::Extension { plugin, .. } => {
            panel.watching.contains(&frame.session)
                && matches!(plugin.as_str(), "bingo.tasks" | "bingo.rooms")
        }
        Event::CatalogChanged { kind } => kind == "models",
        _ => false,
    }
}
// These ids describe subscriptions, not a second roster or a cache of its facts.
pub fn dependencies(owner: &SessionId, rows: &crate::state::Rows) -> BTreeSet<SessionId> {
    let mut ids = BTreeSet::from([owner.clone()]);
    for row in rows.agents.iter().flatten() {
        ids.insert(row.summary.id.clone());
        if let Some(parent) = &row.summary.parent {
            ids.insert(parent.session.clone());
        }
    }
    for row in rows.rooms.iter().flatten() {
        ids.insert(row.id.clone());
    }
    for row in rows.team.iter().flatten() {
        if let Some(summary) = &row.summary {
            ids.insert(summary.id.clone());
        }
    }
    ids
}

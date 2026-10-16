//! Client actions returned in command receipts.
use super::*;
/// What a command's receipt asks of the surface: an action, a message, a view.
pub(super) fn receipt(run: &mut Run, result: &Value) {
    if let Ok(action) = serde_json::from_value::<bingo_sdk::Action>(result["surface"].clone()) {
        // ack calls receipt only for this client's own intent. Publications and
        // other clients' receipts cannot navigate this surface.
        if matches!(action.name.as_str(), "Pane" | "View")
            && let Some(id) = action.args["view"].as_str()
        {
            run.show(SessionId::from_raw(id));
        }
        sessions::surface_action(run, &action);
        self::action(run, action);
    }
    // Acknowledgements refresh the palette alongside asynchronous changes.
    catalogues::fetch(run, "plugins", CatalogKind::Plugins);
    // A recorded receipt is already a line in the transcript.
    if result.get("surface").is_none()
        && result.get("item").is_none()
        && let Some(message) = result.get("message").and_then(Value::as_str)
        && (!message.starts_with("permission mode: ") || message.contains('\n'))
    {
        run.ui
            .notify(Level::Info, message.to_string(), Instant::now());
    }
    if let Some(view) = result
        .get("view")
        .and_then(|v| serde_json::from_value::<View>(v.clone()).ok())
    {
        run.ui.block = Some(view);
    }
}

pub(super) fn action(run: &mut Run, action: bingo_sdk::Action) {
    let now = run.detection.now();
    match action.name.as_str() {
        "Browse" => crate::browse::open(
            &mut run.ui,
            &run.session.tree,
            None,
            action.args["confirm"].as_bool().unwrap_or(false),
            now,
        ),
        "Composer" => {
            if let Ok(fill) = serde_json::from_value(action.args) {
                crate::composer::fill::fill(&mut run.ui, fill, now);
            }
        }
        "Pager" => page(run, &action.args, now),
        "Pane" => focus(run, &action.args),
        "Plugins" => plugins(run, &action.args, now),
        _ => palette::surface_action(run, action),
    }
}

fn page(run: &mut Run, args: &Value, now: crate::clock::Now) {
    if let Ok(page) = serde_json::from_value(args.clone()) {
        crate::pager::page::open(&mut run.ui, &run.session.tree, page, now);
    }
}
/// `/plugins` answers with the panel to open and the group it opens on.
fn plugins(run: &mut Run, args: &Value, now: crate::clock::Now) {
    if let Ok(first) = serde_json::from_value(args["open"].clone()) {
        crate::plugins::open(&mut run.ui, first, now);
    }
}
fn focus(run: &mut Run, args: &Value) {
    if run.session.tree.open_interaction().is_some() {
        return;
    }
    let (Some(plugin), Some(id)) = (args["plugin"].as_str(), args["id"].as_str()) else {
        return;
    };
    if let Some(pane) = crate::pane::placed(run.session.tree.viewed())
        .into_iter()
        .find(|pane| pane.plugin == plugin && pane.id == id && pane.asked)
    {
        run.ui.focus = crate::focus::Focus::Pane(pane.key);
        run.ui.sync_panes(run.session.tree.viewed());
    }
}

pub(super) fn restore_prompt(run: &mut Run, text: Option<String>) {
    if let Some(text) = text {
        let now = run.detection.now();
        crate::composer::fill::fill(&mut run.ui, crate::composer::fill::Fill { text }, now);
    }
}

#[derive(Default)]
pub(super) struct Prompts(
    std::collections::BTreeMap<(SessionId, bingo_sdk::TurnId), (IntentId, String)>,
);
impl Prompts {
    pub fn cancel(&mut self, intent: &IntentId) {
        self.0.retain(|_, (pending, _)| pending != intent);
    }
}
pub(super) fn capture(run: &mut Run, intent: &IntentId, line: Option<&str>) {
    let Some(("rewind", args)) = line.and_then(crate::commands::split) else {
        return;
    };
    let mut words = args.split_whitespace();
    let Some(target) = words.next() else {
        return;
    };
    if words.next() == Some("files") {
        return;
    }
    let transcript = &run.session.tree.viewed().transcript;
    if let Some(turn) = bingo_sdk::transcript::turns(transcript)
        .into_iter()
        .find(|turn| turn.id.as_str() == target)
        && let Some(text) = turn.asked(transcript).map(str::to_owned)
    {
        run.rewind_prompts.0.insert(
            (run.session.tree.view().clone(), turn.id),
            (intent.clone(), text),
        );
    }
}
pub(super) fn undone(run: &mut Run, frame: &bingo_sdk::Frame) -> Option<String> {
    let Event::Rewound { to_turn, .. } = &frame.event else {
        return None;
    };
    let (_, text) = run
        .rewind_prompts
        .0
        .remove(&(frame.session.clone(), to_turn.clone()))?;
    (&frame.session == run.session.tree.view()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        doubles::{Recorder, TestSession},
        test_support::*,
    };
    use std::sync::Arc;
    #[tokio::test]
    async fn a_browse_action_preserves_the_requested_rewind_sheet() {
        let now = still(scene().1);
        let mut run = super::super::tests::idle(now.instant);
        for confirm in [false, true] {
            action(
                &mut run,
                bingo_sdk::Action {
                    name: "Browse".into(),
                    args: serde_json::json!({"confirm":confirm}),
                },
            );
            assert!(
                matches!(run.ui.focus.open(), crate::ui::Open::Browse(open) if open.confirm == confirm)
            );
        }
    }

    #[tokio::test]
    async fn changing_mode_does_not_leave_a_stale_notice() {
        let now = still(scene().1);
        let session = Arc::new(TestSession::default());
        let mut run = super::super::tests::idle_in(state(), session, now.instant);
        for mode in ["bypass permissions", "accept edits"] {
            receipt(
                &mut run,
                &serde_json::json!({"message":format!("permission mode: {mode}")}),
            );
            assert!(run.ui.notice().is_none());
        }
        receipt(&mut run, &serde_json::json!({"message":"saved"}));
        assert_eq!(run.ui.notice().unwrap().text, "saved");
    }

    #[tokio::test]
    async fn only_own_rewind_returns_the_full_prompt_and_keeps_a_draft() {
        for (own, draft, queued) in [
            (true, "", false),
            (true, "draft", false),
            (false, "", false),
            (true, "", true),
        ] {
            let now = still(scene().1);
            let mut state = state();
            state.transcript = vec![user("i1", "first\nsecond")].into_iter().collect();
            let session = Arc::new(TestSession::default());
            let mut run = super::super::tests::idle_in(state, session, now.instant);
            run.ui.composer.set(draft);
            if own {
                let intent = run.mint(Some("/rewind trn_1 conversation".into()));
                if queued {
                    run.ack(&intent, &IntentOutcome::Queued { position: 1 });
                }
                run.ack(
                    &intent,
                    &IntentOutcome::Applied {
                        result: serde_json::json!({}),
                    },
                );
            }
            let event = frame(
                1,
                Event::Rewound {
                    generation: 1,
                    to_turn: bingo_sdk::TurnId::from_raw("trn_1"),
                    dropped: vec![bingo_sdk::ItemId::from_raw("i1")],
                    files_restored: vec![],
                },
            );
            let mut screen = Recorder::default();
            run.frame(&event, &mut screen).unwrap();
            assert_eq!(
                run.ui.composer.text(),
                if own && draft.is_empty() {
                    "first\nsecond"
                } else {
                    draft
                }
            );
        }
    }
}

#[cfg(test)]
mod pane_receipt_tests {
    use super::*;
    use crate::test_support::*;
    #[tokio::test]
    async fn a_generic_view_action_keeps_the_root_and_changes_the_view() {
        let now = still(scene().1);
        let session = std::sync::Arc::new(crate::doubles::TestSession::default());
        let mut run = super::super::tests::idle_in(state(), session.clone(), now.instant);
        let root = run.session.tree.root_id().clone();
        let child = SessionId::from_raw("ses_child");
        run.session
            .handles
            .insert(child.clone(), bingo_sdk::SessionHandle(session));
        let action = bingo_sdk::Action {
            name: "View".into(),
            args: serde_json::json!({"view":child}),
        };
        super::action(&mut run, action.clone());
        assert_eq!(run.session.tree.view(), &root);
        let outcome = IntentOutcome::Applied {
            result: serde_json::json!({"surface": action}),
        };
        run.ack(&IntentId::mint(), &outcome);
        assert_eq!(run.session.tree.view(), &root);
        let intent = run.mint(None);
        run.ack(&intent, &outcome);
        assert_eq!(run.session.tree.root_id(), &root);
        assert_eq!(run.session.tree.view(), &child);
    }
}

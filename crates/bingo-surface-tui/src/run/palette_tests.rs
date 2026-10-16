use super::*;
use crate::{doubles::TestSession, test_support::*};
use bingo_sdk::{Action, Catalog, CatalogEntry, Origin, component::PaletteOpen};
use crossterm::event::KeyCode;
use serde_json::json;
use std::sync::Arc;

fn pull(run: &mut Run, now: crate::clock::Now) {
    draw_tree(80, 24, &run.session.tree, &mut run.ui, now);
    let key = run
        .ui
        .shown
        .pulls
        .iter()
        .find(|key| key.request.component == bingo_sdk::render::Component::Palette)
        .unwrap()
        .clone();
    let view = View::Box {
        children: vec![View::text("palette")],
        layout: Box::new(bingo_sdk::view::BoxStyle {
            on_key: Some(Action {
                name: "palette:ui".into(),
                args: json!({"query":key.request.props["query"]}),
            }),
            ..Default::default()
        }),
    };
    run.ui.placed.pulled.request(&key);
    run.reply(
        Reply::Rendered(run.ui.placed.pulled.epoch(), key, Box::new(view)),
        &mut None,
    );
}

#[tokio::test]
async fn rapid_text_then_enter_waits_for_the_matching_palette_frame() {
    let now = still(scene().1);
    let mut state = state();
    state.config.kernel["render"] = json!({"Palette":[{}]});
    let session = Arc::new(TestSession::default());
    let mut run = tests::idle_in(state, session.clone(), now.instant);
    crate::palette::open(&mut run.ui, PaletteOpen::Commands, now);
    for key in [typed('m'), typed('o'), key(KeyCode::Enter)] {
        run.terminal_event(Term::Key(key));
    }
    assert!(session.submitted().is_empty());
    assert_eq!(run.ui.composer.text(), "/mo");
    pull(&mut run, now);
    let submissions = session.submissions();
    assert_eq!(submissions.len(), 1);
    let (_, Input::Action { action, .. }) = &submissions[0] else {
        panic!("list action")
    };
    assert_eq!(action.args["query"], "/mo");
    assert_eq!(action.args["value"]["key"], "enter");
    assert_eq!(run.ui.composer.text(), "/mo");
}

#[test]
fn a_session_list_reply_belongs_only_to_its_opening() {
    let now = still(scene().1);
    let mut run = tests::idle(now.instant);
    crate::palette::open(&mut run.ui, PaletteOpen::Sessions, now);
    let old = crate::palette::request(&run.ui).unwrap().to_owned();
    crate::palette::close(&mut run.ui, now);
    crate::palette::open(&mut run.ui, PaletteOpen::Sessions, now);
    palette::fill_palette(&mut run, &old, vec![summary()]);
    let Open::Palette(open) = run.ui.focus.open() else {
        panic!("palette")
    };
    assert!(open.sessions.is_empty());
    let current = open.id.clone();
    palette::fill_palette(&mut run, &current, vec![summary()]);
    let Open::Palette(open) = run.ui.focus.open() else {
        panic!("palette")
    };
    assert_eq!(open.sessions.len(), 1);
}

#[tokio::test]
async fn the_top_palette_owns_keys_when_an_ask_arrives_after_idle() {
    let now = still(scene().1);
    for first in [PaletteOpen::Commands, PaletteOpen::Keys] {
        for ask in [question(false, true), permission(None, None)] {
            for pressed in [typed('x'), key(KeyCode::Enter), key(KeyCode::Esc)] {
                let mut state = state();
                state.config.kernel["render"] = json!({"Palette":[{}]});
                let session = Arc::new(TestSession::default());
                let mut run = tests::idle_in(state, session.clone(), now.instant);
                crate::palette::open(&mut run.ui, first, now);
                run.ui.last_typed = Some(now.wall - std::time::Duration::from_secs(2));
                run.session
                    .tree
                    .fold
                    .apply(&child_frame(1, announced("asking child")));
                run.session
                    .tree
                    .fold
                    .apply(&child_frame(2, opened(ask.clone())));
                interactions::refocus_at(&mut run, now.wall);
                assert!(
                    run.ui.dialog.aside,
                    "the palette keeps the arriving ask aside"
                );
                pull(&mut run, now);
                let id = crate::palette::request(&run.ui).unwrap().to_owned();
                run.terminal_event(Term::Key(pressed));
                if pressed.code == KeyCode::Char('x') {
                    assert!(session.submitted().is_empty(), "editing stays local");
                } else {
                    let submissions = session.submissions();
                    let (intent, Input::Action { .. }) = &submissions[0] else {
                        panic!("palette action")
                    };
                    let result = json!({"surface":{"name":"surface:palette","args":{"handler":id,"value":{"close":true}}}});
                    run.ack(intent, &IntentOutcome::Applied { result });
                }
                assert!(run.session.tree.open_interaction().is_some());
                assert_eq!(
                    session.submissions().len(),
                    usize::from(pressed.code != KeyCode::Char('x')),
                    "no answer or interrupt escaped"
                );
                if pressed.code == KeyCode::Char('x') {
                    assert!(run.ui.composer.text().ends_with('x'));
                    assert!(run.ui.dialog.aside);
                } else {
                    assert!(!crate::palette::active(&run.ui));
                    assert!(!run.ui.dialog.aside, "closing returns focus to the ask");
                }
            }
        }
    }
}

#[tokio::test]
async fn an_unmatched_palette_replacement_never_queues_the_keyboard() {
    let now = still(scene().1);
    let mut state = state();
    state.config.kernel["render"] = json!({"Palette":[{"props":{"open":"sessions"}}]});
    let mut run = tests::idle_in(state, Arc::new(TestSession::default()), now.instant);
    crate::palette::open(&mut run.ui, PaletteOpen::Commands, now);
    draw_tree(80, 24, &run.session.tree, &mut run.ui, now);
    run.terminal_event(Term::Key(typed('m')));
    assert_eq!(run.ui.composer.text(), "/m");
    run.terminal_event(Term::Key(key(KeyCode::Esc)));
    assert!(!crate::palette::active(&run.ui));
    assert!(run.typeahead.is_empty());
}

#[test]
fn the_resume_picker_offers_twenty_newest_roots_and_never_a_child_or_a_room() {
    let now = still(scene().1);
    let mut run = tests::idle(now.instant);
    crate::palette::open(&mut run.ui, PaletteOpen::Sessions, now);
    let request = crate::palette::request(&run.ui).unwrap().to_owned();
    let mut sessions = vec![child_summary("reviewer"), log_summary("#design")];
    sessions.extend((0..25).map(|at| SessionSummary {
        id: SessionId::from_raw(format!("ses_root{at}")),
        updated_at: now.wall + std::time::Duration::from_secs(at),
        ..summary()
    }));
    palette::fill_palette(&mut run, &request, sessions);
    let Open::Palette(open) = run.ui.focus.open() else {
        panic!("palette")
    };
    assert_eq!(open.sessions.len(), 20);
    assert!(open.sessions.iter().all(|session| session.parent.is_none()));
    assert_eq!(open.sessions[0].id.as_str(), "ses_root24");
    assert_eq!(open.sessions[19].id.as_str(), "ses_root5");
}

#[tokio::test]
async fn stored_sessions_use_the_roots_workspace_when_a_child_is_in_view() {
    let now = still(scene().1);
    let mut run = tests::idle(now.instant);
    let mut child = child_summary("reviewer");
    child.cwd = "/another/workspace".into();
    run.session
        .tree
        .fold
        .apply(&child_frame(1, Event::SessionUpdated { summary: child }));
    run.session.tree.show(&child_id());
    let host = bingo_sdk::testing::TestHost::default();
    let cwd = run.session.tree.root().summary.cwd.clone();
    let observed = Arc::new(std::sync::Mutex::new(None));
    let capture = observed.clone();
    host.on_sessions(move |filter| {
        *capture.lock().unwrap() = filter.cwd;
        async { Ok(vec![]) }
    });
    run.host = host.handle();
    let (sender, mut replies) = tokio::sync::mpsc::channel(1);
    run.replies = sender;
    crate::palette::open(&mut run.ui, PaletteOpen::Sessions, now);
    palette::list_stored(&mut run);
    assert!(matches!(replies.recv().await, Some(Reply::Stored(_, _))));
    assert_eq!(
        *observed.lock().unwrap(),
        Some(std::path::PathBuf::from(cwd))
    );
}

#[test]
fn the_palette_receives_drawing_facts_without_a_copy_of_session_state() {
    let now = still(scene().1);
    let mut state = state();
    state.config.kernel["render"] = json!({"Palette":[{}]});
    let mut run = tests::idle_in(state, Arc::new(TestSession::default()), now.instant);
    crate::palette::open(&mut run.ui, PaletteOpen::Sessions, now);
    draw_tree(80, 24, &run.session.tree, &mut run.ui, now);
    let props = &run
        .ui
        .shown
        .pulls
        .iter()
        .find(|key| key.request.component == bingo_sdk::render::Component::Palette)
        .unwrap()
        .request
        .props;
    for field in ["summary", "config", "signals", "context"] {
        assert!(
            props.get(field).is_none(),
            "{field} should not duplicate the fold"
        );
    }
    assert_eq!(
        props["conversation"][0]["id"],
        run.session.tree.root_id().as_str()
    );
}

/// Every press carries the props, so they hold only the catalogue the
/// query's command completes from, and none before its arguments.
#[test]
fn a_press_carries_only_the_catalogue_its_command_completes_from() {
    let now = still(scene().1);
    let mut state = state();
    state.config.kernel["render"] = json!({"Palette":[{}]});
    let mut run = tests::idle_in(state, Arc::new(TestSession::default()), now.instant);
    run.ui.catalogs.commands = crate::commands::entries(vec![
        serde_json::from_value(json!({
            "name":"model","hint":"","family":"session",
            "args":{"kind":"options","args":{"kind":"catalog","source":"models"},"flags":[]}
        }))
        .unwrap(),
    ]);
    for kind in ["models", "providers"] {
        run.ui.catalogs.values.insert(
            kind.into(),
            vec![CatalogEntry {
                id: kind.into(),
                label: kind.into(),
                meta: json!({}),
            }],
        );
    }
    crate::palette::open(&mut run.ui, PaletteOpen::Commands, now);
    let catalogs = |run: &mut Run, query: &str| {
        run.ui.composer.set(query);
        draw_tree(80, 24, &run.session.tree, &mut run.ui, now);
        let key = run
            .ui
            .shown
            .pulls
            .iter()
            .find(|key| key.request.component == bingo_sdk::render::Component::Palette)
            .unwrap();
        key.request.props["catalogs"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    };
    assert!(catalogs(&mut run, "/mo").is_empty());
    assert_eq!(catalogs(&mut run, "/model "), ["models"]);
    assert!(catalogs(&mut run, "/think ").is_empty());
}

/// A held arrow: each press waits on the palette's redraw, so its
/// invalidation asks for the drawing at once rather than at the next paint.
#[tokio::test]
async fn a_drawing_a_press_waits_on_is_pulled_when_invalidated() {
    for pressed in [false, true] {
        let now = still(scene().1);
        let mut state = state();
        state.config.kernel["render"] = json!({"Palette":[{}]});
        let mut run = tests::idle_in(state, Arc::new(TestSession::default()), now.instant);
        crate::palette::open(&mut run.ui, PaletteOpen::Commands, now);
        pull(&mut run, now);
        if pressed {
            run.terminal_event(Term::Key(key(KeyCode::Down)));
            assert!(!run.site_keys.is_empty(), "the press waits on the site");
        }
        let id = crate::palette::request(&run.ui).unwrap().to_owned();
        let invalidated = frame(1, Event::Invalidated { requests: vec![id] });
        run.frame(&invalidated, &mut Recorder::default()).unwrap();
        draw_tree(80, 24, &run.session.tree, &mut run.ui, now);
        let key = run
            .ui
            .shown
            .pulls
            .iter()
            .find(|key| key.request.component == bingo_sdk::render::Component::Palette)
            .unwrap()
            .clone();
        assert_eq!(
            !run.ui.placed.pulled.request(&key),
            pressed,
            "only a waiting press pulls before the paint"
        );
    }
}

#[tokio::test]
async fn acknowledgements_refetch_only_plugins_and_changes_fetch_only_their_kind() {
    let now = still(scene().1);
    let mut run = tests::idle(now.instant);
    let senders = run.replies.strong_count();
    surface::receipt(&mut run, &json!({"message":"renamed"}));
    assert_eq!(
        run.replies.strong_count(),
        senders + 1,
        "only Plugins is refreshed"
    );
    catalogues::changed(&mut run, CatalogKind::Commands);
    assert_eq!(
        run.replies.strong_count(),
        senders + 2,
        "only the changed catalogue is read"
    );
}

#[tokio::test]
async fn an_applied_plugins_reload_refreshes_the_waiting_count() {
    let home = tempfile::tempdir().unwrap();
    let now = still(scene().1);
    let session = Arc::new(TestSession::default());
    let mut run = tests::idle_in(state(), session.clone(), now.instant);
    let host = bingo_sdk::testing::TestHost::default();
    host.on_catalog(|kind| async move {
        Ok(Catalog {
            kind,
            entries: vec![CatalogEntry {
                id: "repo".into(),
                label: "repo".into(),
                meta: json!({"state":{"kind":"waiting"}}),
            }],
        })
    });
    run.host = host.handle();
    let (sender, mut replies) = tokio::sync::mpsc::channel(8);
    run.replies = sender;
    run.data_dir = home.path().into();
    run.submit(Input::text("/plugins reload", Origin::surface("tui")));
    let intent = session.submissions()[0].0.clone();
    run.ack(&intent, &IntentOutcome::Applied { result: json!({}) });
    tokio::task::yield_now().await;
    let reply = replies
        .try_recv()
        .expect("reload acknowledgement refreshes Plugins");
    run.reply(reply, &mut None);
    assert!(matches!(
        host.calls().as_slice(),
        [bingo_sdk::testing::HostCall::Catalog {
            kind: CatalogKind::Plugins
        }]
    ));
    let screen = draw_tree(120, 40, &run.session.tree, &mut run.ui, now);
    assert!(screen.contains("1 from this repo waiting"), "{screen}");
}

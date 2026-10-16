use super::*;
use crate::{doubles::TestSession, test_support::*};
use bingo_sdk::{Catalog, testing::TestHost};
use std::sync::Arc;

fn answer(kind: CatalogKind) -> Catalog {
    let entries = if kind == CatalogKind::Commands {
        vec![
            serde_json::from_value(serde_json::json!({"id":"review","label":"review","meta":{
                "name":"review","hint":"review","family":"terminal","args":{"kind":"none"},
                "instant":true,"opens":true,"keys":["ctrl+n"]
            }}))
            .unwrap(),
        ]
    } else {
        vec![]
    };
    Catalog { kind, entries }
}

#[tokio::test(start_paused = true)]
async fn the_first_key_sees_a_prompt_command_catalogue() {
    let mut run = super::super::tests::idle(scene().1.instant);
    let host = TestHost::default();
    host.on_catalog(|kind| async move { Ok(answer(kind)) });
    run.host = host.handle();
    fetch_all(&mut run).await;
    assert_eq!(
        crate::keys::commands::line(&run.ui, run.session.tree.viewed(), ctrl('n')).as_deref(),
        Some("/review")
    );
    assert!(!run.typeahead.catalogue_pending);
}

#[tokio::test(start_paused = true)]
async fn a_slow_catalogue_releases_first_paint_and_keeps_the_first_shortcut() {
    let session = Arc::new(TestSession::default());
    let mut run = super::super::tests::idle_in(state(), session.clone(), scene().1.instant);
    let host = TestHost::default();
    let release = Arc::new(tokio::sync::Notify::new());
    let gate = release.clone();
    host.on_catalog(move |kind| {
        let gate = gate.clone();
        async move {
            if kind == CatalogKind::Commands {
                gate.notified().await;
            }
            Ok(answer(kind))
        }
    });
    run.host = host.handle();
    let (tx, mut replies) = mpsc::channel(16);
    run.replies = tx;
    let start = tokio::time::Instant::now();
    fetch_all(&mut run).await;
    assert_eq!(start.elapsed(), INITIAL_WAIT);
    assert!(draw_tree(80, 24, &run.session.tree, &mut run.ui, scene().1).contains("ask anything"));
    run.terminal_event(Term::Key(typed('a')));
    assert_eq!(run.ui.composer.text(), "a");
    run.terminal_event(Term::Key(ctrl('n')));
    assert_eq!(run.typeahead.len(), 1);
    assert!(session.submitted().is_empty());
    release.notify_one();
    loop {
        let reply = replies.recv().await.unwrap();
        let ready = matches!(reply, Reply::Commands(..));
        run.reply(reply, &mut None);
        if ready {
            break;
        }
    }
    assert!(matches!(&session.submitted()[..], [Input::Text { text, .. }] if text == "/review"));
    assert!(run.typeahead.is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_catalogue_error_keeps_startup_and_typing_available() {
    let mut run = super::super::tests::idle(scene().1.instant);
    let host = TestHost::default();
    host.on_catalog(|_| async { Err(KernelError::new(bingo_sdk::ErrorCode::Internal, "offline")) });
    run.host = host.handle();
    fetch_all(&mut run).await;
    assert!(!run.typeahead.catalogue_pending);
    assert!(
        run.ui
            .notice()
            .iter()
            .any(|notice| notice.text.contains("offline"))
    );
    run.terminal_event(Term::Key(typed('a')));
    assert_eq!(run.ui.composer.text(), "a");
}

#[tokio::test(start_paused = true)]
async fn a_background_error_releases_held_input_and_allows_a_later_refresh() {
    let mut run = super::super::tests::idle(scene().1.instant);
    run.typeahead.catalogue_pending = true;
    run.terminal_event(Term::Key(ctrl('n')));
    run.terminal_event(Term::Key(typed('a')));
    run.reply(
        Reply::Commands(
            0,
            Err(KernelError::new(bingo_sdk::ErrorCode::Internal, "offline")),
        ),
        &mut None,
    );
    assert!(run.typeahead.is_empty());
    assert_eq!(run.ui.composer.text(), "a");
    run.reply(
        Reply::Commands(0, Ok(answer(CatalogKind::Commands).entries)),
        &mut None,
    );
    assert_eq!(
        crate::keys::commands::line(&run.ui, run.session.tree.viewed(), ctrl('n')).as_deref(),
        Some("/review")
    );
}

#[tokio::test(start_paused = true)]
async fn a_stalled_initial_catalogue_fails_at_its_overall_deadline() {
    let mut run = super::super::tests::idle(scene().1.instant);
    let host = TestHost::default();
    host.on_catalog(|kind| async move {
        if kind == CatalogKind::Commands {
            std::future::pending::<()>().await;
        }
        Ok(answer(kind))
    });
    run.host = host.handle();
    let (tx, mut replies) = mpsc::channel(16);
    run.replies = tx;
    let start = tokio::time::Instant::now();
    fetch_all(&mut run).await;
    run.terminal_event(Term::Key(ctrl('n')));
    run.terminal_event(Term::Key(typed('a')));
    assert_eq!(run.ui.composer.text(), "a");
    assert_eq!(run.typeahead.len(), 1);
    loop {
        let reply = replies.recv().await.unwrap();
        let ready = matches!(reply, Reply::Commands(..));
        run.reply(reply, &mut None);
        if ready {
            break;
        }
    }
    assert_eq!(start.elapsed(), COMMAND_WAIT);
    assert!(!run.typeahead.catalogue_pending);
    assert!(run.typeahead.is_empty());
    assert!(run.ui.notice().iter().any(|n| n.text.contains("timed out")));
}

#[tokio::test]
async fn only_catalogue_holds_let_plain_keys_and_exit_pass() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for key in [
        ctrl('n'),
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT),
        KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE),
    ] {
        let mut run = super::super::tests::idle(scene().1.instant);
        run.typeahead.catalogue_pending = true;
        run.terminal_event(Term::Key(key));
        run.terminal_event(Term::Key(typed('a')));
        assert_eq!(run.ui.composer.text(), "a");
        assert_eq!(run.typeahead.len(), 1);
        run.ui.composer.set("");
        run.terminal_event(Term::Key(ctrl('d')));
        assert!(run.exit.is_some());
        let mut run = super::super::tests::idle(scene().1.instant);
        run.typeahead.catalogue_pending = true;
        run.terminal_event(Term::Key(key));
        typeahead::command_sent(&mut run, IntentId::mint());
        run.terminal_event(Term::Key(typed('a')));
        assert_eq!(run.typeahead.len(), 2, "opening still preserves order");
    }
}

#[tokio::test]
async fn an_older_fetch_cannot_replace_a_newer_catalogue_or_report_failure() {
    let mut run = super::super::tests::idle(scene().1.instant);
    let host = TestHost::default();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    let entered = Arc::new(tokio::sync::Notify::new());
    let started = entered.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let gate = release.clone();
    host.on_catalog(move |kind| {
        let first = counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
        let gate = gate.clone();
        let started = started.clone();
        async move {
            if first {
                started.notify_one();
                gate.notified().await;
                Ok(Catalog {
                    kind,
                    entries: vec![],
                })
            } else {
                Ok(answer(kind))
            }
        }
    });
    run.host = host.handle();
    let (tx, mut replies) = mpsc::channel(16);
    run.replies = tx;
    run.typeahead.catalogue_pending = true;
    fetch_commands(&mut run);
    entered.notified().await;
    fetch_commands(&mut run);
    run.reply(replies.recv().await.unwrap(), &mut None);
    assert!(!run.typeahead.catalogue_pending);
    release.notify_one();
    run.reply(replies.recv().await.unwrap(), &mut None);
    run.reply(
        Reply::Commands(
            1,
            Err(KernelError::new(bingo_sdk::ErrorCode::Timeout, "old")),
        ),
        &mut None,
    );
    assert_eq!(run.command_fetch, 2);
    assert_eq!(
        crate::keys::commands::line(&run.ui, run.session.tree.viewed(), ctrl('n')).as_deref(),
        Some("/review")
    );
    assert!(run.ui.notice.is_none());
}

#[tokio::test]
async fn command_requests_follow_the_viewed_session() {
    let mut run = super::super::tests::idle(scene().1.instant);
    run.session.tree = folded_tree(vec![child_frame(1, announced("child"))]);
    let (tx, mut replies) = mpsc::channel(16);
    run.replies = tx;
    run.session.handles.insert(
        child_id(),
        bingo_sdk::SessionHandle(Arc::new(TestSession::default())),
    );
    let host = TestHost::default();
    host.on_catalog(|kind| async move { Ok(answer(kind)) });
    run.host = host.handle();
    fetch_all(&mut run).await;
    assert!(host.calls().iter().any(|call| matches!(call,
        bingo_sdk::testing::HostCall::CatalogFor { session, kind: CatalogKind::Commands }
        if session == run.session.tree.view()
    )));
    let previous = run.command_fetch;
    run.show(child_id());
    assert_eq!(run.command_fetch, previous + 1);
    assert!(run.ui.catalogs.commands.is_empty());
    assert!(run.typeahead.catalogue_pending);
    loop {
        let reply = replies.recv().await.unwrap();
        let ready = matches!(reply, Reply::Commands(..));
        run.reply(reply, &mut None);
        if ready {
            break;
        }
    }
    assert!(host.calls().iter().any(|call| matches!(call,
        bingo_sdk::testing::HostCall::CatalogFor { session, kind: CatalogKind::Commands }
        if session == &child_id()
    )));
}

#[tokio::test]
async fn a_disabled_panel_restores_its_folded_names_on_the_next_palette_open() {
    let now = still(scene().1);
    let mut state = state();
    state.config.kernel["render"] = serde_json::json!({"Palette":[{}]});
    let mut run =
        super::super::tests::idle_in(state, Arc::new(TestSession::default()), now.instant);
    let host = TestHost::default();
    let enabled = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let live = enabled.clone();
    host.on_catalog(move |kind| {
        let enabled = live.load(std::sync::atomic::Ordering::SeqCst);
        async move {
            let entries = if kind == CatalogKind::Commands {
                ["crew", "agents"].into_iter().filter(|name| enabled || *name != "crew")
                    .map(|name| bingo_sdk::CatalogEntry {
                        id: name.into(), label: name.into(),
                        meta: serde_json::json!({"name":name,"hint":"fixture","family":"session",
                            "args":{"kind":"none"},"foldedInto":if enabled && name == "agents" {Some("crew")} else {None}}),
                    }).collect()
            } else { vec![] };
            Ok(Catalog { kind, entries })
        }
    });
    run.host = host.handle();
    fetch_all(&mut run).await;
    run.ui.composer.set("/a");
    assert!(
        run.ui
            .suggestions("/tmp", &[])
            .iter()
            .all(|row| row.label != "/agents")
    );
    let fixture = crate::palette::tests::Fixture::new();
    crate::palette::open(
        &mut run.ui,
        bingo_sdk::component::PaletteOpen::Commands,
        now,
    );
    let shown = fixture
        .paint_tree(&mut run.ui, &run.session.tree, now, 108, 30)
        .await;
    assert!(shown.contains("/crew"), "{shown}");
    assert!(!shown.contains("/agents"), "{shown}");
    crate::palette::close(&mut run.ui, now);
    enabled.store(false, std::sync::atomic::Ordering::SeqCst);
    let (tx, mut replies) = mpsc::channel(16);
    run.replies = tx;
    changed(&mut run, CatalogKind::Commands);
    run.reply(replies.recv().await.unwrap(), &mut None);
    run.ui.composer.set("/a");
    assert!(
        run.ui
            .suggestions("/tmp", &[])
            .iter()
            .any(|row| row.label == "/agents")
    );
    crate::palette::open(
        &mut run.ui,
        bingo_sdk::component::PaletteOpen::Commands,
        now,
    );
    let shown = fixture
        .paint_tree(&mut run.ui, &run.session.tree, now, 108, 30)
        .await;
    assert!(shown.contains("/agents"), "{shown}");
    assert!(!shown.contains("/crew"), "{shown}");
}
